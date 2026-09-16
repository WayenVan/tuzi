use std::{io, path::{Path, PathBuf}, sync::Arc, time::Duration};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edtui::EditorMode;
use tokio::sync::mpsc::UnboundedSender;

use crate::{column_mode::ColumnMode, core::{Node, Selection, Tree, Visual}, event::Event, finder::Finder, fs::{Cha, Engine, LocalEngine, format_size}, preview::Preview, scheduler::FsScheduler, status::{StatusLine, StatusMode}, watcher::Watcher};

use super::input::{Completion, InputPurpose, InputSession};

/// One tab: its own tree, cursor, selection and background workers —
/// everything but whether the whole program should quit. The clipboard
/// lives on `App` instead, shared by every tab (yank in one, paste in
/// another). `id` is assigned once at creation and never reused or
/// renumbered, so that background events tagged with it
/// (`Loaded`/`Deleted`/`Pasted`/`Changed`) keep routing to the right tab
/// even after some *other* tab closes and every tab after it would
/// otherwise shift position in `App::tabs`.
pub struct Tab {
	pub id:             usize,
	pub tree:           Tree,
	pub cursor:         usize,
	/// The tree view's scroll offset (index of its first visible row),
	/// persisted across frames so ratatui only nudges it when the cursor
	/// would otherwise leave the viewport, instead of recomputing it from
	/// scratch — which would re-track the cursor on every move.
	pub scroll:         usize,
	pub column_mode:    ColumnMode,
	pub preview:        Preview,
	pub watcher:        Watcher,
	pub fs_scheduler:   FsScheduler,
	pub selection:      Selection,
	pub visual:         Option<Visual>,
	pub pending_delete: Option<Vec<PathBuf>>,
	pub finder:         Option<Finder>,
	pub notice:         Option<String>,
	pending_reveal:     Option<RevealState>,
	pub(super) input:   Option<InputSession>,
	input_seq:          u64,
	tx:                 UnboundedSender<Event>,
}

struct RevealState {
	target:           PathBuf,
	refreshed_parent: bool,
}

impl Tab {
	pub fn open(id: usize, path: PathBuf, tx: UnboundedSender<Event>) -> io::Result<Self> {
		let mut tree = Tree::open(path)?;
		let root_path = tree.root.path.clone();
		let needs_fetch = tree.mark_expanded(&root_path).unwrap_or(false);

		let mut watcher = Watcher::new(id, tx.clone())?;
		watcher.watch(&root_path)?;

		let engine: Arc<dyn Engine> = Arc::new(LocalEngine);
		let mut fs_scheduler = FsScheduler::new(id, tx.clone(), engine);
		if needs_fetch {
			fs_scheduler.refresh(root_path);
		}

		Ok(Self {
			id,
			tree,
			cursor: 0,
			scroll: 0,
			column_mode: ColumnMode::None,
			preview: Preview::new(id, tx.clone()),
			watcher,
			fs_scheduler,
			selection: Selection::default(),
			visual: None,
			pending_delete: None,
			finder: None,
			notice: None,
			pending_reveal: None,
			input: None,
			input_seq: 0,
			tx,
		})
	}

	pub fn visible(&self) -> Vec<(usize, &Node)> { self.tree.root.visible(0) }

	pub fn move_cursor(&mut self, delta: isize) {
		let len = self.visible().len();
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
		let cursor = self.visible().len().saturating_sub(1);
		if cursor != self.cursor {
			self.cursor = cursor;
			self.preview.target_changed();
		}
	}

	/// Marks the directory open immediately (the triangle flips, the row
	/// stays put) and, if it's never been listed, kicks off a background
	/// read — the listing lands later as a `Loaded` event instead of
	/// blocking this call.
	pub fn expand_selected(&mut self) {
		let Some(path) = self.selected_dir() else { return };
		let needs_fetch = self.tree.mark_expanded(&path).unwrap_or(false);
		let _ = self.watcher.watch(&path);
		if needs_fetch {
			self.fs_scheduler.refresh(path);
		}
	}

	pub fn toggle_expand_selected(&mut self) {
		let Some((_, node)) = self.visible().into_iter().nth(self.cursor) else { return };
		if !node.cha.is_dir {
			return;
		}
		if node.expanded {
			let path = node.path.clone();
			self.tree.collapse(&path);
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
		let Some((_, node)) = self.visible().into_iter().nth(self.cursor) else { return };
		let path = node.path.clone();

		let target = if node.cha.is_dir && node.expanded { path } else { self.tree.parent_of(&path).unwrap_or(path) };

		self.tree.collapse(&target);
		self.watcher.unwatch(&target);
		self.fs_scheduler.forget(&target);
		self.select(&target);
	}

	pub fn toggle_selected(&mut self) {
		if let Some((_, node)) = self.visible().into_iter().nth(self.cursor) {
			self.selection.toggle(node.path.clone());
		}
		self.move_cursor(1);
	}

	pub fn enter_visual(&mut self, unset: bool) { self.visual = Some(Visual::new(self.cursor, unset)); }

	/// Applies the pending visual range to the selection — adding every row
	/// in it if this was a select, removing them if it was an unset — and
	/// leaves visual mode. Mirrors yazi: the range only touches `selection`
	/// once, on commit, not row-by-row as the cursor moves over it.
	fn commit_visual(&mut self) -> bool {
		let Some(visual) = self.visual.take() else { return false };
		let rows = self.visible();
		let last = rows.len().saturating_sub(1);
		let (lo, hi) = visual.range(self.cursor.min(last));
		let paths: Vec<PathBuf> = rows[lo..=hi.min(last)].iter().map(|(_, node)| node.path.clone()).collect();

		for path in paths {
			if visual.unset {
				self.selection.remove(&path);
			} else {
				self.selection.insert(path);
			}
		}
		true
	}

	/// Opens a modal confirmation for the current operation targets. The
	/// event loop owns the modal keys and calls `confirm_delete`; another
	/// ordinary `d` can never submit the destructive action.
	pub fn delete_selected(&mut self) {
		let targets = self.action_targets();
		self.pending_delete = (!targets.is_empty()).then_some(targets);
	}

	pub fn confirm_delete(&mut self, submit: bool) {
		let Some(targets) = self.pending_delete.take() else { return };
		if submit {
			self.fs_scheduler.delete(targets);
		}
	}

	/// The action targets to yank (the current selection, or the hovered
	/// node), with the selection then cleared since it converts into
	/// clipboard markers held by `App`.
	pub(super) fn take_yank_targets(&mut self) -> Vec<PathBuf> {
		let targets = self.action_targets();
		if !targets.is_empty() {
			self.selection.clear();
		}
		targets
	}

	/// Copies or moves `paths` into the cursor's parent directory (see
	/// `paste_target`) in the background; the target's listing refreshes
	/// once `Pasted` comes back. Returns whether there was a parent to
	/// paste into.
	pub(super) fn paste_into(&mut self, paths: Vec<PathBuf>, cut: bool) -> bool {
		let Some(target_dir) = self.paste_target() else { return false };
		if cut {
			self.fs_scheduler.move_paths(paths, target_dir);
		} else {
			self.fs_scheduler.copy(paths, target_dir);
		}
		true
	}

	/// Opens the rename prompt for whatever's under the cursor, prefilled
	/// with its current name in Insert mode, cursor at the end — ready to
	/// type over it. Renaming the tree's own root is refused — it would
	/// orphan every path already cached under it.
	pub fn start_rename(&mut self) {
		let Some((_, node)) = self.visible().into_iter().nth(self.cursor) else { return };
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

	pub fn start_create(&mut self) {
		self.input_seq += 1;
		self.input = Some(InputSession::new(
			self.input_seq,
			InputPurpose::Create { base: self.tree.root.path.clone() },
			"",
		));
	}

	pub fn start_find(&mut self, previous: bool) {
		self.finder = None;
		self.input_seq += 1;
		self.input = Some(InputSession::new(self.input_seq, InputPurpose::Find { previous }, ""));
	}

	pub fn find_arrow(&mut self, previous: bool, include_current: bool) {
		let Some(finder) = &self.finder else { return };
		let rows = self.visible();
		if rows.is_empty() {
			return;
		}
		let len = rows.len();
		let first = usize::from(!include_current);
		let found = (first..len).find_map(|offset| {
			let index = if previous {
				(self.cursor + len - offset % len) % len
			} else {
				(self.cursor + offset) % len
			};
			let name = node_name(rows[index].1);
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
	pub fn handle_input_key(&mut self, key: KeyEvent) {
		let Some(mut input) = self.input.take() else { return };

		if input.is_cd() && input.completion.is_some() {
			match key.code {
				KeyCode::Up => {
					input.move_completion(-1);
					self.input = Some(input);
					return;
				}
				KeyCode::Down => {
					input.move_completion(1);
					self.input = Some(input);
					return;
				}
				KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
					input.move_completion(-1);
					self.input = Some(input);
					return;
				}
				KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
					input.move_completion(1);
					self.input = Some(input);
					return;
				}
				KeyCode::Tab => {
					if input.complete_selected() {
						self.schedule_completion(&mut input);
					}
					self.input = Some(input);
					return;
				}
				_ => {}
			}
		}

		match key.code {
			KeyCode::Enter => {
				if input.is_cd() {
					input.complete_selected();
				}
				self.submit_input(input);
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
					return;
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
				if input.is_cd() || input.find_previous().is_some() {
					self.input_changed(&mut input);
				}
				self.input = Some(input);
			}
			_ => self.forward_input_key(input, key),
		}
	}

	fn forward_input_key(&mut self, mut input: InputSession, key: KeyEvent) {
		let before = (input.value(), input.state.cursor.col);
		input.handler.on_key_event(key, &mut input.state);
		input.error = None;
		let after = (input.value(), input.state.cursor.col);
		if (input.is_cd() && before != after) || (input.find_previous().is_some() && before.0 != after.0) {
			self.input_changed(&mut input);
		}
		self.input = Some(input);
	}

	fn input_changed(&mut self, input: &mut InputSession) {
		if input.is_cd() {
			self.schedule_completion(input);
		}
		if let Some(previous) = input.find_previous() {
				self.finder = Finder::new(input.value(), previous);
			if self.finder.is_some() {
				self.find_arrow(previous, true);
			}
		}
	}

	fn submit_input(&mut self, mut input: InputSession) {
		let value = input.value();
		match &input.purpose {
			InputPurpose::Rename { target } => self.confirm_rename(target.clone(), value),
			InputPurpose::Create { base } => {
				if !value.is_empty() {
					self.fs_scheduler.create(base.clone(), value);
				}
			}
			InputPurpose::Find { previous } => {
				self.finder = Finder::new(value, *previous);
				if self.finder.is_some() {
					self.find_arrow(*previous, true);
				}
			}
			InputPurpose::Cd { base } => {
				if value.is_empty() {
					return;
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
	}

	fn confirm_rename(&mut self, target: PathBuf, name: String) {
		let Some(parent) = target.parent() else { return };
		let dest = parent.join(&name);
		if name.is_empty() || dest == target || std::fs::rename(&target, &dest).is_err() {
			return;
		}

		self.watcher.unwatch(&target);
		self.fs_scheduler.forget(&target);
		self.selection.remove(&target);
		if self.tree.is_loaded(parent) {
			self.fs_scheduler.refresh(parent.to_path_buf());
		}
	}

	pub fn cd(&mut self, path: PathBuf) -> io::Result<()> {
		if !std::fs::metadata(&path)?.is_dir() {
			return Err(io::Error::new(io::ErrorKind::InvalidInput, "target is not a directory"));
		}
		if path == self.tree.root.path {
			return Ok(());
		}
		let mut replacement = Self::open(self.id, path, self.tx.clone())?;
		replacement.input_seq = self.input_seq;
		*self = replacement;
		Ok(())
	}

	pub fn cd_parent(&mut self) {
		let Some(parent) = self.tree.root.path.parent().map(Path::to_path_buf) else { return };
		if let Err(error) = self.cd(parent) {
			self.notice = Some(error.to_string());
		}
	}

	pub fn cd_selected(&mut self) {
		let Some(directory) = self.selected_dir() else { return };
		if let Err(error) = self.cd(directory) {
			self.notice = Some(error.to_string());
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
		let Some(state) = &self.pending_reveal else { return };
		let target = state.target.clone();
		if self.visible().iter().any(|(_, node)| node.path == target) {
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
					let _ = self.watcher.watch(&directory);
					if needs_fetch {
						self.fs_scheduler.refresh(directory);
						return;
					}
				}
				None => return,
			}
		}

		let Some(state) = &mut self.pending_reveal else { return };
		if !state.refreshed_parent {
			state.refreshed_parent = true;
			self.fs_scheduler.refresh(parent);
		} else {
			self.pending_reveal = None;
			self.notice = Some("reveal target is no longer present".into());
		}
	}

	fn schedule_completion(&self, input: &mut InputSession) {
		let InputPurpose::Cd { base } = &input.purpose else { return };
		if let Some(task) = input.completion_task.take() {
			task.abort();
		}
		input.revision += 1;
		input.completion = None;
		let tab = self.id;
		let input_id = input.id;
		let revision = input.revision;
		let base = base.clone();
		let value = input.value();
		let cursor = input.state.cursor.col;
		let tx = self.tx.clone();
		input.completion_task = Some(tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			let result = tokio::task::spawn_blocking(move || complete_directories(&base, &value, cursor))
				.await
				.unwrap_or_else(|err| Err(io::Error::other(err)));
			let _ = tx.send(Event::CompletionLoaded { tab, input: input_id, revision, result });
		}));
	}

	pub fn on_completion_loaded(&mut self, input_id: u64, revision: u64, result: io::Result<Vec<String>>) {
		let Some(input) = &mut self.input else { return };
		if input.id != input_id || input.revision != revision {
			return;
		}
		input.completion_task.take();
		input.completion = result.ok().filter(|items| !items.is_empty()).map(|candidates| Completion { candidates, selected: 0 });
	}

	/// Esc cancels whatever's most "in progress": an open visual selection
	/// (committing it), then an armed delete, then the selection. Reaching
	/// here at all means no input prompt was open — while one is, Esc
	/// routes to `handle_input_key` instead.
	pub fn escape(&mut self) {
		if self.finder.take().is_some() {
			return;
		}
		if self.commit_visual() {
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

	/// A background listing finished. `accept` first checks it's still the
	/// most recent request for that path — a superseded one is discarded
	/// rather than clobbering a listing a newer request already applied.
	pub fn on_loaded(&mut self, path: PathBuf, ticket: u64, result: io::Result<Vec<(PathBuf, Cha)>>) {
		if !self.fs_scheduler.accept(&path, ticket) {
			return;
		}
		if let Ok(entries) = result {
			self.tree.apply_listing(&path, entries);
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

		self.pending_reveal = Some(RevealState { target: target.clone(), refreshed_parent: false });
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

	fn selected_dir(&self) -> Option<PathBuf> {
		let (_, node) = self.visible().into_iter().nth(self.cursor)?;
		node.cha.is_dir.then(|| node.path.clone())
	}

	fn select(&mut self, path: &Path) {
		if let Some(i) = self.visible().iter().position(|(_, node)| node.path == *path) {
			self.cursor = i;
		}
	}

	/// What an operation like delete/yank should act on: the current
	/// selection if there is one, otherwise just whatever's under the cursor.
	fn action_targets(&self) -> Vec<PathBuf> {
		if !self.selection.is_empty() {
			return self.selection.iter().cloned().collect();
		}
		self.visible().into_iter().nth(self.cursor).map(|(_, node)| node.path.clone()).into_iter().collect()
	}

	pub fn open_targets(&self) -> Vec<PathBuf> { self.action_targets() }

	/// Where paste drops files: always the parent of whatever's under the
	/// cursor, never the cursor's own node — the cursor marks a position in
	/// the tree, not a directory to descend into. `None` when the cursor is
	/// on the tree's own root, which has no parent to paste into.
	fn paste_target(&self) -> Option<PathBuf> {
		let (_, node) = self.visible().into_iter().nth(self.cursor)?;
		self.tree.parent_of(&node.path)
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
		let len = self.visible().len();
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
		let Some((_, node)) = self.visible().into_iter().nth(self.cursor) else { return StatusLine::empty(mode) };
		let name = node.path.file_name().map_or_else(|| node.path.display().to_string(), |n| n.to_string_lossy().into_owned());
		StatusLine { mode, name, size: format_size(node.cha.len), permissions: node.cha.permissions(), error: self.notice.clone() }
	}
}

fn node_name(node: &Node) -> String {
	node.path.file_name().map_or_else(|| node.path.display().to_string(), |name| name.to_string_lossy().into_owned())
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

fn ancestor_directories(root: &Path, parent: &Path) -> Vec<PathBuf> {
	let mut directories = vec![root.to_path_buf()];
	let Ok(relative) = parent.strip_prefix(root) else { return directories };
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
		if !entry.file_type()?.is_dir() {
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

		let event = rx.recv().await.unwrap();
		apply(&mut tab, event);

		(tab, rx)
	}

	async fn pump(tab: &mut Tab, rx: &mut mpsc::UnboundedReceiver<Event>) {
		let event = rx.recv().await.unwrap();
		apply(tab, event);
	}

	/// A minimal stand-in for `Dispatcher::dispatch` that only understands
	/// the events a single `Tab` can produce for itself — these tests don't
	/// need `App`'s tab-routing at all.
	fn apply(tab: &mut Tab, event: Event) {
		match event {
			Event::Changed { path, .. } => tab.on_changed(path),
			Event::Loaded { path, ticket, result, .. } => tab.on_loaded(path, ticket, result),
			Event::Deleted { paths, .. } => tab.on_deleted(paths),
			Event::Pasted { target, .. } => tab.on_pasted(target),
			Event::Created { base, value, target, result, .. } => tab.on_created(base, value, target, result),
			_ => panic!("unexpected event in a single-tab test"),
		}
	}

	fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }

	fn rename_value(tab: &Tab) -> String {
		tab.input.as_ref().unwrap().state.lines.to_vecs().into_iter().next().unwrap_or_default().into_iter().collect()
	}

	fn set_input_value(tab: &mut Tab, value: &str) {
		let input = tab.input.as_mut().unwrap();
		input.state.lines = edtui::Lines::from(value);
		input.state.cursor = edtui::Index2::new(0, value.chars().count());
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

		tab.cd_parent();
		assert_eq!(tab.tree.root.path, root);
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
	async fn delete_requires_an_explicit_modal_confirmation() {
		let root = std::env::temp_dir().join("tuzi-tab-test-delete");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.move_cursor(1); // onto "leaf.txt"

		tab.delete_selected();
		assert!(root.join("leaf.txt").exists(), "opening the confirmation does not delete");
		assert!(tab.pending_delete.is_some());

		tab.delete_selected();
		assert!(root.join("leaf.txt").exists(), "a second d still does not submit the modal");
		assert!(tab.pending_delete.is_some());

		tab.confirm_delete(true);
		pump(&mut tab, &mut rx).await; // wait for the background removal to actually land
		assert!(!root.join("leaf.txt").exists());
		assert!(tab.pending_delete.is_none());

		fs::remove_dir_all(&root).unwrap();
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
		tab.delete_selected(); // arms, targeting the selection
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
	async fn paste_into_targets_the_cursors_parent_not_the_cursor_itself() {
		let root = std::env::temp_dir().join("tuzi-tab-test-paste-into");
		fs::create_dir_all(root.join("dst/sub")).unwrap();
		fs::write(root.join("source.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, mut rx) = tab(&root).await;
		tab.move_cursor(1); // dst
		tab.expand_selected();
		pump(&mut tab, &mut rx).await; // dst.children = [sub]
		tab.move_cursor(1); // onto "dst/sub", itself a directory

		// Pasting while the cursor sits on "sub" lands in "sub"'s parent,
		// "dst" — not inside "sub", even though "sub" is a directory.
		assert!(tab.paste_into(vec![root.join("source.txt")], true));
		pump(&mut tab, &mut rx).await;

		assert!(!root.join("source.txt").exists());
		assert!(root.join("dst/source.txt").exists());
		assert!(!root.join("dst/sub/source.txt").exists());
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn paste_into_refuses_when_the_cursor_is_on_the_tree_root() {
		let root = std::env::temp_dir().join("tuzi-tab-test-paste-into-root");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("source.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut tab, _rx) = tab(&root).await;
		assert_eq!(tab.cursor, 0, "starts on the tree's own root");

		assert!(!tab.paste_into(vec![root.join("source.txt")], true), "the root has no parent to paste into");
		assert!(root.join("source.txt").exists(), "nothing was moved");

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
