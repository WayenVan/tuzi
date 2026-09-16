use std::{cell::Cell, io};

use edtui::EditorMode;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::{event::Event, preview::PreviewTarget, tui::{Raterm, widgets::{CompletionPopup, ConfirmPopup, OpenPopup, PreviewView, Prompt, StatusBar, TabBar, TreeView, TreeViewState, WhichPopup, WinBar}}};

use super::App;

impl App {
	pub(super) fn render(&mut self, term: &mut Raterm) -> io::Result<()> {
		let tree_rows = Cell::new(self.tree_rows);
		let preview_size = Cell::new((0, 0));
		let redraw_tx = self.tx.clone();
		let which = self.which.clone();
		let icon_theme = &self.icon_theme;
		let cwd = self.active_tab().tree.root.path.clone();
		let labels: Vec<(bool, String)> = self
			.tabs
			.iter()
			.map(|tab| {
				let name = tab
					.tree
					.root
					.path
					.file_name()
					.map_or_else(|| tab.tree.root.path.display().to_string(), |name| name.to_string_lossy().into_owned());
				(tab.id == self.active, name)
			})
			.collect();

		let active = self.active;
		let tab = self.tabs.iter_mut().find(|tab| tab.id == active).expect("active tab exists");
		// Input is temporarily moved out because visible rows borrow the tree,
		// while edtui needs mutable access to the input state during rendering.
		let mut input = tab.input.take();
		let rows = tab.visible();
		let mut status = tab.status_line();
		if let Some(error) = input.as_ref().and_then(|input| input.error.as_ref()) {
			status.error = Some(error.clone());
		}
		let column_mode = tab.column_mode;
		let preview_visible = tab.preview.visible;
		let pending_delete = tab.pending_delete.clone();
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
			TreeView::render(
				frame,
				tree_area,
				&rows,
				TreeViewState {
					cursor: tab.cursor,
					selection: &tab.selection,
					clipboard: &tab.clipboard,
					clipboard_cut: tab.clipboard_cut,
					column_mode,
					icon_theme,
					finder: tab.finder.as_ref(),
				},
			);
			if let Some(area) = preview_area {
				preview_size.set((area.width.saturating_sub(1), area.height));
				PreviewView::render(frame, area, rows.get(tab.cursor).map(|(_, node)| *node), &tab.preview.state, tab.preview.skip);
			}
			StatusBar::render(frame, status_area, &status);
			WhichPopup::render(frame, frame.area(), &which);
			if let Some(picker) = &self.open_picker {
				OpenPopup::render(frame, frame.area(), picker);
			}
			if let Some(targets) = &pending_delete {
				ConfirmPopup::render_delete(frame, frame.area(), targets);
			}
			if let Some(input) = &mut input {
				let (x, y, rect) = Prompt::render(frame, frame.area(), input.title(), &mut input.state);
				if let Some(completion) = &input.completion {
					CompletionPopup::render(frame, frame.area(), rect, &completion.candidates, completion.selected);
				}
				frame.set_cursor_position((x, y));
			}
		})?;

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
		Ok(())
	}
}
