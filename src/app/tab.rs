use std::{
	collections::HashMap,
	io,
	os::unix::ffi::OsStrExt,
	path::{Path, PathBuf},
	sync::Arc,
	time::Duration,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edtui::EditorMode;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
	command::{CopyKind, DeleteMode},
	column_mode::ColumnMode,
	config::Config,
	core::{Filter, Node, Selection, Tree, Visual},
	dds::Body,
	event::Event,
	finder::Finder,
	fs::{Cha, Engine, FsChange, LocalEngine, SortBy, SortPolicy, format_size, unique_dest_avoiding},
	notice::NoticeLevel,
	preview::Preview,
	scheduler::FsScheduler,
	status::{StatusLine, StatusMode},
	watcher::Watcher,
};

use super::{
	input::{Completion, InputPurpose, InputSession},
	path_history::PathHistory,
	visible_projection::VisibleProjection,
};

/// One tab: its own tree, cursor, selection and background workers —
/// everything but whether the whole program should quit. The clipboard
/// lives on `App` instead, shared by every tab (yank in one, paste in
/// another). `id` is assigned once at creation and never reused or
/// renumbered, so that background events tagged with it
/// (`Loaded`/`Deleted`/`Changed`) keep routing to the right tab
/// even after some *other* tab closes and every tab after it would
/// otherwise shift position in `App::tabs`.
pub struct Tab {
	config: Arc<Config>,
	pub id: usize,
	pub tree: Tree,
	projection: VisibleProjection,
	pub cursor: usize,
	/// The tree view's scroll offset (index of its first visible row),
	/// persisted across frames so ratatui only nudges it when the cursor
	/// would otherwise leave the viewport, instead of recomputing it from
	/// scratch — which would re-track the cursor on every move.
	pub scroll: usize,
	pub column_mode: ColumnMode,
	pub sort_policy: SortPolicy,
	pub show_hidden: bool,
	path_history: PathHistory,
	pub preview: Preview,
	pub watcher: Watcher,
	pub fs_scheduler: FsScheduler,
	pending_listings: HashMap<PathBuf, PendingListing>,
	pub selection: Selection,
	pub visual: Option<Visual>,
	pub pending_delete: Option<(Vec<PathBuf>, DeleteMode)>,
	pub finder: Option<Finder>,
	pub filter: Option<Filter>,
	/// A one-off operational message (an invalid cd, a refused delete, …)
	/// waiting to be drained into `App`'s toast queue — this is an outbox,
	/// not something rendered from `Tab` directly. A directory listing
	/// failure is a different, persistent kind of problem and lives on the
	/// `Node` itself (`Node::load_error`) instead of here.
	pub(super) pending_notice: Option<(NoticeLevel, String)>,
	pending_reveal: Option<RevealState>,
	pub(super) input: Option<InputSession>,
	input_seq: u64,
	tx: UnboundedSender<Event>,
}

struct RevealState {
	target: PathBuf,
	refreshed_parent: bool,
}

enum PendingListing {
	Incremental { ticket: u64 },
	Buffered { ticket: u64, entries: Vec<(PathBuf, Cha)> },
}

/// What a completed listing needs done with it — the tree is already
/// up to date for `Incremental` (batches were applied as they arrived),
/// so it only needs the closing `finish_incremental_listing` pass, while
/// `Full` still has to be handed its entries via `apply_listing`.
enum ListingOutcome {
	Incremental,
	Full(Vec<(PathBuf, Cha)>),
}

impl PendingListing {
	/// Resolves a `done` listing for `ticket` against whatever pending
	/// record (if any) was tracking it. `None` means a newer request has
	/// already superseded this one, so the caller should leave the tree
	/// alone entirely rather than clobber it with a stale result.
	fn finish(pending: Option<Self>, ticket: u64) -> Option<ListingOutcome> {
		match pending {
			None => Some(ListingOutcome::Full(Vec::new())),
			Some(Self::Incremental { ticket: current }) if current == ticket => Some(ListingOutcome::Incremental),
			Some(Self::Buffered { ticket: current, entries }) if current == ticket => Some(ListingOutcome::Full(entries)),
			Some(_) => None,
		}
	}
}

impl Tab {
	#[cfg(test)]
	pub fn open(id: usize, path: PathBuf, tx: UnboundedSender<Event>) -> io::Result<Self> {
		Self::open_configured(id, path, tx, Arc::new(Config::default()))
	}

	pub fn open_configured(id: usize, path: PathBuf, tx: UnboundedSender<Event>, config: Arc<Config>) -> io::Result<Self> {
		let mut tree = Tree::open(path)?;
		let root_path = tree.root.path.clone();
		let needs_fetch = tree.mark_expanded(&root_path).unwrap_or(false);

		let watcher = Watcher::new(
			id,
			tx.clone(),
			Duration::from_millis(config.watcher.debounce_ms),
			Duration::from_millis(config.watcher.max_wait_ms),
			Duration::from_millis(config.watcher.poll_interval_ms),
		)?;
		watcher.watch(&root_path)?;

		let engine: Arc<dyn Engine> = Arc::new(LocalEngine);
		let mut fs_scheduler = FsScheduler::new(id, tx.clone(), engine);
		if needs_fetch {
			fs_scheduler.refresh(root_path.clone());
		}

		let show_hidden = config.mgr.show_hidden;
		let projection = VisibleProjection::new(&tree.root, None, show_hidden);
		let path_history = PathHistory::new(root_path.clone(), config.mgr.history_size);
		Ok(Self {
			config: config.clone(),
			id,
			tree,
			projection,
			cursor: 0,
			scroll: 0,
			column_mode: config.mgr.column_mode,
			sort_policy: config.mgr.sort,
			show_hidden,
			path_history,
			preview: Preview::configured(id, tx.clone(), config.preview.clone()),
			watcher,
			fs_scheduler,
			pending_listings: HashMap::new(),
			selection: Selection::default(),
			visual: None,
			pending_delete: None,
			finder: None,
			filter: None,
			pending_notice: None,
			pending_reveal: None,
			input: None,
			input_seq: 0,
			tx,
		})
	}

	/// The rows to draw: every row when no filter is active, otherwise only
	/// rows that match it or have a visible descendant that does — with the
	/// root kept regardless, so an empty result still shows where you are.
	#[cfg(test)]
	pub fn visible(&self) -> Vec<(usize, &Node)> {
		self.projection.range(&self.tree, 0..self.projection.len())
	}

	pub fn visible_len(&self) -> usize {
		self.projection.len()
	}

	pub fn visible_at(&self, target: usize) -> Option<(usize, &Node)> {
		self.projection.get(&self.tree, target)
	}

	pub fn visible_range(&self, range: std::ops::Range<usize>) -> Vec<(usize, &Node)> {
		self.projection.range(&self.tree, range)
	}

	fn visible_position(&self, path: &Path) -> Option<usize> {
		self.projection.position(path)
	}

	fn sync_projection(&mut self, path: &Path) {
		self.projection.sync_subtree(&self.tree.root, path, self.filter.as_ref(), self.show_hidden);
	}

	fn rebuild_projection(&mut self) {
		self.projection.rebuild(&self.tree.root, self.filter.as_ref(), self.show_hidden);
	}

	fn cancel_listing(&mut self, path: &Path) {
		if matches!(self.pending_listings.remove(path), Some(PendingListing::Incremental { .. })) {
			self.tree.discard_incremental_listing(path);
		}
	}

	pub fn move_cursor(&mut self, delta: isize) {
		let len = self.visible_len();
		if len == 0 {
			return;
		}
		let cursor = (self.cursor as isize + delta).clamp(0, len as isize - 1) as usize;
		if cursor != self.cursor {
			self.cursor = cursor;
			self.preview.target_changed();
		}
	}

	pub fn move_to_top(&mut self) {
		if self.cursor != 0 {
			self.cursor = 0;
			self.preview.target_changed();
		}
	}

	pub fn move_to_bottom(&mut self) {
		let cursor = self.visible_len().saturating_sub(1);
		if cursor != self.cursor {
			self.cursor = cursor;
			self.preview.target_changed();
		}
	}

	pub fn set_sort(&mut self, policy: SortPolicy) {
		let hovered = self.visible_at(self.cursor).map(|(_, node)| node.path.clone());
		self.sort_policy = policy;
		self.tree.sort(policy);
		self.rebuild_projection();
		match policy.by {
			SortBy::Modified => self.column_mode = ColumnMode::Modified,
			SortBy::Size => self.column_mode = ColumnMode::Size,
			SortBy::Name | SortBy::Extension => {}
		}
		if let Some(path) = hovered {
			self.select(&path);
		}
	}

	pub fn toggle_hidden(&mut self) {
		let hovered = self.visible_at(self.cursor).map(|(_, node)| node.path.clone());
		let old_cursor = self.cursor;
		self.show_hidden = !self.show_hidden;
		self.visual = None;
		self.rebuild_projection();

		let mut target = hovered;
		let mut cursor = None;
		while let Some(path) = target {
			if let Some(position) = self.visible_position(&path) {
				cursor = Some(position);
				break;
			}
			target = self.tree.parent_of(&path);
		}
		self.cursor = cursor.unwrap_or_else(|| old_cursor.min(self.visible_len().saturating_sub(1)));
		self.preview.target_changed();
	}

	/// Marks the directory open immediately (the triangle flips, the row
	/// stays put) and, if it's never been listed, kicks off a background
	/// read — the listing lands later as a `Loaded` event instead of
	/// blocking this call.
	pub fn expand_selected(&mut self) {
		let Some(path) = self.selected_dir() else {
			return;
		};
		let needs_fetch = self.tree.mark_expanded(&path).unwrap_or(false);
		self.sync_projection(&path);
		self.watcher.watch_async(path.clone());
		if needs_fetch {
			self.fs_scheduler.refresh(path);
		}
	}

	pub fn toggle_expand_selected(&mut self) {
		let Some((_, node)) = self.visible_at(self.cursor) else {
			return;
		};
		if !node.cha.is_dir {
			return;
		}
		if node.expanded {
			let path = node.path.clone();
			self.cancel_listing(&path);
			self.tree.collapse(&path);
			self.sync_projection(&path);
			self.watcher.unwatch(&path);
			self.fs_scheduler.forget(&path);
		} else {
			self.expand_selected();
		}
	}

	/// Collapses the selected directory; if the selection isn't an open
	/// directory (a file, or an already-collapsed one), collapses its parent
	/// and moves the cursor there instead — pressing "collapse" always does
	/// something visible, the way it does in most tree file managers.
	pub fn collapse_selected(&mut self) {
		let Some((_, node)) = self.visible_at(self.cursor) else {
			return;
		};
		let path = node.path.clone();

		let target = if node.cha.is_dir && node.expanded { path } else { self.tree.parent_of(&path).unwrap_or(path) };

		self.cancel_listing(&target);
		self.tree.collapse(&target);
		self.sync_projection(&target);
		self.watcher.unwatch(&target);
		self.fs_scheduler.forget(&target);
		self.select(&target);
	}

	pub fn collapse_subtree(&mut self) {
		let Some((_, node)) = self.visible_at(self.cursor) else { return };
		let path = node.path.clone();
		let target = if node.cha.is_dir { path } else { self.tree.parent_of(&path).unwrap_or(path) };
		for collapsed in self.tree.collapse_subtree(&target) {
			self.cancel_listing(&collapsed);
			self.watcher.unwatch(&collapsed);
			self.fs_scheduler.forget(&collapsed);
		}
		self.sync_projection(&target);
		self.select(&target);
		self.preview.target_changed();
	}

	pub fn collapse_all(&mut self) {
		let root = self.tree.root.path.clone();
		for collapsed in self.tree.collapse_all() {
			self.cancel_listing(&collapsed);
			self.watcher.unwatch(&collapsed);
			self.fs_scheduler.forget(&collapsed);
		}
		self.sync_projection(&root);
		self.select(&root);
		self.preview.target_changed();
	}

	pub fn toggle_selected(&mut self) {
		if let Some((_, node)) = self.visible_at(self.cursor) {
			self.selection.toggle(node.path.clone());
		}
		self.move_cursor(1);
	}

	pub fn enter_visual(&mut self, unset: bool) {
		self.visual = Some(Visual::new(self.cursor, unset));
	}

	/// Applies the pending visual range to the selection — adding every row
	/// in it if this was a select, removing them if it was an unset — and
	/// leaves visual mode. Mirrors yazi: the range only touches `selection`
	/// once, on commit, not row-by-row as the cursor moves over it.
	fn commit_visual(&mut self) -> bool {
		let Some(visual) = self.visual.take() else {
			return false;
		};
		let last = self.visible_len().saturating_sub(1);
		let (lo, hi) = visual.range(self.cursor.min(last));
		let paths: Vec<PathBuf> = self.visible_range(lo..hi.min(last).saturating_add(1)).into_iter().map(|(_, node)| node.path.clone()).collect();

		for path in paths {
			if visual.unset {
				self.selection.remove(&path);
			} else {
				self.selection.insert(path);
			}
		}
		true
	}

	/// Takes the paths highlighted by a plain visual selection without
	/// merging them into an older committed selection. Visual-unset is not
	/// an operation range: it still needs `commit_visual` to subtract from
	/// the existing selection.
	fn take_visual_targets(&mut self) -> Option<Vec<PathBuf>> {
		let visual = self.visual?;
		if visual.unset {
			return None;
		}
		self.visual = None;
		let last = self.visible_len().saturating_sub(1);
		let (lo, hi) = visual.range(self.cursor.min(last));
		Some(self.visible_range(lo..hi.min(last).saturating_add(1)).into_iter().map(|(_, node)| node.path.clone()).collect())
	}

	/// Resolves one operation's targets. A plain visual range is an explicit
	/// one-off target set and therefore wins over an older selection.
	fn take_action_targets(&mut self) -> Vec<PathBuf> {
		if let Some(targets) = self.take_visual_targets() {
			return targets;
		}
		self.commit_visual();
		self.action_targets()
	}

	/// Queues a one-off toast for `App` to pick up on its next drain —
	/// see `pending_notice`. A later call before that drain happens simply
	/// replaces the earlier one; nothing here needs more than the latest.
	pub(super) fn raise(&mut self, level: NoticeLevel, message: impl Into<String>) {
		self.pending_notice = Some((level, message.into()));
	}

	/// Opens a modal confirmation for the current operation targets. The
	/// event loop owns the modal keys and calls `take_pending_delete`;
	/// another ordinary `d`/`D` can never submit the destructive action.
	pub fn delete_selected(&mut self, mode: DeleteMode) {
		let visual_targets = self.take_visual_targets();
		if let Some(targets) = &visual_targets {
			// Deleting from Visual mode first makes that range the committed
			// selection. Canceling the confirmation therefore leaves exactly
			// what the user just selected, matching Yazi's interaction.
			self.selection.clear();
			for path in targets {
				self.selection.insert(path.clone());
			}
		}
		let root = &self.tree.root.path;
		let root = root.clone();
		let targets: Vec<_> = visual_targets.unwrap_or_else(|| self.take_action_targets()).into_iter().filter(|path| path != &root).collect();
		if targets.is_empty() {
			self.pending_delete = None;
			self.raise(NoticeLevel::Warn, "The current tree root cannot be deleted");
			return;
		}
		self.pending_delete = (!targets.is_empty()).then_some((targets, mode));
	}

	pub(super) fn delete_selected_configured(&mut self, mode: DeleteMode, confirm: bool) -> Option<(Vec<PathBuf>, DeleteMode)> {
		self.delete_selected(mode);
		if confirm { None } else { self.take_pending_delete(true) }
	}

	/// Takes the armed confirmation, handing the targets and mode to the
	/// caller (which owns the task queue) only if the user actually
	/// confirmed — declining or canceling just clears it.
	pub(super) fn take_pending_delete(&mut self, submit: bool) -> Option<(Vec<PathBuf>, DeleteMode)> {
		let pending = self.pending_delete.take()?;
		submit.then_some(pending)
	}

	/// The action targets to yank (the current selection, or the hovered
	/// node), with the selection then cleared since it converts into
	/// clipboard markers held by `App`. Commits a pending visual range
	/// first — mirrors yazi: yanking mid-visual-select acts on the
	/// highlighted range and leaves visual mode, rather than ignoring it.
	pub(super) fn take_yank_targets(&mut self) -> Vec<PathBuf> {
		let targets = self.take_action_targets();
		if !targets.is_empty() {
			self.selection.clear();
		}
		targets
	}

	/// Copies or moves `paths` into the directory under the cursor, or next
	/// to the cursor when it is a file (see
	/// `paste_target`). App hands this destination to its global task queue.
	pub(super) fn paste_destination(&self) -> Option<PathBuf> {
		self.paste_target()
	}

	/// Opens the rename prompt for whatever's under the cursor, prefilled
	/// with its current name in Insert mode, cursor at the end — ready to
	/// type over it. Renaming the tree's own root is refused — it would
	/// orphan every path already cached under it.
	pub fn start_rename(&mut self) {
		let Some((_, node)) = self.visible_at(self.cursor) else {
			return;
		};
		if node.path == self.tree.root.path {
			return;
		}
		let target = node.path.clone();
		let name = node.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

		self.input_seq += 1;
		self.input = Some(InputSession::new(self.input_seq, InputPurpose::Rename { target }, &name));
	}

	pub fn start_cd(&mut self) {
		self.input_seq += 1;
		let mut input = InputSession::new(self.input_seq, InputPurpose::Cd { base: self.tree.root.path.clone() }, "");
		self.schedule_completion(&mut input);
		self.input = Some(input);
	}

	pub fn cd_path(&mut self, value: &str) -> io::Result<()> {
		let path = resolve_path(&self.tree.root.path, value)?;
		self.cd(path)
	}

	pub fn rename_selected(&mut self, name: String) {
		let Some((_, node)) = self.visible_at(self.cursor) else { return };
		if node.path != self.tree.root.path { self.confirm_rename(node.path.clone(), name); }
	}

	pub fn start_create(&mut self) {
		let base = self.create_base();
		self.input_seq += 1;
		self.input = Some(InputSession::new(self.input_seq, InputPurpose::Create { base }, ""));
	}

	pub fn create_path(&mut self, value: String) {
		if !value.is_empty() { self.fs_scheduler.create_with_policy(self.create_base(), value, self.config.fs.create_conflict); }
	}

	fn create_base(&self) -> PathBuf {
		self.visible_at(self.cursor)
			.map(|(_, node)| (node.path.clone(), node.cha.is_dir && node.expanded))
			.map(|(path, create_inside)| if create_inside { path.clone() } else { self.tree.parent_of(&path).unwrap_or_else(|| self.tree.root.path.clone()) })
			.unwrap_or_else(|| self.tree.root.path.clone())
	}

	pub fn start_find(&mut self, previous: bool) {
		self.finder = None;
		self.input_seq += 1;
		self.input = Some(InputSession::new(self.input_seq, InputPurpose::Find { previous }, ""));
	}

	pub fn start_filter(&mut self) {
		self.filter = None;
		self.rebuild_projection();
		self.input_seq += 1;
		self.input = Some(InputSession::new(self.input_seq, InputPurpose::Filter, ""));
	}

	pub fn start_command(&mut self) {
		self.input_seq += 1;
		let mut input = InputSession::new(self.input_seq, InputPurpose::Command, "");
		self.schedule_completion(&mut input);
		self.input = Some(input);
	}

	pub fn find_arrow(&mut self, previous: bool, include_current: bool) {
		let Some(finder) = &self.finder else { return };
		let len = self.visible_len();
		if len == 0 {
			return;
		}
		let first = usize::from(!include_current);
		let found = (first..len).find_map(|offset| {
			let index = if previous { (self.cursor + len - offset % len) % len } else { (self.cursor + offset) % len };
			let name = node_name(self.visible_at(index)?.1);
			finder.matches(&name).then_some(index)
		});
		if let Some(cursor) = found
			&& cursor != self.cursor
		{
			self.cursor = cursor;
			self.preview.target_changed();
		}
	}

	pub fn repeat_find(&mut self, opposite: bool) {
		let Some(finder) = &self.finder else { return };
		self.find_arrow(finder.previous() ^ opposite, false);
	}

	/// Handles the common vim input used by rename and interactive cd.
	pub fn handle_input_key(&mut self, key: KeyEvent) -> Option<crate::command::Command> {
		let Some(mut input) = self.input.take() else {
			return None;
		};

		if input.completion.is_some() {
			match key.code {
				KeyCode::Up => {
					input.move_completion(-1);
					self.input = Some(input);
					return None;
				}
				KeyCode::Down => {
					input.move_completion(1);
					self.input = Some(input);
					return None;
				}
				KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
					input.move_completion(-1);
					self.input = Some(input);
					return None;
				}
				KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
					input.move_completion(1);
					self.input = Some(input);
					return None;
				}
				KeyCode::Tab => {
					if input.complete_selected() && input.is_cd() {
						self.schedule_completion(&mut input);
					}
					self.input = Some(input);
					return None;
				}
				_ => {}
			}
		}

		match key.code {
			KeyCode::Enter => {
				if input.is_cd() {
					input.complete_selected();
				}
				return self.submit_input(input);
			}
			KeyCode::Esc => {
				if input.state.mode != EditorMode::Normal {
					input.handler.on_key_event(key, &mut input.state);
					self.input = Some(input);
				}
			}
			// edtui's vim_mode doesn't bind `C` (vim's "change to end of
			// line") at all — synthesize it from what it does have: `D`
			// (delete to eol) followed by dropping straight into Insert,
			// appending right where the cut happened. Only in Normal mode;
			// typing a literal capital C elsewhere goes through untouched.
			KeyCode::Char('C') => {
				if input.state.mode != EditorMode::Normal {
					self.forward_input_key(input, key);
					return None;
				}
				// edtui's own uppercase-letter bindings (like this `D`) key
				// off the modifier flag, not just the letter's case — and
				// most terminals report Shift+<letter> as the already-
				// capitalized char with the modifier bit left unset, so it
				// has to be set explicitly here rather than forwarded from
				// whatever `key.modifiers` the incoming `C` carried.
				input.handler.on_key_event(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT), &mut input.state);
				// `D` leaves the Normal-mode cursor sitting *on* whatever's
				// now the last character (or col 0, on an emptied line) —
				// vim's `C` instead appends *after* it, so nudge to the
				// line's current length rather than just flipping the mode.
				input.state.cursor.col = input.state.lines.len_col(input.state.cursor.row).unwrap_or(0);
				input.state.mode = EditorMode::Insert;
				if input.is_cd() || input.find_previous().is_some() || input.is_command() {
					self.input_changed(&mut input);
				}
				self.input = Some(input);
			}
			_ => self.forward_input_key(input, key),
		}
		None
	}

	fn forward_input_key(&mut self, mut input: InputSession, key: KeyEvent) {
		let before = (input.value(), input.state.cursor.col);
		input.handler.on_key_event(key, &mut input.state);
		input.error = None;
		let after = (input.value(), input.state.cursor.col);
		if (input.is_cd() && before != after) || ((input.find_previous().is_some() || input.is_filter() || input.is_command()) && before.0 != after.0) {
			self.input_changed(&mut input);
		}
		self.input = Some(input);
	}

	fn input_changed(&mut self, input: &mut InputSession) {
		if input.is_cd() {
			self.schedule_completion(input);
		}
		if input.is_command() {
			self.schedule_completion(input);
		}
		if let Some(previous) = input.find_previous() {
			self.finder = Finder::new(input.value(), previous);
			if self.finder.is_some() {
				self.find_arrow(previous, true);
			}
		}
		if input.is_filter() {
			self.filter = Filter::new(input.value());
			self.rebuild_projection();
		}
	}

	fn submit_input(&mut self, mut input: InputSession) -> Option<crate::command::Command> {
		let value = input.value();
		match &input.purpose {
			InputPurpose::Command => match value.parse() {
				Ok(command) => return Some(command),
				Err(error) => { input.error = Some(error); self.input = Some(input); },
			},
			InputPurpose::Rename { target } => self.confirm_rename(target.clone(), value),
			InputPurpose::Create { base } => {
				if !value.is_empty() {
					self.fs_scheduler.create_with_policy(base.clone(), value, self.config.fs.create_conflict);
				}
			}
			InputPurpose::Find { previous } => {
				self.finder = Finder::new(value, *previous);
				if self.finder.is_some() {
					self.find_arrow(*previous, true);
				}
			}
			InputPurpose::Filter => {
				self.filter = Filter::new(value);
				self.rebuild_projection();
			}
			InputPurpose::Cd { base } => {
				if value.is_empty() {
					return None;
				}
				match resolve_path(base, &value).and_then(|path| self.cd(path)) {
					Ok(()) => {}
					Err(err) => {
						input.error = Some(err.to_string());
						self.input = Some(input);
					}
				}
			}
		}
		None
	}

	fn confirm_rename(&mut self, target: PathBuf, name: String) {
		let Some(parent) = target.parent() else {
			return;
		};
		let requested = parent.join(&name);
		if name.is_empty() || requested == target { return; }
		let dest = match self.config.fs.rename_conflict {
			crate::config::ConflictPolicy::Rename => unique_dest_avoiding(parent, requested.file_name().unwrap_or_default(), |_| false),
			crate::config::ConflictPolicy::Error if requested.exists() => { self.raise(NoticeLevel::Error, format!("{} already exists", requested.display())); return; },
			crate::config::ConflictPolicy::Error => requested,
		};
		if std::fs::rename(&target, &dest).is_err() {
			return;
		}
		let _ = self.tx.send(Event::Pubsub(Body::Renamed { from: target.clone(), to: dest.clone() }));

		self.watcher.unwatch(&target);
		self.fs_scheduler.forget(&target);
		self.selection.remove(&target);
		if self.tree.is_loaded(parent) {
			self.fs_scheduler.refresh(parent.to_path_buf());
		}
	}

	pub fn cd(&mut self, path: PathBuf) -> io::Result<()> {
		self.cd_inner(path, true)
	}

	fn cd_inner(&mut self, path: PathBuf, record: bool) -> io::Result<()> {
		if !std::fs::metadata(&path)?.is_dir() {
			return Err(io::Error::new(io::ErrorKind::InvalidInput, "target is not a directory"));
		}
		if path == self.tree.root.path {
			return Ok(());
		}
		let mut replacement = Self::open_configured(self.id, path, self.tx.clone(), self.config.clone())?;
		let _ = self.tx.send(Event::Visited(replacement.tree.root.path.clone()));
		let _ = self.tx.send(Event::Pubsub(Body::Cd { path: replacement.tree.root.path.clone() }));
		replacement.input_seq = self.input_seq;
		replacement.sort_policy = self.sort_policy;
		replacement.column_mode = self.column_mode;
		replacement.show_hidden = self.show_hidden;
		replacement.rebuild_projection();
		replacement.path_history = std::mem::replace(&mut self.path_history, PathHistory::new(replacement.tree.root.path.clone(), self.config.mgr.history_size));
		if record {
			replacement.path_history.push(replacement.tree.root.path.clone());
		}
		*self = replacement;
		Ok(())
	}

	pub fn history_back(&mut self) {
		let Some(path) = self.path_history.back().map(Path::to_path_buf) else {
			return;
		};
		if let Err(error) = self.cd_inner(path, false) {
			self.path_history.forward();
			self.raise(NoticeLevel::Error, error.to_string());
		}
	}

	pub fn history_forward(&mut self) {
		let Some(path) = self.path_history.forward().map(Path::to_path_buf) else {
			return;
		};
		if let Err(error) = self.cd_inner(path, false) {
			self.path_history.back();
			self.raise(NoticeLevel::Error, error.to_string());
		}
	}

	pub fn cd_selected(&mut self) {
		let Some(directory) = self.selected_dir() else {
			return;
		};
		if let Err(error) = self.cd(directory) {
			self.raise(NoticeLevel::Error, error.to_string());
		}
	}

	pub fn cd_trash(&mut self) {
		let dirs = trash_dirs();
		let Some(path) = dirs.iter().position(|path| path == &self.tree.root.path).and_then(|index| dirs.get((index + 1) % dirs.len())).or_else(|| dirs.first()).cloned() else {
			self.raise(NoticeLevel::Warn, "No trash location found for this platform");
			return;
		};
		if let Err(error) = self.cd(path) {
			self.raise(NoticeLevel::Error, error.to_string());
		}
	}

	pub fn cd_config(&mut self) {
		match home_dir().map(|home| home.join(".config")).and_then(|path| self.cd(path)) {
			Ok(()) => {}
			Err(error) => self.raise(NoticeLevel::Error, error.to_string()),
		}
	}

	pub fn reveal(&mut self, target: PathBuf) -> io::Result<()> {
		let target = target.canonicalize()?;
		if !target.starts_with(&self.tree.root.path) {
			return Err(io::Error::new(io::ErrorKind::InvalidInput, "target is outside the tab root"));
		}
		self.pending_reveal = Some(RevealState { target, refreshed_parent: false });
		self.continue_reveal();
		Ok(())
	}

	fn continue_reveal(&mut self) {
		let Some(state) = &self.pending_reveal else {
			return;
		};
		let target = state.target.clone();
		if self.visible_position(&target).is_some() {
			self.select(&target);
			self.pending_reveal = None;
			self.preview.target_changed();
			return;
		}

		let root = self.tree.root.path.clone();
		let Some(parent) = target.parent().map(Path::to_path_buf) else {
			self.pending_reveal = None;
			return;
		};
		let Ok(_) = parent.strip_prefix(&root) else {
			self.pending_reveal = None;
			return;
		};
		for directory in ancestor_directories(&root, &parent) {
			match self.tree.mark_expanded(&directory) {
				Some(needs_fetch) => {
					self.sync_projection(&directory);
					let _ = self.watcher.watch(&directory);
					if needs_fetch {
						self.fs_scheduler.refresh(directory);
						return;
					}
				}
				None => return,
			}
		}

		let Some(state) = &mut self.pending_reveal else {
			return;
		};
		if !state.refreshed_parent {
			state.refreshed_parent = true;
			self.fs_scheduler.refresh(parent);
		} else {
			self.pending_reveal = None;
			self.raise(NoticeLevel::Info, "reveal target is no longer present");
		}
	}

	/// Requests a fresh completion list, debounced by 50ms. Deliberately
	/// leaves the previous `input.completion` in place rather than clearing
	/// it here — every keystroke reaches this function, so clearing
	/// synchronously would blank the popup for the length of the debounce
	/// on every single character, flashing it empty-then-full repeatedly.
	/// `on_completion_loaded` replaces it once the fresh list actually
	/// arrives (with `None` if that list turns out to be empty).
	fn schedule_completion(&self, input: &mut InputSession) {
		let base = match &input.purpose {
			InputPurpose::Cd { base } => Some(base.clone()),
			InputPurpose::Command => None,
			_ => return,
		};
		if let Some(task) = input.completion_task.take() {
			task.abort();
		}
		input.revision += 1;
		let tab = self.id;
		let input_id = input.id;
		let revision = input.revision;
		let value = input.value();
		let cursor = input.state.cursor.col;
		let tx = self.tx.clone();
		input.completion_task = Some(tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			let result = tokio::task::spawn_blocking(move || match base {
				Some(base) => complete_directories(&base, &value, cursor),
				None => Ok(crate::command::completions(&value)),
			}).await.unwrap_or_else(|err| Err(io::Error::other(err)));
			let _ = tx.send(Event::CompletionLoaded { tab, input: input_id, revision, result });
		}));
	}

	pub fn on_completion_loaded(&mut self, input_id: u64, revision: u64, result: io::Result<Vec<String>>) {
		let Some(input) = &mut self.input else { return };
		if input.id != input_id || input.revision != revision {
			return;
		}
		input.completion_task.take();
		let command = matches!(input.purpose, InputPurpose::Command);
		input.completion = result.ok().filter(|items| !items.is_empty()).map(|candidates| Completion { candidates, selected: 0, command });
	}

	/// Esc cancels whatever's most "in progress": an open visual selection
	/// (committing it), then an active filter, then an armed delete, then
	/// the selection. Reaching here at all means no input prompt was open —
	/// while one is, Esc routes to `handle_input_key` instead.
	pub fn escape(&mut self) {
		if self.finder.take().is_some() {
			return;
		}
		if self.commit_visual() {
			return;
		}
		if self.filter.take().is_some() {
			self.rebuild_projection();
			return;
		}
		if self.pending_delete.take().is_some() {
			return;
		}
		self.selection.clear();
	}

	/// A watched directory changed on disk; request a fresh listing for it.
	/// Directories that were never expanded aren't watched, so this only
	/// ever fires for ones we actually have cached.
	pub fn on_changed(&mut self, path: PathBuf) {
		if self.tree.is_loaded(&path) {
			self.fs_scheduler.refresh(path);
		}
	}

	pub fn on_files_changed(&mut self, parent: PathBuf, changes: Vec<FsChange>) {
		if !self.tree.is_loaded(&parent) {
			return;
		}
		let hovered = self.visible_at(self.cursor).map(|(_, node)| node.path.clone());
		if !self.tree.apply_changes(&parent, changes, self.sort_policy) {
			return;
		}
		self.sync_projection(&parent);
		if let Some(path) = hovered
			&& let Some(position) = self.visible_position(&path)
		{
			self.cursor = position;
		}
		self.clamp_cursor();
		self.preview.target_changed();
		self.continue_reveal();
	}

	/// A background listing message arrived — either one more batch, or the
	/// final word on whether the whole listing succeeded. Just dispatches;
	/// `on_listing_batch` and `on_listing_done` each own one concern.
	pub fn on_loaded(&mut self, path: PathBuf, ticket: u64, result: io::Result<Vec<(PathBuf, Cha)>>, done: bool) {
		if !self.fs_scheduler.accept(&path, ticket, done) {
			return;
		}
		if done {
			self.on_listing_done(path, ticket, result);
		} else if let Ok(entries) = result {
			self.on_listing_batch(path, ticket, entries);
		}
	}

	/// First loads are appended directly so large directories become usable
	/// before their scan finishes; refreshes keep the old listing visible
	/// and buffer batches until `on_listing_done` completes.
	fn on_listing_batch(&mut self, path: PathBuf, ticket: u64, entries: Vec<(PathBuf, Cha)>) {
		if !self.pending_listings.contains_key(&path) {
			let listing = if self.tree.begin_incremental_listing(&path) {
				PendingListing::Incremental { ticket }
			} else {
				PendingListing::Buffered { ticket, entries: Vec::new() }
			};
			self.pending_listings.insert(path.clone(), listing);
		}
		match self.pending_listings.get_mut(&path) {
			Some(PendingListing::Incremental { ticket: current }) if *current == ticket => {
				let start = self.tree.root.find(&path).and_then(|node| node.children.as_ref()).map_or(0, Vec::len);
				self.tree.append_listing(&path, entries);
				self.projection.append_children(&self.tree.root, &path, start, self.filter.as_ref(), self.show_hidden);
			}
			Some(PendingListing::Buffered { ticket: current, entries: buffered }) if *current == ticket => buffered.extend(entries),
			_ => return,
		}
		self.continue_reveal();
		self.clamp_cursor();
	}

	/// A listing finished, one way or another. A failed listing (permission
	/// denied, the directory vanished, …) collapses the node and pins the
	/// error to it (`Node::load_error`) instead of leaving it stuck showing
	/// "(loading…)" forever — this is a standing problem with that one
	/// directory, not a one-off toast.
	fn on_listing_done(&mut self, path: PathBuf, ticket: u64, result: io::Result<Vec<(PathBuf, Cha)>>) {
		let hovered = self.visible_at(self.cursor).map(|(_, node)| node.path.clone());
		let pending = self.pending_listings.remove(&path);
		match result {
			Ok(_) => {
				let mut reordered = false;
				match PendingListing::finish(pending, ticket) {
					Some(ListingOutcome::Incremental) => {
						if let Some(order) = self.tree.finish_incremental_listing(&path, self.sort_policy) {
							reordered = self.filter.is_none() && self.projection.reorder_new_children(&self.tree.root, &path, &order, self.show_hidden);
						}
					}
					Some(ListingOutcome::Full(entries)) => {
						self.tree.apply_listing(&path, entries, self.sort_policy);
					}
					None => return,
				}
				if !reordered {
					self.sync_projection(&path);
				}
				if let Some(hovered) = hovered {
					self.select(&hovered);
				}
			}
			Err(error) => {
				if matches!(&pending, Some(PendingListing::Incremental { ticket: current }) if *current == ticket) {
					self.tree.discard_incremental_listing(&path);
				}
				self.tree.fail_listing(&path, error.to_string());
				self.sync_projection(&path);
			}
		}
		self.continue_reveal();
		self.clamp_cursor();
	}

	pub fn on_created(&mut self, base: PathBuf, value: String, target: PathBuf, result: io::Result<()>) {
		if let Err(error) = result {
			self.input_seq += 1;
			let mut input = InputSession::new(self.input_seq, InputPurpose::Create { base }, &value);
			input.error = Some(error.to_string());
			self.input = Some(input);
			return;
		}

		self.pending_reveal = Some(RevealState {
			target: target.clone(),
			refreshed_parent: false,
		});
		let mut refresh = target.parent();
		while let Some(path) = refresh {
			if self.tree.is_loaded(path) {
				self.fs_scheduler.refresh(path.to_path_buf());
				return;
			}
			refresh = path.parent();
		}
	}

	pub fn on_deleted(&mut self, paths: Vec<PathBuf>) {
		for path in &paths {
			self.cancel_listing(path);
			self.watcher.unwatch(path);
			self.fs_scheduler.forget(path);
			self.selection.remove(path);
		}
		self.refresh_parents(&paths);
	}

	pub fn on_pasted(&mut self, target: PathBuf) {
		if self.tree.is_loaded(&target) {
			self.fs_scheduler.refresh(target);
		}
	}

	pub fn on_linked(&mut self, target: PathBuf, result: io::Result<()>) {
		match result {
			Ok(()) => self.on_pasted(target),
			Err(error) => self.raise(NoticeLevel::Error, error.to_string()),
		}
	}

	fn selected_dir(&self) -> Option<PathBuf> {
		let (_, node) = self.visible_at(self.cursor)?;
		node.cha.is_dir.then(|| node.path.clone())
	}

	fn select(&mut self, path: &Path) {
		if let Some(i) = self.visible_position(path) {
			self.cursor = i;
		}
	}

	/// What an operation like delete/yank should act on: the current
	/// selection if there is one, otherwise just whatever's under the cursor.
	fn action_targets(&self) -> Vec<PathBuf> {
		if !self.selection.is_empty() {
			return self.selection.iter().cloned().collect();
		}
		self.visible_at(self.cursor).map(|(_, node)| node.path.clone()).into_iter().collect()
	}

	pub fn take_open_targets(&mut self) -> Vec<PathBuf> {
		self.take_action_targets()
	}

	pub fn copy_text(&mut self, kind: CopyKind) -> Vec<u8> {
		let mut paths = self.take_action_targets();
		paths.sort();
		if matches!(kind, CopyKind::DirectoryPath | CopyKind::DirectoryUrl) {
			let root = &self.tree.root.path;
			paths = paths.into_iter().filter_map(|path| if path == *root { Some(path) } else { path.parent().map(Path::to_path_buf) }).collect();
			paths.dedup();
			if paths.is_empty() {
				paths.push(root.clone());
			}
		}

		paths
			.iter()
			.filter_map(|path| match kind {
				CopyKind::Path | CopyKind::DirectoryPath => Some(path.as_os_str().as_bytes().to_vec()),
				CopyKind::Url | CopyKind::DirectoryUrl => Some(file_url(path)),
				CopyKind::Filename => path.file_name().map(|name| name.as_bytes().to_vec()),
				CopyKind::Stem => path.file_stem().map(|name| name.as_bytes().to_vec()),
			})
			.collect::<Vec<_>>()
			.join(&b'\n')
	}

	/// An *expanded* directory is an explicit destination, including an
	/// empty one and the tree root (always expanded on open). A collapsed
	/// directory is just another row — same as a file, it means "beside
	/// this", not "into this" — matching `collapse_selected`/`start_create`.
	fn paste_target(&self) -> Option<PathBuf> {
		let (_, node) = self.visible_at(self.cursor)?;
		if node.cha.is_dir && node.expanded { Some(node.path.clone()) } else { self.tree.parent_of(&node.path) }
	}

	fn refresh_parents(&mut self, paths: &[PathBuf]) {
		let mut parents: Vec<PathBuf> = paths.iter().filter_map(|p| self.tree.parent_of(p)).collect();
		parents.sort();
		parents.dedup();
		for parent in parents {
			if self.tree.is_loaded(&parent) {
				self.fs_scheduler.refresh(parent);
			}
		}
	}

	fn clamp_cursor(&mut self) {
		let len = self.visible_len();
		if self.cursor >= len {
			self.cursor = len.saturating_sub(1);
		}
	}

	/// Both files and directories display the size reported by their own
	/// filesystem metadata. This is deliberately not a recursive directory
	/// total: rendering reads `Cha` only and never starts background I/O.
	pub fn status_line(&self) -> StatusLine {
		let mode = match self.visual {
			Some(visual) if visual.unset => StatusMode::Unset,
			Some(_) => StatusMode::Select,
			None => StatusMode::Normal,
		};
		let Some((_, node)) = self.visible_at(self.cursor) else {
			return StatusLine::empty(mode);
		};
		let name = node.path.file_name().map_or_else(|| node.path.display().to_string(), |n| n.to_string_lossy().into_owned());
		// `error` here is left for `render.rs` to fill in from the active
		// input's own validation error, if any — `pending_notice` is a
		// separate, App-level toast now, not something the status line shows.
		StatusLine {
			mode,
			name,
			size: format_size(node.cha.len),
			permissions: node.cha.permissions(),
			error: None,
		}
	}
}

fn node_name(node: &Node) -> String {
	node.path.file_name().map_or_else(|| node.path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

fn file_url(path: &Path) -> Vec<u8> {
	let mut out = b"file://".to_vec();
	for byte in path.as_os_str().as_bytes() {
		if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
			out.push(*byte);
		} else {
			out.extend_from_slice(format!("%{byte:02X}").as_bytes());
		}
	}
	out
}

fn resolve_path(base: &Path, value: &str) -> io::Result<PathBuf> {
	let raw = if value == "~" {
		home_dir()?
	} else if let Some(rest) = value.strip_prefix("~/") {
		home_dir()?.join(rest)
	} else {
		let path = PathBuf::from(value);
		if path.is_absolute() { path } else { base.join(path) }
	};
	let path = raw.canonicalize()?;
	if !path.is_dir() {
		return Err(io::Error::new(io::ErrorKind::InvalidInput, "path is not a directory"));
	}
	Ok(path)
}

fn home_dir() -> io::Result<PathBuf> {
	std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
}

/// The current platform's default trash location, queried through the
/// `trash` crate's own platform code wherever it exposes one — hand-rolling
/// `.Trash`/XDG paths ourselves would just reinvent (and risk getting
/// wrong) exactly what that crate already handles for `delete_selected`.
/// macOS is the one exception: its `os_limited` module (and the folder
/// listing it would otherwise provide) isn't compiled there at all, because
/// macOS trashes files through an OS API that never needs a path from us —
/// so `~/.Trash` is `cd_trash`'s only option, not a choice we're avoiding.
fn trash_dirs() -> Vec<PathBuf> {
	#[cfg(target_os = "macos")]
	{
		home_dir().ok().map(|home| vec![home.join(".Trash")]).unwrap_or_default()
	}
	#[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
	{
		let Ok(folders) = trash::os_limited::trash_folders() else {
			return Vec::new();
		};
		let home = home_dir().ok();
		let mut folders: Vec<_> = folders.into_iter().collect();
		folders.sort_by_key(|folder| (!home.as_deref().is_some_and(|home| folder.starts_with(home)), folder.clone()));
		folders
	}
	#[cfg(not(any(target_os = "macos", all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))))]
	{
		Vec::new()
	}
}

fn ancestor_directories(root: &Path, parent: &Path) -> Vec<PathBuf> {
	let mut directories = vec![root.to_path_buf()];
	let Ok(relative) = parent.strip_prefix(root) else {
		return directories;
	};
	let mut path = root.to_path_buf();
	for component in relative.components() {
		path.push(component.as_os_str());
		directories.push(path.clone());
	}
	directories
}

fn complete_directories(base: &Path, value: &str, cursor: usize) -> io::Result<Vec<String>> {
	let before: String = value.chars().take(cursor).collect();
	let split = before.char_indices().rev().find(|(_, c)| *c == '/' || *c == '\\').map(|(i, _)| i + 1).unwrap_or(0);
	let (parent_text, word) = before.split_at(split);
	let parent = if parent_text == "~/" {
		home_dir()?
	} else if let Some(rest) = parent_text.strip_prefix("~/") {
		home_dir()?.join(rest)
	} else {
		let path = PathBuf::from(parent_text);
		if path.is_absolute() { path } else { base.join(path) }
	};

	let smart = !word.bytes().any(|b| b.is_ascii_uppercase());
	let needle = if smart { word.to_ascii_lowercase() } else { word.to_owned() };
	let mut exact = Vec::new();
	let mut fuzzy = Vec::new();
	for entry in std::fs::read_dir(parent)? {
		let entry = entry?;
		let file_type = entry.file_type()?;
		// `file_type` won't follow a symlink, so a symlinked directory needs
		// a second look through `Path::is_dir` (which does) before it's
		// ruled out — same reasoning as `fs::engine::cha_for`.
		if !file_type.is_dir() && !(file_type.is_symlink() && entry.path().is_dir()) {
			continue;
		}
		let name = entry.file_name().to_string_lossy().into_owned();
		let candidate = if smart { name.to_ascii_lowercase() } else { name.clone() };
		if candidate.starts_with(&needle) {
			exact.push(name);
		} else if candidate.contains(&needle) {
			fuzzy.push(name);
		}
	}
	exact.sort_by_key(|s| s.to_ascii_lowercase());
	fuzzy.sort_by_key(|s| s.to_ascii_lowercase());
	exact.extend(fuzzy);
	exact.truncate(30);
	Ok(exact)
}

#[cfg(test)]
mod tests {
	use std::fs;

	use tokio::sync::mpsc;

	use super::*;

	async fn tab(root: &Path) -> (Tab, mpsc::UnboundedReceiver<Event>) {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let mut tab = Tab::open(0, root.to_path_buf(), tx).unwrap();

		pump(&mut tab, &mut rx).await;

		(tab, rx)
	}

	async fn pump(tab: &mut Tab, rx: &mut mpsc::UnboundedReceiver<Event>) {
		loop {
			let event = rx.recv().await.unwrap();
			let done = !matches!(&event, Event::Loaded { done: false, .. } | Event::Pubsub(_));
			apply(tab, event);
			if done {
				break;
			}
		}
	}

	/// A minimal stand-in for `Dispatcher::dispatch` that only understands
	/// the events a single `Tab` can produce for itself — these tests don't
	/// need `App`'s tab-routing at all.
	fn apply(tab: &mut Tab, event: Event) {
		match event {
			Event::Changed { path, .. } => tab.on_changed(path),
			Event::FilesChanged { parent, changes, .. } => tab.on_files_changed(parent, changes),
			Event::Loaded { path, ticket, result, done, .. } => tab.on_loaded(path, ticket, result, done),
			Event::Created { base, value, target, result, .. } => tab.on_created(base, value, target, result),
			// No subscriber exists yet in these single-tab tests; a DDS
			// publish from `cd`/rename is a no-op here.
			Event::Pubsub(_) => {}
			_ => panic!("unexpected event in a single-tab test"),
		}
	}

	fn key(code: KeyCode) -> KeyEvent {
		KeyEvent::new(code, KeyModifiers::NONE)
	}

	fn rename_value(tab: &Tab) -> String {
		tab.input.as_ref().unwrap().state.lines.to_vecs().into_iter().next().unwrap_or_default().into_iter().collect()
	}

	fn set_input_value(tab: &mut Tab, value: &str) {
		let input = tab.input.as_mut().unwrap();
		input.state.lines = edtui::Lines::from(value);
		input.state.cursor = edtui::Index2::new(0, value.chars().count());
	}

	#[tokio::test]
	async fn sorting_keeps_the_hovered_path_and_updates_related_columns() {
		let root = std::env::temp_dir().join("tuzi-tab-test-sort");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("a.txt"), b"a").unwrap();
		fs::write(root.join("b.txt"), b"bbbb").unwrap();
		let root = root.canonicalize().unwrap();
		let (mut tab, _rx) = tab(&root).await;
		tab.cursor = tab.visible_position(&root.join("a.txt")).unwrap();

		tab.set_sort(SortPolicy::new(SortBy::Size, true));

		assert_eq!(tab.visible_at(tab.cursor).unwrap().1.path, root.join("a.txt"));
		assert_eq!(tab.visible()[1].1.path, root.join("b.txt"));
		assert_eq!(tab.column_mode, ColumnMode::Size);

		tab.set_sort(SortPolicy::new(SortBy::Modified, false));
		assert_eq!(tab.column_mode, ColumnMode::Modified);
		tab.set_sort(SortPolicy::new(SortBy::Extension, false));
		assert_eq!(tab.column_mode, ColumnMode::Modified, "name-like sorting leaves the chosen column alone");

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn hidden_files_toggle_without_reloading_and_cursor_falls_back_to_parent() {
		let root = std::env::temp_dir().join("tuzi-tab-test-hidden");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join(".hidden"), b"").unwrap();
		fs::write(root.join("visible"), b"").unwrap();
		let root = root.canonicalize().unwrap();
		let (mut tab, _rx) = tab(&root).await;

		assert!(!tab.show_hidden);
		assert_eq!(tab.visible().len(), 2);
		assert!(tab.tree.root.children.as_ref().unwrap().iter().any(|node| node.path == root.join(".hidden")));

		tab.toggle_hidden();
		let hidden = root.join(".hidden");
		tab.cursor = tab.visible_position(&hidden).unwrap();
		assert_eq!(tab.visible().len(), 3);

		tab.toggle_hidden();
		assert_eq!(tab.visible_at(tab.cursor).unwrap().1.path, root);
		assert_eq!(tab.visible().len(), 2);

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn cd_selected_and_cd_parent_change_the_tab_root() {
		let root = std::env::temp_dir().join("tuzi-tab-test-cd-navigation");
		fs::create_dir_all(root.join("child")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1);
		tab.cd_selected();
		assert_eq!(tab.tree.root.path, root.join("child"));

		tab.cd_path("..").unwrap();
		assert_eq!(tab.tree.root.path, root);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn directory_history_is_per_tab_and_discards_the_forward_branch() {
		let root = std::env::temp_dir().join("tuzi-tab-test-directory-history");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("b")).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut tab, _rx) = tab(&root).await;

		tab.cd(root.join("a")).unwrap();
		tab.cd(root.join("b")).unwrap();
		tab.history_back();
		assert_eq!(tab.tree.root.path, root.join("a"));
		tab.history_back();
		assert_eq!(tab.tree.root.path, root);
		tab.history_forward();
		assert_eq!(tab.tree.root.path, root.join("a"));

		tab.cd(root.clone()).unwrap();
		tab.history_forward();
		assert_eq!(tab.tree.root.path, root, "a new navigation discards the old forward branch");

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn failed_history_navigation_restores_its_cursor() {
		let root = std::env::temp_dir().join("tuzi-tab-test-failed-directory-history");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("gone")).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut tab, _rx) = tab(&root).await;

		tab.cd(root.join("gone")).unwrap();
		tab.history_back();
		fs::remove_dir_all(root.join("gone")).unwrap();
		tab.history_forward();
		assert_eq!(tab.tree.root.path, root);

		fs::create_dir_all(root.join("gone")).unwrap();
		tab.history_forward();
		assert_eq!(tab.tree.root.path, root.join("gone"), "the failed attempt did not consume the forward entry");

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn a_failed_listing_collapses_the_node_and_reports_why() {
		use std::os::unix::fs::PermissionsExt;

		let root = std::env::temp_dir().join("tuzi-tab-test-load-failure");
		fs::create_dir_all(root.join("locked")).unwrap();
		let root = root.canonicalize().unwrap();
		fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.move_cursor(1); // onto "locked"
		tab.expand_selected();
		pump(&mut tab, &mut rx).await; // Loaded(locked) -> permission denied

		let locked = tab.tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("locked")).unwrap();
		assert!(!locked.expanded, "collapses instead of staying stuck showing \"(loading…)\" forever");
		assert!(locked.children.is_none());
		assert!(locked.load_error.is_some(), "pinned to the node, instead of failing silently");

		fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn collapse_on_the_open_directory_itself() {
		let root = std::env::temp_dir().join("tuzi-tab-test-self");
		fs::create_dir_all(root.join("a/b")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1); // onto "a"
		tab.expand_selected(); // flips `expanded` immediately, no need to wait on the fetch
		tab.collapse_selected();

		let a = tab.tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("a")).unwrap();
		assert!(!a.expanded);
		assert_eq!(tab.visible()[tab.cursor].1.path, root.join("a"), "cursor stays on a");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn enter_toggles_a_directory_but_not_a_file() {
		let root = std::env::temp_dir().join("tuzi-tab-test-toggle-expand");
		fs::create_dir_all(root.join("dir")).unwrap();
		fs::write(root.join("file.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1); // dir
		tab.toggle_expand_selected();
		assert!(tab.tree.root.children.as_ref().unwrap()[0].expanded);
		tab.toggle_expand_selected();
		assert!(!tab.tree.root.children.as_ref().unwrap()[0].expanded);

		tab.move_cursor(1); // file.txt
		let cursor = tab.cursor;
		tab.toggle_expand_selected();
		assert_eq!(tab.cursor, cursor);

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn collapse_from_a_child_jumps_to_and_closes_the_parent() {
		let root = std::env::temp_dir().join("tuzi-tab-test-child");
		fs::create_dir_all(root.join("a/b")).unwrap();
		fs::write(root.join("a/leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.move_cursor(1); // onto "a"
		tab.expand_selected();
		pump(&mut tab, &mut rx).await; // wait for "a"'s listing so it actually has visible children
		tab.move_cursor(1); // onto "a/b" or "a/leaf.txt", a's first child

		tab.collapse_selected();

		let a = tab.tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("a")).unwrap();
		assert!(!a.expanded, "collapsing from a child closes its parent");
		assert_eq!(tab.visible()[tab.cursor].1.path, root.join("a"), "cursor jumps to the parent");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn toggle_select_marks_the_row_and_moves_on() {
		let root = std::env::temp_dir().join("tuzi-tab-test-select");
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("z")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1); // onto "a" (sorted before "z")
		tab.toggle_selected();

		assert!(tab.selection.contains(&root.join("a")));
		assert_eq!(tab.cursor, 2, "toggling moves on, like holding space to select a run");

		tab.move_cursor(-1); // back onto "a"
		tab.toggle_selected();
		assert!(!tab.selection.contains(&root.join("a")), "toggling again clears it");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn find_moves_between_visible_matches_and_wraps() {
		let root = std::env::temp_dir().join("tuzi-tab-test-find");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("alpha.rs"), b"").unwrap();
		fs::write(root.join("beta.txt"), b"").unwrap();
		fs::write(root.join("gamma.rs"), b"").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.finder = Finder::new(".rs".into(), false);
		tab.find_arrow(false, false);
		assert_eq!(node_name(tab.visible()[tab.cursor].1), "alpha.rs");
		tab.find_arrow(false, false);
		assert_eq!(node_name(tab.visible()[tab.cursor].1), "gamma.rs");
		tab.find_arrow(false, false);
		assert_eq!(node_name(tab.visible()[tab.cursor].1), "alpha.rs", "next wraps");
		tab.find_arrow(true, false);
		assert_eq!(node_name(tab.visible()[tab.cursor].1), "gamma.rs", "previous wraps");

		tab.finder = Finder::new(".rs".into(), true);
		tab.repeat_find(false);
		assert_eq!(node_name(tab.visible()[tab.cursor].1), "alpha.rs", "n preserves the original previous direction");
		tab.repeat_find(true);
		assert_eq!(node_name(tab.visible()[tab.cursor].1), "gamma.rs", "N reverses the original direction");

		tab.escape();
		assert!(tab.finder.is_none());
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn find_updates_while_the_prompt_is_being_edited() {
		let root = std::env::temp_dir().join("tuzi-tab-test-live-find");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("alpha.txt"), b"").unwrap();
		fs::write(root.join("gamma.txt"), b"").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.start_find(false);
		for ch in "gamma".chars() {
			tab.handle_input_key(key(KeyCode::Char(ch)));
		}
		assert_eq!(node_name(tab.visible()[tab.cursor].1), "gamma.txt");
		assert!(tab.finder.as_ref().is_some_and(|finder| finder.matches("gamma.txt")));

		for _ in 0..5 {
			tab.handle_input_key(key(KeyCode::Backspace));
		}
		assert!(tab.finder.is_none(), "emptying the prompt clears live highlights");

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn filter_hides_non_matching_entries_live() {
		let root = std::env::temp_dir().join("tuzi-tab-test-live-filter");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("alpha.txt"), b"").unwrap();
		fs::write(root.join("gamma.txt"), b"").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		assert_eq!(tab.visible().len(), 3, "root plus both files, unfiltered");

		tab.start_filter();
		for ch in "gamma".chars() {
			tab.handle_input_key(key(KeyCode::Char(ch)));
		}

		let names: Vec<_> = tab.visible().iter().map(|(_, node)| node_name(node)).collect();
		assert_eq!(names, [node_name(&tab.tree.root), "gamma.txt".to_owned()], "alpha.txt is hidden, the root stays");

		for _ in 0..5 {
			tab.handle_input_key(key(KeyCode::Backspace));
		}
		assert!(tab.filter.is_none(), "emptying the prompt clears the filter");
		assert_eq!(tab.visible().len(), 3, "clearing it live restores every entry");

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn filter_keeps_the_root_visible_even_when_nothing_matches() {
		let root = std::env::temp_dir().join("tuzi-tab-test-filter-no-match");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("alpha.txt"), b"").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.start_filter();
		for ch in "nope".chars() {
			tab.handle_input_key(key(KeyCode::Char(ch)));
		}

		assert_eq!(tab.visible().len(), 1, "no matches, but the root is never filtered away");
		assert_eq!(node_name(tab.visible()[0].1), node_name(&tab.tree.root));

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn escape_clears_the_filter_and_restores_hidden_entries() {
		let root = std::env::temp_dir().join("tuzi-tab-test-escape-filter");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("alpha.txt"), b"").unwrap();
		fs::write(root.join("gamma.txt"), b"").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.start_filter();
		for ch in "gamma".chars() {
			tab.handle_input_key(key(KeyCode::Char(ch)));
		}
		tab.handle_input_key(key(KeyCode::Enter));
		assert!(tab.filter.is_some());
		assert_eq!(tab.visible().len(), 2, "root plus the one match");

		tab.escape();
		assert!(tab.filter.is_none());
		assert_eq!(tab.visible().len(), 3, "escape restores what the filter hid");

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn delete_requires_an_explicit_modal_confirmation() {
		let root = std::env::temp_dir().join("tuzi-tab-test-delete");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1); // onto "leaf.txt"

		tab.delete_selected(DeleteMode::Trash);
		assert!(root.join("leaf.txt").exists(), "opening the confirmation does not delete");
		assert!(tab.pending_delete.is_some());

		tab.delete_selected(DeleteMode::Trash);
		assert!(root.join("leaf.txt").exists(), "a second d still does not submit the modal");
		assert!(tab.pending_delete.is_some());

		// Declining just clears the confirmation — actually removing the
		// file is the caller's job once it gets `Some(_)` back from a
		// confirmed take, which owns the task queue this hands off to.
		assert!(tab.take_pending_delete(false).is_none());
		assert!(root.join("leaf.txt").exists());

		tab.delete_selected(DeleteMode::Permanent);
		let (targets, mode) = tab.take_pending_delete(true).unwrap();
		assert_eq!(targets, vec![root.join("leaf.txt")]);
		assert_eq!(mode, DeleteMode::Permanent);
		assert!(tab.pending_delete.is_none());

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn delete_refuses_to_target_the_current_tree_root() {
		let root = std::env::temp_dir().join("tuzi-tab-test-protect-root");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut tab, _rx) = tab(&root).await;

		for mode in [DeleteMode::Trash, DeleteMode::Permanent] {
			tab.delete_selected(mode);
			assert!(tab.pending_delete.is_none());
			assert_eq!(tab.pending_notice.as_ref().map(|(_, message)| message.as_str()), Some("The current tree root cannot be deleted"));
		}
		assert!(root.exists());
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn escape_cancels_a_pending_delete_before_touching_selection() {
		let root = std::env::temp_dir().join("tuzi-tab-test-escape");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1); // onto "leaf.txt"
		tab.toggle_selected();
		tab.move_cursor(-1);
		tab.delete_selected(DeleteMode::Trash); // arms, targeting the selection
		assert!(tab.pending_delete.is_some());

		tab.escape();
		assert!(tab.pending_delete.is_none(), "escape cancels the pending delete first");
		assert!(tab.selection.contains(&root.join("leaf.txt")), "…without touching the selection yet");

		tab.escape();
		assert!(tab.selection.is_empty(), "a second escape then clears the selection");
		assert!(root.join("leaf.txt").exists(), "nothing was ever actually deleted");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn take_yank_targets_returns_the_selection_and_clears_it() {
		let root = std::env::temp_dir().join("tuzi-tab-test-yank-targets");
		fs::create_dir_all(root.join("src")).unwrap();
		fs::write(root.join("src/leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.move_cursor(1); // onto "src"
		tab.expand_selected();
		pump(&mut tab, &mut rx).await; // src.children = [leaf.txt]
		tab.move_cursor(1); // onto "src/leaf.txt"
		tab.selection.insert(root.join("src/leaf.txt"));

		let targets = tab.take_yank_targets();
		assert_eq!(targets, vec![root.join("src/leaf.txt")]);
		assert!(tab.selection.is_empty(), "yanking converts selected markers into clipboard markers");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn take_yank_targets_commits_a_pending_visual_range_and_leaves_visual_mode() {
		let root = std::env::temp_dir().join("tuzi-tab-test-yank-visual");
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("b")).unwrap();
		fs::create_dir_all(root.join("c")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.selection.insert(root.join("c"));
		tab.move_cursor(1); // onto "a"
		tab.enter_visual(false);
		tab.move_cursor(1); // onto "b" — range is now a..=b, not yet committed

		let mut targets = tab.take_yank_targets();
		targets.sort();
		assert_eq!(targets, [root.join("a"), root.join("b")], "the current visual range replaces an older selection");
		assert!(tab.visual.is_none(), "yanking mid-visual-select leaves visual mode");
		assert!(tab.selection.is_empty(), "and the range converts straight into clipboard markers, same as a committed selection");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn delete_and_open_commit_the_pending_visual_range() {
		let root = std::env::temp_dir().join("tuzi-tab-test-actions-visual");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("a"), b"a").unwrap();
		fs::write(root.join("b"), b"b").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1);
		tab.enter_visual(false);
		tab.move_cursor(1);
		tab.delete_selected(DeleteMode::Trash);
		assert_eq!(tab.pending_delete.as_ref().unwrap().0.len(), 2);
		assert!(tab.visual.is_none());

		tab.pending_delete = None;
		tab.selection.clear();
		tab.move_cursor(-1);
		tab.enter_visual(false);
		tab.move_cursor(1);
		assert_eq!(tab.take_open_targets().len(), 2);
		assert!(tab.visual.is_none());
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn delete_in_visual_mode_uses_only_the_current_range() {
		let root = std::env::temp_dir().join("tuzi-tab-test-delete-visual-replaces-selection");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		for name in ["a", "b", "c"] {
			fs::write(root.join(name), name.as_bytes()).unwrap();
		}
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.selection.insert(root.join("a"));
		tab.move_cursor(2); // onto "b"
		tab.enter_visual(false);
		tab.move_cursor(1); // range is b..=c
		tab.delete_selected(DeleteMode::Trash);

		let mut targets = tab.pending_delete.as_ref().unwrap().0.clone();
		targets.sort();
		assert_eq!(targets, [root.join("b"), root.join("c")]);
		assert!(tab.visual.is_none());
		assert!(!tab.selection.contains(&root.join("a")), "the older selection was replaced");
		assert!(tab.selection.contains(&root.join("b")));
		assert!(tab.selection.contains(&root.join("c")));

		assert!(tab.take_pending_delete(false).is_none());
		assert!(tab.selection.contains(&root.join("b")), "canceling keeps the committed visual selection");
		assert!(tab.selection.contains(&root.join("c")), "canceling keeps the committed visual selection");

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn paste_destination_enters_an_expanded_directory_but_not_a_collapsed_one() {
		let root = std::env::temp_dir().join("tuzi-tab-test-paste-into");
		fs::create_dir_all(root.join("dst/sub")).unwrap();
		fs::write(root.join("source.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.move_cursor(1); // dst
		tab.expand_selected();
		pump(&mut tab, &mut rx).await; // dst.children = [sub]

		tab.move_cursor(1); // onto "dst/sub" — a directory, but never itself expanded
		assert_eq!(tab.paste_destination(), Some(root.join("dst")), "a collapsed directory is just another row to paste beside, not into");

		tab.move_cursor(-1); // back onto "dst", which is expanded
		assert_eq!(tab.paste_destination(), Some(root.join("dst")), "an expanded directory is a valid paste-into target");

		tab.move_cursor(2); // past "dst/sub", onto "source.txt"
		assert_eq!(tab.paste_destination(), Some(root.clone()), "a file always pastes beside itself");
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn cd_reports_the_new_root_as_visited() {
		let root = std::env::temp_dir().join("tuzi-tab-test-cd-visited");
		let target = root.join("target");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&target).unwrap();
		let root = root.canonicalize().unwrap();
		let target = target.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.cd(target.clone()).unwrap();

		// Sent synchronously inside `cd_inner`, before the replacement tab's
		// own (spawned) initial listing gets a chance to run — so it's
		// always the first thing to arrive, ahead of any `Loaded` events.
		let event = rx.recv().await.unwrap();
		assert!(matches!(event, Event::Visited(path) if path == target), "cd must be recorded for zoxide the same way a shell's cd hook would");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn paste_destination_accepts_the_tree_root() {
		let root = std::env::temp_dir().join("tuzi-tab-test-paste-into-root");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("source.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (tab, _rx) = tab(&root).await;
		assert_eq!(tab.cursor, 0, "starts on the tree's own root");

		assert_eq!(tab.paste_destination(), Some(root.clone()));

		fs::remove_dir_all(root).unwrap();
	}

	#[cfg(unix)]
	#[tokio::test]
	async fn paste_link_creates_a_relative_symlink_at_the_destination() {
		let root = std::env::temp_dir().join(format!("tuzi-tab-test-paste-link-relative-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("dst")).unwrap();
		fs::write(root.join("source.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.fs_scheduler.link(vec![root.join("source.txt")], root.join("dst"), false);

		let Event::Linked { target, result, .. } = rx.recv().await.unwrap() else { panic!("expected a Linked event") };
		tab.on_linked(target, result);

		assert_eq!(fs::read_link(root.join("dst/source.txt")).unwrap(), PathBuf::from("../source.txt"));

		fs::remove_dir_all(root).unwrap();
	}

	#[cfg(unix)]
	#[tokio::test]
	async fn paste_link_absolute_stores_the_source_path_unchanged() {
		let root = std::env::temp_dir().join(format!("tuzi-tab-test-paste-link-absolute-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("dst")).unwrap();
		fs::write(root.join("source.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.fs_scheduler.link(vec![root.join("source.txt")], root.join("dst"), true);

		let Event::Linked { target, result, .. } = rx.recv().await.unwrap() else { panic!("expected a Linked event") };
		tab.on_linked(target, result);

		assert_eq!(fs::read_link(root.join("dst/source.txt")).unwrap(), root.join("source.txt"));

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn clipboard_copy_variants_match_the_yazi_chords() {
		let root = std::env::temp_dir().join("tuzi-tab-test-copy-text");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("folder")).unwrap();
		fs::write(root.join("hello world.rs"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();
		let (mut tab, _rx) = tab(&root).await;
		tab.select(&root.join("hello world.rs"));

		assert_eq!(tab.copy_text(CopyKind::Path), root.join("hello world.rs").as_os_str().as_bytes());
		assert_eq!(tab.copy_text(CopyKind::Filename), b"hello world.rs");
		assert_eq!(tab.copy_text(CopyKind::Stem), b"hello world");
		assert_eq!(tab.copy_text(CopyKind::DirectoryPath), root.as_os_str().as_bytes());
		assert_eq!(tab.copy_text(CopyKind::Url), file_url(&root.join("hello world.rs")));

		tab.select(&root.join("folder"));
		assert_eq!(tab.copy_text(CopyKind::DirectoryPath), root.as_os_str().as_bytes(), "cd copies the containing directory");

		tab.cursor = 0;
		assert_eq!(tab.copy_text(CopyKind::DirectoryPath), root.as_os_str().as_bytes(), "the synthetic root row represents the cwd");
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn visual_select_commits_the_range_on_escape() {
		let root = std::env::temp_dir().join("tuzi-tab-test-visual-select");
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("b")).unwrap();
		fs::create_dir_all(root.join("c")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1); // onto "a"
		tab.enter_visual(false);
		tab.move_cursor(1); // onto "b" — range is now a..=b

		tab.escape();
		assert!(tab.visual.is_none());
		assert!(tab.selection.contains(&root.join("a")));
		assert!(tab.selection.contains(&root.join("b")));
		assert!(!tab.selection.contains(&root.join("c")), "outside the range, untouched");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn visual_unset_clears_only_the_range_it_covers() {
		let root = std::env::temp_dir().join("tuzi-tab-test-visual-unset");
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("b")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.selection.insert(root.join("a"));
		tab.selection.insert(root.join("b"));

		tab.move_cursor(1); // onto "a"
		tab.enter_visual(true); // VISUAL UNSET
		tab.escape(); // range is just "a" (cursor never moved off it)

		assert!(!tab.selection.contains(&root.join("a")), "unset over a clears it");
		assert!(tab.selection.contains(&root.join("b")), "b was outside the range");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn rename_confirm_renames_on_disk_and_refreshes_the_parent() {
		let root = std::env::temp_dir().join("tuzi-tab-test-rename");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("old.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.move_cursor(1); // onto "old.txt"

		tab.start_rename();
		assert_eq!(rename_value(&tab), "old.txt", "prefilled with the current name");
		assert!(tab.input.as_ref().unwrap().state.mode == EditorMode::Insert);

		// still in Insert mode (cursor at the end): clear the prefill and
		// type the new name, one raw key event at a time through edtui.
		for _ in 0..7 {
			tab.handle_input_key(key(KeyCode::Backspace));
		}
		for c in "new.txt".chars() {
			tab.handle_input_key(key(KeyCode::Char(c)));
		}
		assert_eq!(rename_value(&tab), "new.txt");

		tab.handle_input_key(key(KeyCode::Enter)); // confirm
		assert!(tab.input.is_none());
		pump(&mut tab, &mut rx).await; // parent's listing refreshes

		assert!(!root.join("old.txt").exists());
		assert!(root.join("new.txt").exists());
		let renamed = tab.tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("new.txt"));
		assert!(renamed.is_some(), "tree reflects the new name after the refresh lands");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn create_input_makes_a_file_and_reveals_it() {
		let root = std::env::temp_dir().join("tuzi-tab-test-create-file");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.start_create();
		set_input_value(&mut tab, "note.txt");
		tab.handle_input_key(key(KeyCode::Enter));
		pump(&mut tab, &mut rx).await;
		pump(&mut tab, &mut rx).await;

		assert!(root.join("note.txt").is_file());
		assert_eq!(tab.visible()[tab.cursor].1.path, root.join("note.txt"));
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn create_beside_a_hovered_file_and_reveals_it() {
		let root = std::env::temp_dir().join("tuzi-tab-test-create-beside-file");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("parent")).unwrap();
		fs::write(root.join("parent/existing.txt"), b"existing").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.select(&root.join("parent"));
		tab.expand_selected();
		pump(&mut tab, &mut rx).await;
		tab.select(&root.join("parent/existing.txt"));
		tab.start_create();
		set_input_value(&mut tab, "sibling.txt");
		tab.handle_input_key(key(KeyCode::Enter));
		pump(&mut tab, &mut rx).await;
		pump(&mut tab, &mut rx).await;

		let target = root.join("parent/sibling.txt");
		assert!(target.is_file());
		assert_eq!(tab.visible()[tab.cursor].1.path, target);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn create_beside_a_collapsed_directory_and_reveals_it() {
		let root = std::env::temp_dir().join("tuzi-tab-test-create-beside-collapsed-directory");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("parent/closed")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.select(&root.join("parent"));
		tab.expand_selected();
		pump(&mut tab, &mut rx).await;
		tab.select(&root.join("parent/closed"));
		tab.start_create();
		set_input_value(&mut tab, "sibling.txt");
		tab.handle_input_key(key(KeyCode::Enter));
		pump(&mut tab, &mut rx).await;
		pump(&mut tab, &mut rx).await;

		let target = root.join("parent/sibling.txt");
		assert!(target.is_file());
		assert_eq!(tab.visible()[tab.cursor].1.path, target);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn create_inside_an_expanded_directory_and_reveals_it() {
		let root = std::env::temp_dir().join("tuzi-tab-test-create-inside-expanded-directory");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("open")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.select(&root.join("open"));
		tab.expand_selected();
		pump(&mut tab, &mut rx).await;
		tab.start_create();
		set_input_value(&mut tab, "child.txt");
		tab.handle_input_key(key(KeyCode::Enter));
		pump(&mut tab, &mut rx).await;
		pump(&mut tab, &mut rx).await;

		let target = root.join("open/child.txt");
		assert!(target.is_file());
		assert_eq!(tab.visible()[tab.cursor].1.path, target);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn trailing_slash_creates_nested_directories() {
		let root = std::env::temp_dir().join("tuzi-tab-test-create-dir");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.start_create();
		set_input_value(&mut tab, "one/two/");
		tab.handle_input_key(key(KeyCode::Enter));
		pump(&mut tab, &mut rx).await;

		assert!(root.join("one/two").is_dir());
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn create_does_not_overwrite_an_existing_file() {
		let root = std::env::temp_dir().join("tuzi-tab-test-create-existing");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("keep.txt"), b"keep").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.start_create();
		set_input_value(&mut tab, "keep.txt");
		tab.handle_input_key(key(KeyCode::Enter));
		pump(&mut tab, &mut rx).await;

		assert_eq!(fs::read(root.join("keep.txt")).unwrap(), b"keep");
		assert_eq!(tab.input.as_ref().unwrap().value(), "keep.txt");
		assert!(tab.input.as_ref().unwrap().error.is_some());
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn rename_escape_from_insert_stays_open_then_cancels() {
		let root = std::env::temp_dir().join("tuzi-tab-test-rename-escape");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("keep.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1); // onto "keep.txt"
		tab.start_rename();

		tab.handle_input_key(key(KeyCode::Esc)); // Insert -> Normal, handled by edtui itself
		assert!(tab.input.is_some(), "first escape only drops to normal mode");
		assert!(tab.input.as_ref().unwrap().state.mode == EditorMode::Normal);

		tab.handle_input_key(key(KeyCode::Esc)); // Normal -> we intercept and close
		assert!(tab.input.is_none());
		assert!(root.join("keep.txt").exists(), "nothing was renamed");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn capital_c_changes_from_the_cursor_to_the_end_of_line() {
		let root = std::env::temp_dir().join("tuzi-tab-test-rename-change");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("keep.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1); // onto "keep.txt"
		tab.start_rename();

		tab.handle_input_key(key(KeyCode::Esc)); // Insert -> Normal, cursor lands on the last char ('t')
		tab.handle_input_key(key(KeyCode::Char('0'))); // BOL
		// edtui's `w` treats punctuation as its own word class, matching
		// real vim: "keep" | "." | "txt" is three words, not one.
		tab.handle_input_key(key(KeyCode::Char('w'))); // -> the '.'
		tab.handle_input_key(key(KeyCode::Char('w'))); // -> start of "txt"

		// edtui's vim_mode has no native `C` binding at all — this is our
		// own synthesized delete-to-eol-then-insert.
		tab.handle_input_key(key(KeyCode::Char('C')));
		assert_eq!(rename_value(&tab), "keep.");
		assert!(tab.input.as_ref().unwrap().state.mode == EditorMode::Insert, "C drops straight into Insert");

		for c in "md".chars() {
			tab.handle_input_key(key(KeyCode::Char(c)));
		}
		assert_eq!(rename_value(&tab), "keep.md");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn interactive_cd_replaces_navigation() {
		let root = std::env::temp_dir().join("tuzi-tab-test-cd");
		let next = root.join("next");
		fs::create_dir_all(&next).unwrap();
		let root = root.canonicalize().unwrap();
		let next = next.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.start_cd();
		set_input_value(&mut tab, "next");
		tab.handle_input_key(key(KeyCode::Enter));

		assert_eq!(tab.tree.root.path, next);
		assert_eq!(tab.cursor, 0);
		assert!(tab.input.is_none());

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn invalid_cd_keeps_the_input_open_with_an_error() {
		let root = std::env::temp_dir().join("tuzi-tab-test-cd-invalid");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.start_cd();
		set_input_value(&mut tab, "missing");
		tab.handle_input_key(key(KeyCode::Enter));

		assert_eq!(tab.tree.root.path, root);
		assert!(tab.input.as_ref().unwrap().error.is_some());

		fs::remove_dir_all(&root).unwrap();
	}

	#[test]
	fn completion_lists_only_matching_directories_with_smart_case() {
		let root = std::env::temp_dir().join("tuzi-tab-test-completion");
		fs::create_dir_all(root.join("Projects")).unwrap();
		fs::create_dir_all(root.join("profiles")).unwrap();
		fs::write(root.join("prompt.txt"), b"not a directory").unwrap();
		let root = root.canonicalize().unwrap();

		assert_eq!(complete_directories(&root, "pro", 3).unwrap(), ["profiles", "Projects"]);
		assert_eq!(complete_directories(&root, "Pro", 3).unwrap(), ["Projects"]);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn status_line_displays_the_directorys_own_metadata_size() {
		let root = std::env::temp_dir().join("tuzi-tab-test-directory-cha");
		fs::create_dir_all(root.join("dir")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		tab.move_cursor(1); // onto "dir"

		let dir = tab.tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("dir")).unwrap();
		let expected = format_size(dir.cha.len);

		let line = tab.status_line();
		assert_eq!(line.mode, StatusMode::Normal);
		assert_eq!(line.size, expected, "directories display Cha::len directly");

		tab.enter_visual(false);
		assert_eq!(tab.status_line().mode, StatusMode::Select);
		tab.visual = Some(Visual::new(tab.cursor, true));
		assert_eq!(tab.status_line().mode, StatusMode::Unset);

		fs::remove_dir_all(&root).unwrap();
	}
}
