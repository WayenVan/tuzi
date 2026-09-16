use edtui::{EditorMode, EditorState, EditorTheme, EditorView};
use ratatui::{
	Frame,
	layout::{Constraint, Direction, Layout, Rect},
	style::{Color, Style},
	widgets::{Block, Clear},
};

pub struct Prompt;

impl Prompt {
	/// Renders `state` as a floating, bordered box centered over `area` —
	/// a popup dialog rather than a line squeezed into the status bar.
	/// Border color and title both name edtui's own current vim mode.
	/// Returns the screen column/row to park the terminal cursor at.
	pub fn render(frame: &mut Frame, area: Rect, title: &str, state: &mut EditorState) -> (u16, u16, Rect) {
		let width = area.width.saturating_sub(4).min(50);
		let rect = Self::centered(width + 2, 3, area);

		frame.render_widget(Clear, rect);

		let (label, color) = match state.mode {
			EditorMode::Insert => ("INSERT", Color::Green),
			EditorMode::Normal => ("NORMAL", Color::Blue),
			EditorMode::Visual => ("VISUAL", Color::Magenta),
			EditorMode::Search => ("SEARCH", Color::Yellow),
		};

		let block = Block::bordered().title(format!(" {title} [{label}] ")).border_style(Style::new().fg(color));
		let inner = block.inner(rect);
		frame.render_widget(block, rect);

		// edtui reserves a row for its own mode status line by default —
		// with only one content row to give it (a single-line prompt),
		// that status line would eat the entire row and the typed text
		// would never show at all. We already show the mode on the popup's
		// own border title, so hide edtui's copy. Its default theme also
		// hard-codes a black background regardless of the terminal's own
		// theme; clear that so the popup matches everything else we draw.
		// edtui draws its own cursor by styling the whole character cell
		// (white background by default). That painted cell looks like a block
		// even when the real terminal cursor is configured as a bar. Hide the
		// painted cursor and let the terminal cursor set by App::draw provide
		// the mode-dependent shape instead.
		let theme = EditorTheme::default().hide_status_line().base(Style::default()).hide_cursor();
		frame.render_widget(EditorView::new(state).single_line(true).theme(theme), inner);

		// Not `state.cursor_screen_position()`: for a single-line editor
		// rendered anywhere other than the terminal's own (0, 0), edtui
		// 0.11.7 returns the render area's own top-left corner regardless
		// of where the cursor actually is — column changes never show up.
		// `state.cursor` itself tracks correctly, so derive the screen
		// position from that instead.
		(inner.x + state.cursor.col as u16, inner.y + state.cursor.row as u16, rect)
	}

	fn centered(width: u16, height: u16, area: Rect) -> Rect {
		let vertical = Layout::default()
			.direction(Direction::Vertical)
			.constraints([Constraint::Fill(1), Constraint::Length(height), Constraint::Fill(1)])
			.split(area);

		Layout::default()
			.direction(Direction::Horizontal)
			.constraints([Constraint::Fill(1), Constraint::Length(width), Constraint::Fill(1)])
			.split(vertical[1])[1]
	}
}

#[cfg(test)]
mod tests {
	use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
	use edtui::{EditorEventHandler, Index2, Lines};
	use ratatui::{Terminal, backend::TestBackend};

	use super::*;

	fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }

	/// Regression test for an edtui 0.11.7 quirk: `cursor_screen_position()`
	/// ignores the cursor's column when the popup isn't drawn at the
	/// terminal's own (0, 0) — which it never is, since it's centered. We
	/// derive the position from `state.cursor` directly instead; this
	/// confirms that actually tracks movement.
	#[test]
	fn cursor_position_tracks_the_column_inside_the_popup() {
		let mut state = EditorState::new(Lines::from("old.txt"));
		state.set_single_line(true);
		state.mode = EditorMode::Insert;
		state.cursor = Index2::new(0, 7);

		let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
		let mut before = (0, 0);
		terminal.draw(|frame| {
			let (x, y, _) = Prompt::render(frame, frame.area(), "Rename", &mut state);
			before = (x, y);
		}).unwrap();

		let mut handler = EditorEventHandler::vim_mode();
		// Esc itself steps the cursor back one column too (vim's own
		// Insert -> Normal behavior), so two explicit `h`s after it is a
		// total move of three columns, not two.
		handler.on_key_event(key(KeyCode::Esc), &mut state); // Insert -> Normal
		handler.on_key_event(key(KeyCode::Char('h')), &mut state);
		handler.on_key_event(key(KeyCode::Char('h')), &mut state);

		let mut after = (0, 0);
		terminal.draw(|frame| {
			let (x, y, _) = Prompt::render(frame, frame.area(), "Rename", &mut state);
			after = (x, y);
		}).unwrap();

		assert_eq!(before.0 - after.0, 3, "Esc plus two lefts should move the reported cursor left by three columns");
		assert_eq!(before.1, after.1, "single line: the row never changes");
	}
}
