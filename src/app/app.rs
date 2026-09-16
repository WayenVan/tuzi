use std::{cell::Cell, env, io, thread};

use crossterm::event::KeyEventKind;
use edtui::EditorMode;
use ratatui::layout::{Constraint, Direction, Layout};
use tokio::sync::mpsc;

use crate::{event::Event, keymap::{Key, KeyContext, Route, Router, WhichCandidate}, preview::PreviewTarget, tui::{Raterm, widgets::{CompletionPopup, PreviewView, Prompt, StatusBar, TabBar, TreeView, WhichPopup, WinBar}}};

use super::{Dispatcher, Tab};

pub struct App {
	pub tabs:    Vec<Tab>,
	pub active:  usize,
	pub quit:    bool,
	next_tab_id: usize,
	tree_rows:   usize,
	which:       Vec<WhichCandidate>,
	tx:          mpsc::UnboundedSender<Event>,
}

impl App {
	pub async fn serve() -> io::Result<()> {
		let (tx, mut rx) = mpsc::unbounded_channel();

		// Raw terminal events are wrapped, not translated, here — the
		// background thread doesn't know whether an input prompt is open,
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

		let first = Tab::open(0, env::current_dir()?, tx.clone())?;
		let mut app = Self { tabs: vec![first], active: 0, quit: false, next_tab_id: 1, tree_rows: 0, which: Vec::new(), tx };
		let mut term = Raterm::start()?;
		let mut router = Router::default();

		let draw = |app: &mut App, term: &mut Raterm| -> io::Result<()> {
			let tree_rows = Cell::new(app.tree_rows);
			let preview_size = Cell::new((0, 0));
			let redraw_tx = app.tx.clone();
			let which = app.which.clone();
			let cwd = app.active_tab().tree.root.path.clone();
			// Collected as owned strings *before* grabbing the active tab
			// mutably below — otherwise the tab bar's shared borrow of
			// every tab and the active tab's exclusive borrow (needed for
			// the input take/put-back trick) would overlap.
			let labels: Vec<(bool, String)> = app
				.tabs
				.iter()
				.map(|t| {
					let name =
						t.tree.root.path.file_name().map_or_else(|| t.tree.root.path.display().to_string(), |n| n.to_string_lossy().into_owned());
					(t.id == app.active, name)
				})
				.collect();

			let tab = app.active_tab_mut();
			// Taken out (and put back at the end) so that its `&mut` doesn't
			// overlap, for the borrow checker's purposes, with the `&Node`s
			// `tab.visible()` lends out below — both ultimately borrow from
			// `tab` through `&self` methods, which erases field-level
			// disjointness even though `input` and `tree` never actually
			// touch each other.
			let mut input = tab.input.take();

			let rows = tab.visible();
			let (mut status, mut warn) = tab.status_line();
			if let Some(error) = input.as_ref().and_then(|input| input.error.as_ref()) {
				status = error.clone();
				warn = true;
			}
			let visual = tab.visual_range();
			let column_mode = tab.column_mode;
			let preview_visible = tab.preview.visible;
			let preview_target = rows.get(tab.cursor).map(|(_, node)| PreviewTarget::from_node(node));
			term.terminal.draw(|frame| {
				let [win_area, tab_area, body_area, status_area] = Layout::default()
					.direction(Direction::Vertical)
					.constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
					.areas(frame.area());
				let (tree_area, preview_area) = if preview_visible {
					let [tree, preview] = Layout::default()
						.direction(Direction::Horizontal)
						.constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
						.areas(body_area);
					(tree, Some(preview))
				} else {
					(body_area, None)
				};
				tree_rows.set(tree_area.height as usize);

				WinBar::render(frame, win_area, &cwd);
				TabBar::render(frame, tab_area, &labels);
				TreeView::render(frame, tree_area, &rows, tab.cursor, &tab.selection, visual, column_mode);
				if let Some(area) = preview_area {
					preview_size.set((area.width.saturating_sub(1), area.height));
					PreviewView::render(
						frame,
						area,
						rows.get(tab.cursor).map(|(_, node)| *node),
						&tab.preview.state,
						tab.preview.skip,
					);
				}
				StatusBar::render(frame, status_area, &status, warn);
				WhichPopup::render(frame, frame.area(), &which);

				if let Some(input) = &mut input {
					let title = input.title();
					let (x, y, rect) = Prompt::render(frame, frame.area(), title, &mut input.state);
					if let Some(cmp) = &input.completion {
						CompletionPopup::render(frame, frame.area(), rect, &cmp.candidates, cmp.selected);
					}
					frame.set_cursor_position((x, y));
				}
			})?;
			let (preview_width, preview_height) = preview_size.get();
			if tab.preview.sync(preview_target, preview_width, preview_height) {
				let _ = redraw_tx.send(Event::Redraw);
			}

			// Cursor *shape* is a raw terminal escape, not something ratatui's
			// buffer diffing covers — set it after the frame's own writes are
			// flushed so it doesn't get interleaved with them. A bar in
			// Insert mirrors vim's editing feel; a block otherwise (Normal,
			// Visual, edtui's Search) makes clear you're issuing commands.
			if let Some(input) = &input {
				use crossterm::cursor::SetCursorStyle;
				let style = if input.state.mode == EditorMode::Insert { SetCursorStyle::SteadyBar } else { SetCursorStyle::SteadyBlock };
				crossterm::execute!(io::stdout(), style)?;
			}

			tab.input = input;
			app.tree_rows = tree_rows.get();
			Ok(())
		};

		draw(&mut app, &mut term)?;
		while let Some(event) = rx.recv().await {
			match event {
				Event::Term(crossterm::event::Event::Key(key)) if key.kind == KeyEventKind::Press => {
					if app.active_tab().input.is_some() {
						app.active_tab_mut().handle_input_key(key);
					} else {
						match router.route(KeyContext::Manager, Key::from(key)) {
							Route::Actions(actions) => {
								app.which.clear();
								for action in actions {
									Dispatcher::dispatch(&mut app, action);
								}
							}
							Route::Pending(candidates) => app.which = candidates,
							Route::Unmatched if app.which.is_empty() => continue,
							Route::Unmatched => app.which.clear(),
						}
					}
				}
				Event::Term(crossterm::event::Event::Resize(_, _)) => {}
				Event::Term(_) => continue,
				event => Dispatcher::dispatch_event(&mut app, event),
			}
			if app.quit {
				break;
			}
			draw(&mut app, &mut term)?;
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
}

#[cfg(test)]
mod tests {
	use std::{fs, path::Path};

	use crate::{action::Action, column_mode::ColumnMode};

	use super::*;

	async fn app(root: &Path) -> (App, mpsc::UnboundedReceiver<Event>) {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let first = Tab::open(0, root.to_path_buf(), tx.clone()).unwrap();
		let mut app = App { tabs: vec![first], active: 0, quit: false, next_tab_id: 1, tree_rows: 0, which: Vec::new(), tx };

		// drain the root tab's initial listing so it's got visible rows
		let event = rx.recv().await.unwrap();
		Dispatcher::dispatch_event(&mut app, event);

		(app, rx)
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
