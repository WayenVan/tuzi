use std::{cell::Cell, io};

use edtui::EditorMode;
use ratatui::{layout::{Constraint, Direction, Layout}, style::{Modifier, Style}};

use crate::{config::PreviewLayout, event::Event, preview::PreviewTarget, status::{Segment, permission_style, position_labels}, tui::{Raterm, widgets::{ClipboardBadge, CompletionPopup, ConfirmPopup, EntryDetailsPopup, OpenPopup, PreviewView, Prompt, StatusBar, TabBar, TaskPopup, Toast, TreeView, TreeViewState, WhichPopup, WinBar, WinBarState}}};

use super::{App, app::MouseState};

impl App {
	pub(super) fn render(&mut self, term: &mut Raterm) -> io::Result<()> {
		self.prune_notices();
		let tree_rows = Cell::new(self.tree_rows);
		let preview_size = Cell::new((0, 0));
		let redraw_tx = self.tx.clone();
		let which = self.which.clone();
		let icon_theme = &self.icon_theme;
		let theme = &self.theme;
		let popup_width = self.config.ui.popup_width;
		let completion_max_items = self.config.ui.completion_max_items;
		let cwd = self.active_tab().tree.root.path.clone();
		let labels = self.tab_labels();
		let preview_percent = self.mouse.preview_percent;
		let preview_layout = self.config.preview.layout;
		let preview_split_threshold = self.config.preview.split_threshold;
		let terminal_focused = self.terminal_focused;
		let clipboard_badge = (!self.clipboard.is_empty()).then(|| if self.clipboard_cut { ClipboardBadge::Cut(self.clipboard.len()) } else { ClipboardBadge::Copy(self.clipboard.len()) });
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
		let entry_details = self.entry_details;
		let entry_details_scroll = self.entry_details_scroll;
		let task_cursor = self.tasks.cursor;
		let tasks = &self.tasks.tasks;
		let running = tasks.len();
		let pending_quit = self.pending_quit;
		let notices = &self.notices;

		// What goes in the status bar, and on which side, lives entirely
		// here — `StatusBar` itself just lays these two lists out. Adding a
		// clock, a git branch, anything else later is just pushing another
		// `Segment` into whichever of these two it belongs in.
		let mode_edge = Style::new().fg(status.mode.color(theme));
		let mode_fill = status.mode.style(theme).add_modifier(Modifier::BOLD);
		let alt_fill = status.mode.alt_style(theme);
		let mut status_left = vec![
			Segment::new("", mode_edge),
			Segment::new(format!(" {} ", status.mode.label()), mode_fill),
			Segment::new("", Style::new().fg(status.mode.color(theme)).bg(status.mode.alt_background(theme))),
			Segment::new(format!(" {} ", status.size), alt_fill),
			Segment::new("", Style::new().fg(status.mode.alt_background(theme))),
		];
		if let Some(error) = &status.error {
			status_left.push(Segment::new(format!(" {error}"), theme.style("mgr.error").add_modifier(Modifier::BOLD)));
		} else if !status.name.is_empty() {
			status_left.push(Segment::new(format!(" {}", status.name), Style::new()));
		}
		let mut status_right = Vec::new();
		if let Some((count, percent)) = self.tasks.summary() {
			status_right.push(Segment::new(
				format!(" {percent:3.0}% · {count} tasks "),
				theme.style("status.task"),
			));
		}
		for character in status.permissions.chars() {
			status_right.push(Segment::new(character.to_string(), permission_style(character, theme)));
		}
		let (position_percent, position_count) = position_labels(tab.cursor, visible_len);
		status_right.push(Segment::new(" ", Style::new().fg(status.mode.alt_background(theme))));
		status_right.push(Segment::new(format!(" {position_percent} "), alt_fill));
		status_right.push(Segment::new("", Style::new().fg(status.mode.color(theme)).bg(status.mode.alt_background(theme))));
		status_right.push(Segment::new(format!(" {position_count} "), mode_fill));
		status_right.push(Segment::new("", mode_edge));

		term.terminal.draw(|frame| {
			let [win_area, tab_area, body_area, status_area] = Layout::default()
				.direction(Direction::Vertical)
				.constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
				.areas(frame.area());
			let resolved_preview_layout = resolve_preview_layout(preview_layout, body_area.width, preview_split_threshold);
			let (tree_area, preview_area) = if preview_visible {
				let direction = if resolved_preview_layout == PreviewLayout::Vertical { Direction::Vertical } else { Direction::Horizontal };
				let [tree, preview] = Layout::default()
					.direction(direction)
					.constraints([Constraint::Percentage(100 - preview_percent), Constraint::Percentage(preview_percent)])
					.areas(body_area);
				(tree, Some(preview))
			} else {
				(body_area, None)
			};
			tree_rows.set(tree_area.height as usize);
			let range = crate::tui::widgets::viewport(visible_len, tab.cursor, scroll, tree_area.height as usize);
			geometry.set(MouseState { tabs: tab_area, body: body_area, tree: tree_area, preview: preview_area, preview_layout: resolved_preview_layout, tree_row_offset: range.start, ..geometry.get() });
			let rows = tab.visible_range(range.clone());

			WinBar::render(frame, win_area, WinBarState { path: &cwd, finder: finder_query, filter: filter_query, badge: clipboard_badge }, theme);
			TabBar::render(frame, tab_area, &labels, theme);
			TreeView::render(
				frame,
				tree_area,
				&rows,
				range.start,
				TreeViewState {
					cursor: tab.cursor,
					focused: terminal_focused,
					selection: &tab.selection,
					visual: tab.visual,
					clipboard: &self.clipboard,
					clipboard_cut: self.clipboard_cut,
					column_mode,
					icon_theme,
					theme,
					finder: tab.finder.as_ref(),
					filter: tab.filter.as_ref(),
					filename_peek: self.filename_peek,
					scroll: &mut scroll,
				},
			);
			if let Some(area) = preview_area {
				let size = if resolved_preview_layout == PreviewLayout::Vertical { (area.width, area.height.saturating_sub(1)) } else { (area.width.saturating_sub(1), area.height) };
				preview_size.set(size);
				PreviewView::render(frame, area, selected_node, &tab.preview.state, tab.preview.skip, resolved_preview_layout, theme);
			}
			StatusBar::render(frame, status_area, &status_left, &status_right);
			WhichPopup::render(frame, frame.area(), &which, theme);
			if let Some(picker) = &self.open_picker {
				OpenPopup::render(frame, frame.area(), picker, theme, popup_width);
			}
			if let Some((targets, mode)) = &pending_delete {
				ConfirmPopup::render_delete(frame, frame.area(), targets, *mode, theme, popup_width);
			}
			if pending_quit {
				ConfirmPopup::render_quit(frame, frame.area(), running, theme, popup_width);
			}
			if let Some(input) = &mut input {
				let (x, y, rect) = Prompt::render(frame, frame.area(), input.title(), &mut input.state, theme, popup_width);
				if let Some(completion) = &input.completion {
					CompletionPopup::render(frame, frame.area(), rect, &completion.candidates, completion.selected, completion.command, completion_max_items);
				}
				frame.set_cursor_position((x, y));
			}
			if task_visible { TaskPopup::render(frame, frame.area(), tasks, task_cursor, theme); }
			Toast::render(frame, body_area, notices, theme);
			if entry_details && let Some(node) = selected_node {
				EntryDetailsPopup::render(frame, frame.area(), node, entry_details_scroll, theme, popup_width);
			}
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

fn resolve_preview_layout(layout: PreviewLayout, width: u16, threshold: u16) -> PreviewLayout {
	match layout {
		PreviewLayout::Auto if width < threshold => PreviewLayout::Vertical,
		PreviewLayout::Auto => PreviewLayout::Horizontal,
		layout => layout,
	}
}

#[cfg(test)]
mod tests {
	use super::resolve_preview_layout;
	use crate::config::PreviewLayout;

	#[test]
	fn auto_preview_stacks_only_below_the_configured_width() {
		assert_eq!(resolve_preview_layout(PreviewLayout::Auto, 99, 100), PreviewLayout::Vertical);
		assert_eq!(resolve_preview_layout(PreviewLayout::Auto, 100, 100), PreviewLayout::Horizontal);
		assert_eq!(resolve_preview_layout(PreviewLayout::Horizontal, 20, 100), PreviewLayout::Horizontal);
		assert_eq!(resolve_preview_layout(PreviewLayout::Vertical, 200, 100), PreviewLayout::Vertical);
	}
}
