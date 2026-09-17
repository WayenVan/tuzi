use std::{cell::Cell, io};

use edtui::EditorMode;
use ratatui::{layout::{Constraint, Direction, Layout}, style::{Color, Modifier, Style}};

use crate::{event::Event, preview::PreviewTarget, status::Segment, tui::{Raterm, widgets::{CompletionPopup, ConfirmPopup, OpenPopup, PreviewView, Prompt, StatusBar, TabBar, TaskPopup, Toast, TreeView, TreeViewState, WhichPopup, WinBar, WinBarState}}};

use super::{App, app::MouseState};

impl App {
	pub(super) fn render(&mut self, term: &mut Raterm) -> io::Result<()> {
		self.prune_notices();
		let tree_rows = Cell::new(self.tree_rows);
		let preview_size = Cell::new((0, 0));
		let redraw_tx = self.tx.clone();
		let which = self.which.clone();
		let icon_theme = &self.icon_theme;
		let cwd = self.active_tab().tree.root.path.clone();
		let labels = self.tab_labels();
		let preview_percent = self.mouse.preview_percent;
		let geometry = Cell::new(self.mouse);

		let active = self.active;
		let tab = self.tabs.iter_mut().find(|tab| tab.id == active).expect("active tab exists");
		// Input is temporarily moved out because visible rows borrow the tree,
		// while edtui needs mutable access to the input state during rendering.
		let mut input = tab.input.take();
		let visible_len = tab.visible_len();
		let mut status = tab.status_line();
		if let Some(error) = input.as_ref().and_then(|input| input.error.as_ref()) {
			status.error = Some(error.clone());
		}
		let column_mode = tab.column_mode;
		let mut scroll = tab.scroll;
		let preview_visible = tab.preview.visible;
		let pending_delete = tab.pending_delete.clone();
		let selected_node = tab.visible_at(tab.cursor).map(|(_, node)| node);
		let preview_target = selected_node.map(PreviewTarget::from_node);
		let finder_query = tab.finder.as_ref().map(|finder| finder.query());
		let filter_query = tab.filter.as_ref().map(|filter| filter.query());
		let task_visible = self.tasks.visible;
		let task_cursor = self.tasks.cursor;
		let tasks = &self.tasks.tasks;
		let running = tasks.len();
		let pending_quit = self.pending_quit;
		let notices = &self.notices;

		// What goes in the status bar, and on which side, lives entirely
		// here — `StatusBar` itself just lays these two lists out. Adding a
		// clock, a git branch, anything else later is just pushing another
		// `Segment` into whichever of these two it belongs in.
		let mut status_left = vec![Segment::new(format!(" {} ", status.mode.label()), status.mode.style().add_modifier(Modifier::BOLD))];
		if let Some(error) = &status.error {
			status_left.push(Segment::new(format!(" {error}"), Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)));
		} else if !status.name.is_empty() {
			status_left.push(Segment::new(format!(" {}", status.name), Style::new().fg(Color::Gray)));
			status_left.push(Segment::new(format!("  {}  {}", status.size, status.permissions), Style::new().fg(Color::DarkGray)));
		}
		let mut status_right = Vec::new();
		if let Some((count, percent)) = self.tasks.summary() {
			status_right.push(Segment::new(format!(" {percent:3.0}% · {count} tasks "), Style::new().fg(Color::Black).bg(Color::Blue)));
		}

		term.terminal.draw(|frame| {
			let [win_area, tab_area, body_area, status_area] = Layout::default()
				.direction(Direction::Vertical)
				.constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
				.areas(frame.area());
			let (tree_area, preview_area) = if preview_visible {
				let [tree, preview] = Layout::default()
					.direction(Direction::Horizontal)
					.constraints([Constraint::Percentage(100 - preview_percent), Constraint::Percentage(preview_percent)])
					.areas(body_area);
				(tree, Some(preview))
			} else {
				(body_area, None)
			};
			tree_rows.set(tree_area.height as usize);
			let range = crate::tui::widgets::viewport(visible_len, tab.cursor, scroll, tree_area.height as usize);
			geometry.set(MouseState { tabs: tab_area, body: body_area, tree: tree_area, preview: preview_area, tree_row_offset: range.start, ..geometry.get() });
			let rows = tab.visible_range(range.clone());

			WinBar::render(frame, win_area, WinBarState { path: &cwd, finder: finder_query, filter: filter_query });
			TabBar::render(frame, tab_area, &labels);
			TreeView::render(
				frame,
				tree_area,
				&rows,
				range.start,
				TreeViewState {
					cursor: tab.cursor,
					selection: &tab.selection,
					visual: tab.visual,
					clipboard: &self.clipboard,
					clipboard_cut: self.clipboard_cut,
					column_mode,
					icon_theme,
					finder: tab.finder.as_ref(),
					filter: tab.filter.as_ref(),
					scroll: &mut scroll,
				},
			);
			if let Some(area) = preview_area {
				preview_size.set((area.width.saturating_sub(1), area.height));
				PreviewView::render(frame, area, selected_node, &tab.preview.state, tab.preview.skip);
			}
			StatusBar::render(frame, status_area, &status_left, &status_right);
			WhichPopup::render(frame, frame.area(), &which);
			if let Some(picker) = &self.open_picker {
				OpenPopup::render(frame, frame.area(), picker);
			}
			if let Some((targets, mode)) = &pending_delete {
				ConfirmPopup::render_delete(frame, frame.area(), targets, *mode);
			}
			if pending_quit {
				ConfirmPopup::render_quit(frame, frame.area(), running);
			}
			if let Some(input) = &mut input {
				let (x, y, rect) = Prompt::render(frame, frame.area(), input.title(), &mut input.state);
				if let Some(completion) = &input.completion {
					CompletionPopup::render(frame, frame.area(), rect, &completion.candidates, completion.selected);
				}
				frame.set_cursor_position((x, y));
			}
			if task_visible { TaskPopup::render(frame, frame.area(), tasks, task_cursor); }
			Toast::render(frame, body_area, notices);
		})?;
		tab.scroll = scroll;

		let (preview_width, preview_height) = preview_size.get();
		if tab.preview.sync(preview_target, preview_width, preview_height) {
			let _ = redraw_tx.send(Event::Redraw);
		}
		if let Some(input) = &input {
			use crossterm::cursor::SetCursorStyle;
			let style = if input.state.mode == EditorMode::Insert { SetCursorStyle::SteadyBar } else { SetCursorStyle::SteadyBlock };
			crossterm::execute!(io::stdout(), style)?;
		}

		tab.input = input;
		self.tree_rows = tree_rows.get();
		self.mouse = geometry.get();
		Ok(())
	}
}
