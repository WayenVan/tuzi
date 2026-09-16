use std::{env, io, path::{Path, PathBuf}, sync::Arc, thread};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use edtui::{EditorEventHandler, EditorMode, EditorState, Index2, Lines};
use ratatui::layout::{Constraint, Direction, Layout};
use tokio::sync::mpsc;

use crate::{core::{Node, Selection, Tree, Visual}, event::Event, fs::{Engine, LocalEngine}, scheduler::Scheduler, tui::{Raterm, widgets::{Prompt, StatusBar, TreeView}}, watcher::Watcher};

use super::{Dispatcher, Router};

/// An open rename prompt: which path it targets, edtui's own buffer/cursor/
/// mode state, and a per-session key handler (fresh each time, so a
/// half-finished `d`-then-motion sequence from a previous rename can never
/// bleed into the next one).
struct Rename {
	target:  PathBuf,
	state:   EditorState,
	handler: EditorEventHandler,
}

pub struct App {
	pub tree:           Tree,
	pub cursor:         usize,
	pub quit:           bool,
	pub watcher:        Watcher,
	pub scheduler:      Scheduler,
	pub selection:      Selection,
	pub visual:         Option<Visual>,
	pub clipboard:      Vec<PathBuf>,
	pub pending_delete: Option<Vec<PathBuf>>,
	rename:             Option<Rename>,
}

impl App {
	pub async fn serve() -> io::Result<()> {
		let (tx, mut rx) = mpsc::unbounded_channel();

		// Raw terminal events are wrapped, not translated, here — the
		// background thread doesn't know whether a rename prompt is open,
		// so the split between tree keymap and raw-key-to-edtui only
		// happens once the event reaches the main loop below.
		let input_tx = tx.clone();
		thread::spawn(move || {
			while let Ok(term_event) = crossterm::event::read() {
				if input_tx.send(Event::Term(term_event)).is_err() {
					break;
				}
			}
		});

		let mut tree = Tree::open(env::current_dir()?)?;
		let root_path = tree.root.path.clone();
		let needs_fetch = tree.mark_expanded(&root_path).unwrap_or(false);

		let mut watcher = Watcher::new(tx.clone())?;
		watcher.watch(&root_path);

		let engine: Arc<dyn Engine> = Arc::new(LocalEngine);
		let mut scheduler = Scheduler::new(tx, engine);
		if needs_fetch {
			scheduler.refresh(root_path);
		}

		let mut app = Self {
			tree,
			cursor: 0,
			quit: false,
			watcher,
			scheduler,
			selection: Selection::default(),
			visual: None,
			clipboard: Vec::new(),
			pending_delete: None,
			rename: None,
		};
		let mut term = Raterm::start()?;

		let draw = |app: &mut App, term: &mut Raterm| -> io::Result<()> {
			// Taken out (and put back at the end) so that its `&mut` doesn't
			// overlap, for the borrow checker's purposes, with the `&Node`s
			// `app.visible()` lends out below — both ultimately borrow from
			// `app` through `&self` methods, which erases field-level
			// disjointness even though `rename` and `tree` never actually
			// touch each other.
			let mut rename = app.rename.take();

			let rows = app.visible();
			let (status, warn) = app.status_line();
			let visual = app.visual_range();
			term.terminal.draw(|frame| {
				let [tree_area, status_area] =
					Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
				TreeView::render(frame, tree_area, &rows, app.cursor, &app.selection, visual);
				StatusBar::render(frame, status_area, &status, warn);

				if let Some(rename) = &mut rename {
					let (x, y) = Prompt::render(frame, frame.area(), "Rename", &mut rename.state);
					frame.set_cursor_position((x, y));
				}
			})?;

			// Cursor *shape* is a raw terminal escape, not something ratatui's
			// buffer diffing covers — set it after the frame's own writes are
			// flushed so it doesn't get interleaved with them. A bar in
			// Insert mirrors vim's editing feel; a block otherwise (Normal,
			// Visual, edtui's Search) makes clear you're issuing commands.
			if let Some(rename) = &rename {
				use crossterm::cursor::SetCursorStyle;
				let style = if rename.state.mode == EditorMode::Insert { SetCursorStyle::SteadyBar } else { SetCursorStyle::SteadyBlock };
				crossterm::execute!(io::stdout(), style)?;
			}

			app.rename = rename;
			Ok(())
		};

		draw(&mut app, &mut term)?;
		while let Some(event) = rx.recv().await {
			let event = match event {
				Event::Term(crossterm::event::Event::Key(key)) if key.kind == KeyEventKind::Press => {
					if app.rename.is_some() {
						Event::RenameKey(key)
					} else {
						match Router::route(key.code) {
							Some(event) => event,
							None => continue,
						}
					}
				}
				Event::Term(_) => continue,
				event => event,
			};

			Dispatcher::dispatch(&mut app, event);
			if app.quit {
				break;
			}
			draw(&mut app, &mut term)?;
		}

		Ok(())
	}

	pub fn visible(&self) -> Vec<(usize, &Node)> { self.tree.root.visible(0) }

	pub fn move_cursor(&mut self, delta: isize) {
		let len = self.visible().len();
		if len == 0 {
			return;
		}
		self.cursor = (self.cursor as isize + delta).clamp(0, len as isize - 1) as usize;
	}

	/// Marks the directory open immediately (the triangle flips, the row
	/// stays put) and, if it's never been listed, kicks off a background
	/// read — the listing lands later as a `Loaded` event instead of
	/// blocking this call.
	pub fn expand_selected(&mut self) {
		let Some(path) = self.selected_dir() else { return };
		let needs_fetch = self.tree.mark_expanded(&path).unwrap_or(false);
		self.watcher.watch(&path);
		if needs_fetch {
			self.scheduler.refresh(path);
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
		self.scheduler.forget(&target);
		self.select(&target);
	}

	pub fn toggle_selected(&mut self) {
		if let Some((_, node)) = self.visible().into_iter().nth(self.cursor) {
			self.selection.toggle(node.path.clone());
		}
		self.move_cursor(1);
	}

	pub fn enter_visual(&mut self, unset: bool) { self.visual = Some(Visual::new(self.cursor, unset)); }

	/// The live range between the visual anchor and the cursor — for the
	/// renderer to preview, before it's applied to the real selection.
	fn visual_range(&self) -> Option<(usize, usize, bool)> {
		let visual = self.visual?;
		let (lo, hi) = visual.range(self.cursor);
		Some((lo, hi, visual.unset))
	}

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

	/// The first press arms a delete of the current targets; a second press
	/// on the *same* targets confirms it. Anything that changes the targets
	/// in between (a different selection, a moved cursor) just re-arms
	/// instead of firing, so a stray keypress can never confirm a delete it
	/// didn't mean to. The actual removal runs in the background — cleanup
	/// (watcher/selection/refresh) happens once `Deleted` comes back.
	pub fn delete_selected(&mut self) {
		let targets = self.action_targets();
		if targets.is_empty() {
			self.pending_delete = None;
			return;
		}

		if self.pending_delete.as_deref() == Some(targets.as_slice()) {
			self.scheduler.delete(targets);
			self.pending_delete = None;
		} else {
			self.pending_delete = Some(targets);
		}
	}

	pub fn yank_selected(&mut self) { self.clipboard = self.action_targets(); }

	/// Copies the clipboard into the directory under the cursor in the
	/// background; the target's listing refreshes once `Pasted` comes back.
	pub fn paste(&mut self) {
		if self.clipboard.is_empty() {
			return;
		}
		let Some(target_dir) = self.paste_target() else { return };
		self.scheduler.copy(self.clipboard.clone(), target_dir);
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
		let name = node.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

		let mut state = EditorState::new(Lines::from(name.as_str()));
		state.set_single_line(true);
		state.mode = EditorMode::Insert;
		state.cursor = Index2::new(0, name.chars().count());

		self.rename = Some(Rename { target: node.path.clone(), state, handler: EditorEventHandler::vim_mode() });
	}

	/// Enter confirms; Esc from edtui's own Normal mode closes the prompt
	/// (Esc from Insert/Visual is forwarded instead, so edtui can drop it
	/// to Normal itself, vim-style); everything else is handed straight to
	/// edtui's own vim-modal key handler.
	pub fn handle_rename_key(&mut self, key: KeyEvent) {
		match key.code {
			KeyCode::Enter => self.confirm_rename(),
			KeyCode::Esc => {
				let Some(rename) = &mut self.rename else { return };
				if rename.state.mode != EditorMode::Normal {
					rename.handler.on_key_event(key, &mut rename.state);
					return;
				}
				self.rename = None;
			}
			// edtui's vim_mode doesn't bind `C` (vim's "change to end of
			// line") at all — synthesize it from what it does have: `D`
			// (delete to eol) followed by dropping straight into Insert,
			// appending right where the cut happened. Only in Normal mode;
			// typing a literal capital C elsewhere goes through untouched.
			KeyCode::Char('C') => {
				let Some(rename) = &mut self.rename else { return };
				if rename.state.mode != EditorMode::Normal {
					rename.handler.on_key_event(key, &mut rename.state);
					return;
				}
				// edtui's own uppercase-letter bindings (like this `D`) key
				// off the modifier flag, not just the letter's case — and
				// most terminals report Shift+<letter> as the already-
				// capitalized char with the modifier bit left unset, so it
				// has to be set explicitly here rather than forwarded from
				// whatever `key.modifiers` the incoming `C` carried.
				rename.handler.on_key_event(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT), &mut rename.state);
				// `D` leaves the Normal-mode cursor sitting *on* whatever's
				// now the last character (or col 0, on an emptied line) —
				// vim's `C` instead appends *after* it, so nudge to the
				// line's current length rather than just flipping the mode.
				rename.state.cursor.col = rename.state.lines.len_col(rename.state.cursor.row).unwrap_or(0);
				rename.state.mode = EditorMode::Insert;
			}
			_ => {
				let Some(rename) = &mut self.rename else { return };
				rename.handler.on_key_event(key, &mut rename.state);
			}
		}
	}

	pub fn confirm_rename(&mut self) {
		let Some(rename) = self.rename.take() else { return };
		let name: String = rename.state.lines.to_vecs().into_iter().next().unwrap_or_default().into_iter().collect();
		let Some(parent) = rename.target.parent() else { return };
		let dest = parent.join(&name);
		if name.is_empty() || dest == rename.target || std::fs::rename(&rename.target, &dest).is_err() {
			return;
		}

		self.watcher.unwatch(&rename.target);
		self.scheduler.forget(&rename.target);
		self.selection.remove(&rename.target);
		if self.tree.is_loaded(parent) {
			self.scheduler.refresh(parent.to_path_buf());
		}
	}

	/// Esc cancels whatever's most "in progress": an open visual selection
	/// (committing it), then an armed delete, then the selection. Reaching
	/// here at all means no rename prompt was open — while one is, Esc
	/// routes to `handle_rename_key` instead.
	pub fn escape(&mut self) {
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
			self.scheduler.refresh(path);
		}
	}

	/// A background listing finished. `accept` first checks it's still the
	/// most recent request for that path — a superseded one is discarded
	/// rather than clobbering a listing a newer request already applied.
	pub fn on_loaded(&mut self, path: PathBuf, ticket: u64, result: io::Result<Vec<(PathBuf, crate::fs::Cha)>>) {
		if !self.scheduler.accept(&path, ticket) {
			return;
		}
		if let Ok(entries) = result {
			self.tree.apply_listing(&path, entries);
		}
		self.clamp_cursor();
	}

	pub fn on_deleted(&mut self, paths: Vec<PathBuf>) {
		for path in &paths {
			self.watcher.unwatch(path);
			self.scheduler.forget(path);
			self.selection.remove(path);
		}
		self.refresh_parents(&paths);
	}

	pub fn on_pasted(&mut self, target: PathBuf) {
		if self.tree.is_loaded(&target) {
			self.scheduler.refresh(target);
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

	/// Where paste drops files: the directory under the cursor, or its
	/// parent if the cursor is on a file.
	fn paste_target(&self) -> Option<PathBuf> {
		let (_, node) = self.visible().into_iter().nth(self.cursor)?;
		if node.cha.is_dir { Some(node.path.clone()) } else { self.tree.parent_of(&node.path) }
	}

	fn refresh_parents(&mut self, paths: &[PathBuf]) {
		let mut parents: Vec<PathBuf> = paths.iter().filter_map(|p| self.tree.parent_of(p)).collect();
		parents.sort();
		parents.dedup();
		for parent in parents {
			if self.tree.is_loaded(&parent) {
				self.scheduler.refresh(parent);
			}
		}
	}

	fn clamp_cursor(&mut self) {
		let len = self.visible().len();
		if self.cursor >= len {
			self.cursor = len.saturating_sub(1);
		}
	}

	fn status_line(&self) -> (String, bool) {
		if let Some(visual) = self.visual {
			let label = if visual.unset { "VISUAL UNSET" } else { "VISUAL SELECT" };
			return (format!("-- {label} -- move to extend, Esc to apply"), true);
		}
		if let Some(pending) = &self.pending_delete {
			return (format!("Delete {} item(s)? Press d again to confirm, Esc to cancel", pending.len()), true);
		}
		if !self.selection.is_empty() {
			return (format!("{} selected", self.selection.len()), false);
		}
		if !self.clipboard.is_empty() {
			return (format!("{} in clipboard — p to paste", self.clipboard.len()), false);
		}
		("j/k move  h/l collapse/expand  space select  y/p yank/paste  d delete  q quit".to_owned(), false)
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use tokio::sync::mpsc;

	use super::*;
	use crate::event::Event;

	async fn app(root: &Path) -> (App, mpsc::UnboundedReceiver<Event>) {
		let mut tree = Tree::open(root.to_path_buf()).unwrap();
		let root_path = tree.root.path.clone();
		tree.mark_expanded(&root_path);

		let (tx, mut rx) = mpsc::unbounded_channel();
		let engine: Arc<dyn Engine> = Arc::new(LocalEngine);
		let mut scheduler = Scheduler::new(tx.clone(), engine);
		scheduler.refresh(root_path);

		let mut app = App {
			tree,
			cursor: 0,
			quit: false,
			watcher: Watcher::new(tx).unwrap(),
			scheduler,
			selection: Selection::default(),
			visual: None,
			clipboard: Vec::new(),
			pending_delete: None,
			rename: None,
		};

		let event = rx.recv().await.unwrap();
		Dispatcher::dispatch(&mut app, event);

		(app, rx)
	}

	async fn pump(app: &mut App, rx: &mut mpsc::UnboundedReceiver<Event>) {
		let event = rx.recv().await.unwrap();
		Dispatcher::dispatch(app, event);
	}

	fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, crossterm::event::KeyModifiers::NONE) }

	fn rename_value(app: &App) -> String {
		app.rename.as_ref().unwrap().state.lines.to_vecs().into_iter().next().unwrap_or_default().into_iter().collect()
	}

	#[tokio::test]
	async fn collapse_on_the_open_directory_itself() {
		let root = std::env::temp_dir().join("tuzi-app-test-self");
		fs::create_dir_all(root.join("a/b")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.move_cursor(1); // onto "a"
		app.expand_selected(); // flips `expanded` immediately, no need to wait on the fetch
		app.collapse_selected();

		let a = app.tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("a")).unwrap();
		assert!(!a.expanded);
		assert_eq!(app.visible()[app.cursor].1.path, root.join("a"), "cursor stays on a");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn collapse_from_a_child_jumps_to_and_closes_the_parent() {
		let root = std::env::temp_dir().join("tuzi-app-test-child");
		fs::create_dir_all(root.join("a/b")).unwrap();
		fs::write(root.join("a/leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.move_cursor(1); // onto "a"
		app.expand_selected();
		pump(&mut app, &mut rx).await; // wait for "a"'s listing so it actually has visible children
		app.move_cursor(1); // onto "a/b" or "a/leaf.txt", a's first child

		app.collapse_selected();

		let a = app.tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("a")).unwrap();
		assert!(!a.expanded, "collapsing from a child closes its parent");
		assert_eq!(app.visible()[app.cursor].1.path, root.join("a"), "cursor jumps to the parent");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn toggle_select_marks_the_row_and_moves_on() {
		let root = std::env::temp_dir().join("tuzi-app-test-select");
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("z")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.move_cursor(1); // onto "a" (sorted before "z")
		app.toggle_selected();

		assert!(app.selection.contains(&root.join("a")));
		assert_eq!(app.cursor, 2, "toggling moves on, like holding space to select a run");

		app.move_cursor(-1); // back onto "a"
		app.toggle_selected();
		assert!(!app.selection.contains(&root.join("a")), "toggling again clears it");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn delete_needs_a_second_press_on_the_same_target() {
		let root = std::env::temp_dir().join("tuzi-app-test-delete");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.move_cursor(1); // onto "leaf.txt"

		app.delete_selected();
		assert!(root.join("leaf.txt").exists(), "first press only arms it");
		assert!(app.pending_delete.is_some());

		app.delete_selected();
		pump(&mut app, &mut rx).await; // wait for the background removal to actually land
		assert!(!root.join("leaf.txt").exists(), "second press on the same target confirms it");
		assert!(app.pending_delete.is_none());

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn escape_cancels_a_pending_delete_before_touching_selection() {
		let root = std::env::temp_dir().join("tuzi-app-test-escape");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.move_cursor(1); // onto "leaf.txt"
		app.toggle_selected();
		app.move_cursor(-1);
		app.delete_selected(); // arms, targeting the selection
		assert!(app.pending_delete.is_some());

		app.escape();
		assert!(app.pending_delete.is_none(), "escape cancels the pending delete first");
		assert!(app.selection.contains(&root.join("leaf.txt")), "…without touching the selection yet");

		app.escape();
		assert!(app.selection.is_empty(), "a second escape then clears the selection");
		assert!(root.join("leaf.txt").exists(), "nothing was ever actually deleted");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn yank_then_paste_copies_into_the_target_directory() {
		let root = std::env::temp_dir().join("tuzi-app-test-paste");
		fs::create_dir_all(root.join("src")).unwrap();
		fs::create_dir_all(root.join("dst")).unwrap();
		fs::write(root.join("src/leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.move_cursor(1); // onto "dst" or "src", sorted alphabetically: dst, src
		app.move_cursor(1); // onto "src"
		app.expand_selected();
		pump(&mut app, &mut rx).await; // src.children = [leaf.txt]
		app.move_cursor(1); // onto "src/leaf.txt"
		app.yank_selected();
		assert_eq!(app.clipboard, vec![root.join("src/leaf.txt")]);

		app.move_cursor(-2); // back onto "dst"
		app.expand_selected();
		pump(&mut app, &mut rx).await; // dst.children = []

		app.paste();
		pump(&mut app, &mut rx).await; // Pasted(dst) -> requests a fresh listing
		pump(&mut app, &mut rx).await; // Loaded(dst) -> dst.children now includes leaf.txt

		assert!(root.join("dst/leaf.txt").exists());
		let dst = app.tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("dst")).unwrap();
		assert!(dst.children.as_ref().unwrap().iter().any(|n| n.path == root.join("dst/leaf.txt")));

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn visual_select_commits_the_range_on_escape() {
		let root = std::env::temp_dir().join("tuzi-app-test-visual-select");
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("b")).unwrap();
		fs::create_dir_all(root.join("c")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.move_cursor(1); // onto "a"
		app.enter_visual(false);
		app.move_cursor(1); // onto "b" — range is now a..=b
		assert_eq!(app.visual_range(), Some((1, 2, false)), "preview covers a and b, not c");

		app.escape();
		assert!(app.visual.is_none());
		assert!(app.selection.contains(&root.join("a")));
		assert!(app.selection.contains(&root.join("b")));
		assert!(!app.selection.contains(&root.join("c")), "outside the range, untouched");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn visual_unset_clears_only_the_range_it_covers() {
		let root = std::env::temp_dir().join("tuzi-app-test-visual-unset");
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("b")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.selection.insert(root.join("a"));
		app.selection.insert(root.join("b"));

		app.move_cursor(1); // onto "a"
		app.enter_visual(true); // VISUAL UNSET
		app.escape(); // range is just "a" (cursor never moved off it)

		assert!(!app.selection.contains(&root.join("a")), "unset over a clears it");
		assert!(app.selection.contains(&root.join("b")), "b was outside the range");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn rename_confirm_renames_on_disk_and_refreshes_the_parent() {
		let root = std::env::temp_dir().join("tuzi-app-test-rename");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("old.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.move_cursor(1); // onto "old.txt"

		app.start_rename();
		assert_eq!(rename_value(&app), "old.txt", "prefilled with the current name");
		assert!(app.rename.as_ref().unwrap().state.mode == EditorMode::Insert);

		// still in Insert mode (cursor at the end): clear the prefill and
		// type the new name, one raw key event at a time through edtui.
		for _ in 0..7 {
			app.handle_rename_key(key(KeyCode::Backspace));
		}
		for c in "new.txt".chars() {
			app.handle_rename_key(key(KeyCode::Char(c)));
		}
		assert_eq!(rename_value(&app), "new.txt");

		app.handle_rename_key(key(KeyCode::Enter)); // confirm
		assert!(app.rename.is_none());
		pump(&mut app, &mut rx).await; // parent's listing refreshes

		assert!(!root.join("old.txt").exists());
		assert!(root.join("new.txt").exists());
		let renamed = app.tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("new.txt"));
		assert!(renamed.is_some(), "tree reflects the new name after the refresh lands");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn rename_escape_from_insert_stays_open_then_cancels() {
		let root = std::env::temp_dir().join("tuzi-app-test-rename-escape");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("keep.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.move_cursor(1); // onto "keep.txt"
		app.start_rename();

		app.handle_rename_key(key(KeyCode::Esc)); // Insert -> Normal, handled by edtui itself
		assert!(app.rename.is_some(), "first escape only drops to normal mode");
		assert!(app.rename.as_ref().unwrap().state.mode == EditorMode::Normal);

		app.handle_rename_key(key(KeyCode::Esc)); // Normal -> we intercept and close
		assert!(app.rename.is_none());
		assert!(root.join("keep.txt").exists(), "nothing was renamed");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn capital_c_changes_from_the_cursor_to_the_end_of_line() {
		let root = std::env::temp_dir().join("tuzi-app-test-rename-change");
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("keep.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.move_cursor(1); // onto "keep.txt"
		app.start_rename();

		app.handle_rename_key(key(KeyCode::Esc)); // Insert -> Normal, cursor lands on the last char ('t')
		app.handle_rename_key(key(KeyCode::Char('0'))); // BOL
		// edtui's `w` treats punctuation as its own word class, matching
		// real vim: "keep" | "." | "txt" is three words, not one.
		app.handle_rename_key(key(KeyCode::Char('w'))); // -> the '.'
		app.handle_rename_key(key(KeyCode::Char('w'))); // -> start of "txt"

		// edtui's vim_mode has no native `C` binding at all — this is our
		// own synthesized delete-to-eol-then-insert.
		app.handle_rename_key(key(KeyCode::Char('C')));
		assert_eq!(rename_value(&app), "keep.");
		assert!(app.rename.as_ref().unwrap().state.mode == EditorMode::Insert, "C drops straight into Insert");

		for c in "md".chars() {
			app.handle_rename_key(key(KeyCode::Char(c)));
		}
		assert_eq!(rename_value(&app), "keep.md");

		fs::remove_dir_all(&root).unwrap();
	}
}
