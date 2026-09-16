use std::{collections::VecDeque, env, io, path::PathBuf};

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::{event::Event, icon::IconTheme, keymap::{Key, KeyContext, Route, Router, WhichCandidate}, opener::OpenPicker, process::ProcessRequest, scheduler::OpenScheduler, tasks::{TaskEvent, TaskManager}, tui::TerminalSession};

use super::{Dispatcher, Tab};

pub struct App {
	pub tabs:    Vec<Tab>,
	pub active:  usize,
	pub quit:    bool,
	next_tab_id: usize,
	/// The yanked files, shared by every tab: yank in one, paste in another.
	pub(super) clipboard:     Vec<PathBuf>,
	pub(super) clipboard_cut: bool,
	pub(super) tree_rows:  usize,
	pub(super) which:      Vec<WhichCandidate>,
	pub(super) icon_theme: IconTheme,
	pub(super) open:        OpenScheduler,
	pub(super) open_picker: Option<OpenPicker>,
	pub(super) processes:   VecDeque<ProcessRequest>,
	pub(super) tx:         mpsc::UnboundedSender<Event>,
	pub tasks:             TaskManager,
}

impl App {
	pub async fn serve() -> io::Result<()> {
		let (tx, mut rx) = mpsc::unbounded_channel();

		let first = Tab::open(0, env::current_dir()?, tx.clone())?;
		let mut app = Self {
			tabs: vec![first], active: 0, quit: false, next_tab_id: 1,
			clipboard: Vec::new(), clipboard_cut: false,
			tree_rows: 0, which: Vec::new(), icon_theme: IconTheme,
			open: OpenScheduler::new(tx.clone()), open_picker: None, processes: VecDeque::new(), tasks: TaskManager::new(tx.clone()), tx,
		};
		let mut terminal = TerminalSession::start()?;
		let mut router = Router::default();

		app.render(terminal.terminal())?;
		loop {
			let event = tokio::select! {
				biased;
				term_event = terminal.next_event() => match term_event? {
					Some(event) => Event::Term(event),
					None => break,
				},
				background_event = rx.recv() => match background_event {
					Some(event) => event,
					None => break,
				},
			};
			if !app.handle_event(event, &mut router) {
				continue;
			}
			if app.quit {
				break;
			}
			app.run_pending_processes(&mut terminal).await?;
			app.render(terminal.terminal())?;
		}

		Ok(())
	}

	fn handle_event(&mut self, event: Event, router: &mut Router) -> bool {
		match event {
			Event::Term(crossterm::event::Event::Key(key)) if key.kind == KeyEventKind::Press => self.handle_key(key, router),
			Event::Term(crossterm::event::Event::Resize(_, _)) => true,
			Event::Term(_) => false,
			event => {
				Dispatcher::dispatch_event(self, event);
				true
			}
		}
	}

	fn handle_key(&mut self, key: crossterm::event::KeyEvent, router: &mut Router) -> bool {
		if self.tasks.visible {
			return match key.code {
				KeyCode::Up | KeyCode::Char('k') => { self.tasks.move_cursor(-1); true }
				KeyCode::Down | KeyCode::Char('j') => { self.tasks.move_cursor(1); true }
				KeyCode::Char('x') => { self.tasks.cancel_selected(); true }
				KeyCode::Esc | KeyCode::Char('w') | KeyCode::Char('q') => { self.tasks.visible = false; true }
				_ => false,
			};
		}
		if self.open_picker.is_some() {
			return match key.code {
				KeyCode::Up | KeyCode::Char('k') => { self.move_open_picker(-1); true }
				KeyCode::Down | KeyCode::Char('j') => { self.move_open_picker(1); true }
				KeyCode::Enter => { self.submit_open_picker(); true }
				KeyCode::Esc | KeyCode::Char('q') => { self.open_picker = None; true }
				_ => false,
			};
		}
		if self.active_tab().pending_delete.is_some() {
			let submit = match key.code {
				KeyCode::Char('y') => Some(true),
				KeyCode::Enter | KeyCode::Esc | KeyCode::Char('n') => Some(false),
				KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(false),
				_ => None,
			};
			let Some(submit) = submit else { return false };
			self.active_tab_mut().confirm_delete(submit);
			return true;
		}
		if self.active_tab().input.is_some() {
			self.active_tab_mut().handle_input_key(key);
			return true;
		}

		match router.route(KeyContext::Manager, Key::from(key)) {
			Route::Actions(actions) => {
				self.which.clear();
				for action in actions {
					Dispatcher::dispatch(self, action);
				}
				true
			}
			Route::Pending(candidates) => { self.which = candidates; true }
			Route::Unmatched if self.which.is_empty() => false,
			Route::Unmatched => { self.which.clear(); true }
		}
	}

	async fn run_pending_processes(&mut self, terminal: &mut TerminalSession) -> io::Result<()> {
		while let Some(request) = self.processes.pop_front() {
			let blocking = request.mode().blocks_terminal();
			if blocking {
				terminal.suspend();
			}
			let completion = request.execute().await;
			if blocking {
				terminal.resume()?;
			}
			self.on_process_completion(completion);
		}
		Ok(())
	}

	pub(super) fn active_tab(&self) -> &Tab {
		self.tabs.iter().find(|t| t.id == self.active).expect("active always names an existing tab")
	}

	pub(super) fn active_tab_mut(&mut self) -> &mut Tab {
		self.tabs.iter_mut().find(|t| t.id == self.active).expect("active always names an existing tab")
	}

	/// For routing a background event tagged with a tab id — unlike
	/// `active_tab_mut`, `None` is a normal outcome (the tab it was headed
	/// for closed before the event arrived), not a bug.
	pub(super) fn tab_mut(&mut self, id: usize) -> Option<&mut Tab> { self.tabs.iter_mut().find(|t| t.id == id) }

	/// Opens a new tab rooted at wherever the active one currently is,
	/// yazi-style (`tt`), and switches to it.
	pub fn new_tab(&mut self) {
		let path = self.active_tab().tree.root.path.clone();
		let id = self.next_tab_id;
		self.next_tab_id += 1;
		if let Ok(tab) = Tab::open(id, path, self.tx.clone()) {
			self.tabs.push(tab);
			self.active = id;
		}
	}

	/// Closes the active tab and lands on whichever one now sits in its
	/// place (or the last tab, if it was the rightmost). Refuses to close
	/// the last remaining tab — there's always at least one.
	pub fn close_tab(&mut self) {
		if self.tabs.len() <= 1 {
			return;
		}
		let Some(pos) = self.tabs.iter().position(|t| t.id == self.active) else { return };
		self.tabs.remove(pos);
		self.active = self.tabs[pos.min(self.tabs.len() - 1)].id;
	}

	pub fn switch_tab(&mut self, delta: isize) {
		let Some(pos) = self.tabs.iter().position(|t| t.id == self.active) else { return };
		let next = (pos as isize + delta).rem_euclid(self.tabs.len() as isize) as usize;
		self.active = self.tabs[next].id;
	}

	pub fn move_page(&mut self, percent: i8) {
		let rows = self.tree_rows.max(1) as isize;
		let mut delta = rows * percent as isize / 100;
		if delta == 0 && percent != 0 {
			delta = percent.signum() as isize;
		}
		self.active_tab_mut().move_cursor(delta);
	}

	/// Yanks the active tab's targets onto the shared clipboard, so any tab
	/// can paste them afterward.
	pub fn yank_selected(&mut self, cut: bool) {
		let targets = self.active_tab_mut().take_yank_targets();
		if targets.is_empty() {
			return;
		}
		self.clipboard = targets;
		self.clipboard_cut = cut;
	}

	/// Pastes the shared clipboard into the active tab. A cut clipboard is
	/// consumed on the first successful paste; a copy can be pasted again.
	pub fn paste(&mut self) {
		if self.clipboard.is_empty() {
			return;
		}
		let paths = self.clipboard.clone();
		let cut = self.clipboard_cut;
		let tab = self.active;
		let Some(target) = self.active_tab().paste_destination() else { return };
		self.tasks.enqueue(paths, target, cut, tab);
		if cut {
			self.clipboard.clear();
			self.clipboard_cut = false;
		}
	}

	pub(super) fn on_task_event(&mut self, event: TaskEvent) {
		if let Some((tab, target)) = self.tasks.accept(event)
			&& let Some(tab) = self.tab_mut(tab) {
			tab.on_pasted(target);
		}
	}
}

#[cfg(test)]
mod tests {
	use std::{fs, path::Path};

	use crate::{action::Action, column_mode::ColumnMode};

	use super::*;

	async fn app(root: &Path) -> (App, mpsc::UnboundedReceiver<Event>) {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let first = Tab::open(0, root.to_path_buf(), tx.clone()).unwrap();
		let mut app = App {
			tabs: vec![first], active: 0, quit: false, next_tab_id: 1,
			clipboard: Vec::new(), clipboard_cut: false,
			tree_rows: 0, which: Vec::new(), icon_theme: IconTheme,
			open: OpenScheduler::new(tx.clone()), open_picker: None, processes: VecDeque::new(), tasks: TaskManager::new(tx.clone()), tx,
		};

		// drain the root tab's initial listing so it's got visible rows
		let event = rx.recv().await.unwrap();
		Dispatcher::dispatch_event(&mut app, event);

		(app, rx)
	}

	async fn pump(app: &mut App, rx: &mut mpsc::UnboundedReceiver<Event>) {
		loop {
			let event = rx.recv().await.unwrap();
			let task_finished = matches!(&event, Event::Task(TaskEvent::Finished { .. }));
			let task_event = matches!(&event, Event::Task(_));
			Dispatcher::dispatch_event(app, event);
			if !task_event || task_finished {
				break;
			}
		}
	}

	#[tokio::test]
	async fn yank_then_paste_copies_into_the_cursors_parent_directory() {
		let root = std::env::temp_dir().join("tuzi-app-test-paste");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("src")).unwrap();
		fs::create_dir_all(root.join("dst/sub")).unwrap();
		fs::write(root.join("src/leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.active_tab_mut().move_cursor(1); // onto "dst" or "src", sorted alphabetically: dst, src
		app.active_tab_mut().move_cursor(1); // onto "src"
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // src.children = [leaf.txt]
		app.active_tab_mut().move_cursor(1); // onto "src/leaf.txt"
		app.active_tab_mut().selection.insert(root.join("src/leaf.txt"));
		app.yank_selected(false);
		assert_eq!(app.clipboard, vec![root.join("src/leaf.txt")]);
		assert!(app.active_tab().selection.is_empty(), "yanking converts selected markers into clipboard markers");

		app.active_tab_mut().move_cursor(-2); // back onto "dst"
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // dst.children = [sub]
		app.active_tab_mut().move_cursor(1); // onto "dst/sub", itself a directory

		app.paste();
		pump(&mut app, &mut rx).await; // Pasted(dst) -> requests a fresh listing
		pump(&mut app, &mut rx).await; // Loaded(dst) -> dst.children now includes leaf.txt

		// The cursor sat on "sub", but the file lands in "sub"'s parent,
		// "dst" — not inside "sub" itself.
		assert!(root.join("dst/leaf.txt").exists());
		assert!(!root.join("dst/sub/leaf.txt").exists());
		assert_eq!(app.clipboard, vec![root.join("src/leaf.txt")], "a copy stays on the clipboard for another paste");
		let dst = app.active_tab().tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("dst")).unwrap();
		assert!(dst.children.as_ref().unwrap().iter().any(|n| n.path == root.join("dst/leaf.txt")));

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn cut_then_paste_moves_the_file_and_clears_the_marker_state() {
		let root = std::env::temp_dir().join("tuzi-app-test-cut-paste");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("dst/sub")).unwrap();
		fs::write(root.join("source.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.clipboard = vec![root.join("source.txt")];
		app.clipboard_cut = true;
		app.active_tab_mut().move_cursor(1); // dst
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // dst.children = [sub]
		app.active_tab_mut().move_cursor(1); // onto "dst/sub"
		app.paste();
		pump(&mut app, &mut rx).await;

		assert!(!root.join("source.txt").exists());
		assert!(root.join("dst/source.txt").exists());
		assert!(app.clipboard.is_empty());
		assert!(!app.clipboard_cut);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn yanking_in_one_tab_pastes_in_another() {
		let root = std::env::temp_dir().join("tuzi-app-test-cross-tab-paste");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("src")).unwrap();
		fs::create_dir_all(root.join("dst/sub")).unwrap();
		fs::write(root.join("src/leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.active_tab_mut().move_cursor(2); // onto "src" (dst, src sorted alphabetically)
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // src.children = [leaf.txt]
		app.active_tab_mut().move_cursor(1); // onto "src/leaf.txt"
		app.yank_selected(false);
		assert_eq!(app.clipboard, vec![root.join("src/leaf.txt")]);

		app.new_tab(); // a second tab, also rooted at `root`
		pump(&mut app, &mut rx).await; // its own initial listing
		assert_eq!(app.clipboard, vec![root.join("src/leaf.txt")], "the clipboard isn't tied to the tab that filled it");

		app.active_tab_mut().move_cursor(1); // onto "dst" in the new tab
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // dst.children = [sub]
		app.active_tab_mut().move_cursor(1); // onto "dst/sub"
		app.paste();
		pump(&mut app, &mut rx).await; // Pasted(dst) -> requests a fresh listing
		pump(&mut app, &mut rx).await; // Loaded(dst) -> dst.children now includes leaf.txt

		assert!(root.join("dst/leaf.txt").exists(), "pasting in a different tab than the one that yanked should still work");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn new_tab_opens_at_the_same_directory_and_becomes_active() {
		let root = std::env::temp_dir().join("tuzi-app-test-newtab");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.new_tab();

		assert_eq!(app.tabs.len(), 2);
		assert_eq!(app.active, 1, "the new tab's id, not a vec index");
		assert_eq!(app.active_tab().tree.root.path, root);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn closing_the_active_tab_lands_on_a_stable_id_not_a_shifted_index() {
		let root = std::env::temp_dir().join("tuzi-app-test-closetab");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await; // tab 0
		app.new_tab(); // tab 1, active
		app.new_tab(); // tab 2, active
		assert_eq!(app.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), [0, 1, 2]);

		app.close_tab(); // closes tab 2 (active)
		assert_eq!(app.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), [0, 1]);
		assert_eq!(app.active, 1, "lands on the tab that's now in tab 2's old spot");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn close_tab_refuses_to_close_the_last_one() {
		let root = std::env::temp_dir().join("tuzi-app-test-lasttab");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.close_tab();
		assert_eq!(app.tabs.len(), 1, "always at least one tab left");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn relative_tab_switch_wraps_around() {
		let root = std::env::temp_dir().join("tuzi-app-test-cycletab");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await; // 0
		app.new_tab(); // 1
		app.new_tab(); // 2
		app.active = 0;

		app.switch_tab(1);
		assert_eq!(app.active, 1);
		app.switch_tab(1);
		assert_eq!(app.active, 2);
		app.switch_tab(1);
		assert_eq!(app.active, 0, "wraps back to the first");

		app.switch_tab(-1);
		assert_eq!(app.active, 2, "wraps the other way too");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn each_tab_keeps_its_own_column_mode() {
		let root = std::env::temp_dir().join("tuzi-app-test-column-mode");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		Dispatcher::dispatch(&mut app, Action::SetColumnMode(ColumnMode::Size));
		app.new_tab();
		assert_eq!(app.active_tab().column_mode, ColumnMode::None, "new tabs start with the default mode");

		Dispatcher::dispatch(&mut app, Action::SetColumnMode(ColumnMode::Permissions));
		app.switch_tab(-1);
		assert_eq!(app.active_tab().column_mode, ColumnMode::Size);
		app.switch_tab(1);
		assert_eq!(app.active_tab().column_mode, ColumnMode::Permissions);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn preview_visibility_is_off_by_default_and_local_to_each_tab() {
		let root = std::env::temp_dir().join("tuzi-app-test-preview-visibility");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		assert!(!app.active_tab().preview.visible);
		Dispatcher::dispatch(&mut app, Action::TogglePreview);
		assert!(app.active_tab().preview.visible);

		app.new_tab();
		assert!(!app.active_tab().preview.visible, "new tabs hide preview by default");
		app.switch_tab(-1);
		assert!(app.active_tab().preview.visible, "each tab retains its own preview setting");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn fzf_directory_changes_root_and_nested_file_is_revealed_lazily() {
		let root = std::env::temp_dir().join("tuzi-app-test-fzf");
		let nested = root.join("a/b");
		fs::create_dir_all(&nested).unwrap();
		fs::write(nested.join("target.txt"), b"target").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.apply_fzf_output(0, &root, false, b"a/b/target.txt\n");
		for _ in 0..4 {
			if app.active_tab().visible()[app.active_tab().cursor].1.path == root.join("a/b/target.txt") {
				break;
			}
			let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
			Dispatcher::dispatch_event(&mut app, event);
		}
		assert_eq!(app.active_tab().visible()[app.active_tab().cursor].1.path, root.join("a/b/target.txt"));

		app.apply_fzf_output(0, &root, false, b"a\n");
		assert_eq!(app.active_tab().tree.root.path, root.join("a"));
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn page_and_absolute_movement_use_visible_rows_and_clamp() {
		let root = std::env::temp_dir().join("tuzi-app-test-page-movement");
		fs::create_dir_all(&root).unwrap();
		for index in 0..10 {
			fs::create_dir_all(root.join(format!("dir-{index}"))).unwrap();
		}
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.tree_rows = 4;

		app.move_page(50);
		assert_eq!(app.active_tab().cursor, 2);
		app.move_page(100);
		assert_eq!(app.active_tab().cursor, 6);
		app.move_page(-50);
		assert_eq!(app.active_tab().cursor, 4);
		app.move_page(-100);
		assert_eq!(app.active_tab().cursor, 0, "movement clamps at the first row");

		app.move_page(100);
		app.move_page(100);
		app.move_page(100);
		assert_eq!(app.active_tab().cursor, 10, "movement clamps at the last row");

		app.active_tab_mut().move_to_top();
		assert_eq!(app.active_tab().cursor, 0);
		app.active_tab_mut().move_to_bottom();
		assert_eq!(app.active_tab().cursor, 10);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn background_events_route_to_the_tab_that_requested_them_even_when_inactive() {
		let root = std::env::temp_dir().join("tuzi-app-test-route");
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("b")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await; // tab 0, rooted at `root`
		app.new_tab(); // tab 1, also rooted at `root` (new_tab reuses the active dir)
		assert_eq!(app.active, 1);

		// tab 1's own initial listing is already in flight; drain it before
		// triggering the actual scenario below, so it can't race with (and
		// get received ahead of) the event this test cares about.
		let event = rx.recv().await.unwrap();
		assert!(matches!(event, Event::Loaded { tab: 1, .. }), "tab 1's own startup load");
		Dispatcher::dispatch_event(&mut app, event);

		// Expand a directory on the *inactive* tab 0 and route the
		// resulting Loaded event straight through Dispatcher, the way the
		// real event loop would — not by calling tab methods directly. Both
		// tabs share the same channel `app` was built with, so this is the
		// exact path a real background load takes.
		let path = root.join("a");
		app.tab_mut(0).unwrap().tree.mark_expanded(&path);
		app.tab_mut(0).unwrap().fs_scheduler.refresh(path);

		let event = rx.recv().await.unwrap();
		let tab = match &event {
			Event::Loaded { tab, .. } => *tab,
			_ => panic!("expected Loaded"),
		};
		assert_eq!(tab, 0, "tagged with the tab that asked for it, not whichever tab is active");

		Dispatcher::dispatch_event(&mut app, event);
		assert_eq!(app.active, 1, "dispatching a background event never changes which tab is active");
		assert!(app.tab_mut(0).unwrap().tree.is_loaded(&root.join("a")), "but it still lands on the tab it was meant for");

		fs::remove_dir_all(&root).unwrap();
	}
}
