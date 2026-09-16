use std::{env, io, thread};

use crossterm::event::KeyEventKind;
use edtui::EditorMode;
use ratatui::layout::{Constraint, Direction, Layout};
use tokio::sync::mpsc;

use crate::{event::Event, tui::{Raterm, widgets::{Prompt, StatusBar, TabBar, TreeView}}};

use super::{Dispatcher, Router, Tab};

pub struct App {
	pub tabs:    Vec<Tab>,
	pub active:  usize,
	pub quit:    bool,
	next_tab_id: usize,
	tx:          mpsc::UnboundedSender<Event>,
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

		let first = Tab::open(0, env::current_dir()?, tx.clone())?;
		let mut app = Self { tabs: vec![first], active: 0, quit: false, next_tab_id: 1, tx };
		let mut term = Raterm::start()?;
		let mut router = Router::default();

		let draw = |app: &mut App, term: &mut Raterm| -> io::Result<()> {
			// Collected as owned strings *before* grabbing the active tab
			// mutably below — otherwise the tab bar's shared borrow of
			// every tab and the active tab's exclusive borrow (needed for
			// the rename take/put-back trick) would overlap.
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
			// disjointness even though `rename` and `tree` never actually
			// touch each other.
			let mut rename = tab.rename.take();

			let rows = tab.visible();
			let (status, warn) = tab.status_line();
			let visual = tab.visual_range();
			term.terminal.draw(|frame| {
				let [tab_area, tree_area, status_area] = Layout::default()
					.direction(Direction::Vertical)
					.constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
					.areas(frame.area());

				TabBar::render(frame, tab_area, &labels);
				TreeView::render(frame, tree_area, &rows, tab.cursor, &tab.selection, visual);
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

			app.active_tab_mut().rename = rename;
			Ok(())
		};

		draw(&mut app, &mut term)?;
		while let Some(event) = rx.recv().await {
			let event = match event {
				Event::Term(crossterm::event::Event::Key(key)) if key.kind == KeyEventKind::Press => {
					if app.active_tab().rename.is_some() {
						Event::RenameKey(key)
					} else {
						match router.route(key.code) {
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

	pub fn next_tab(&mut self) {
		let Some(pos) = self.tabs.iter().position(|t| t.id == self.active) else { return };
		self.active = self.tabs[(pos + 1) % self.tabs.len()].id;
	}

	pub fn prev_tab(&mut self) {
		let Some(pos) = self.tabs.iter().position(|t| t.id == self.active) else { return };
		self.active = self.tabs[(pos + self.tabs.len() - 1) % self.tabs.len()].id;
	}
}

#[cfg(test)]
mod tests {
	use std::{fs, path::Path};

	use super::*;

	async fn app(root: &Path) -> (App, mpsc::UnboundedReceiver<Event>) {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let first = Tab::open(0, root.to_path_buf(), tx.clone()).unwrap();
		let mut app = App { tabs: vec![first], active: 0, quit: false, next_tab_id: 1, tx };

		// drain the root tab's initial listing so it's got visible rows
		let event = rx.recv().await.unwrap();
		Dispatcher::dispatch(&mut app, event);

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
	async fn next_and_prev_tab_wrap_around() {
		let root = std::env::temp_dir().join("tuzi-app-test-cycletab");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await; // 0
		app.new_tab(); // 1
		app.new_tab(); // 2
		app.active = 0;

		app.next_tab();
		assert_eq!(app.active, 1);
		app.next_tab();
		assert_eq!(app.active, 2);
		app.next_tab();
		assert_eq!(app.active, 0, "wraps back to the first");

		app.prev_tab();
		assert_eq!(app.active, 2, "wraps the other way too");

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
		Dispatcher::dispatch(&mut app, event);

		// Expand a directory on the *inactive* tab 0 and route the
		// resulting Loaded event straight through Dispatcher, the way the
		// real event loop would — not by calling tab methods directly. Both
		// tabs share the same channel `app` was built with, so this is the
		// exact path a real background load takes.
		let path = root.join("a");
		app.tab_mut(0).unwrap().tree.mark_expanded(&path);
		app.tab_mut(0).unwrap().scheduler.refresh(path);

		let event = rx.recv().await.unwrap();
		let tab = match &event {
			Event::Loaded { tab, .. } => *tab,
			_ => panic!("expected Loaded"),
		};
		assert_eq!(tab, 0, "tagged with the tab that asked for it, not whichever tab is active");

		Dispatcher::dispatch(&mut app, event);
		assert_eq!(app.active, 1, "dispatching a background event never changes which tab is active");
		assert!(app.tab_mut(0).unwrap().tree.is_loaded(&root.join("a")), "but it still lands on the tab it was meant for");

		fs::remove_dir_all(&root).unwrap();
	}
}
