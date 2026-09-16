use ratatui::{
	Frame,
	layout::{Constraint, Direction, Layout, Rect},
	style::{Color, Modifier, Style},
	text::{Line, Span},
	widgets::{Block, Clear, Paragraph},
};

use crate::core::{Input, InputMode};

pub struct Prompt;

impl Prompt {
	/// Renders `input` as a floating, bordered box centered over `area` —
	/// a popup dialog rather than a line squeezed into the status bar.
	/// Border color and title both name the current vim mode; the visual
	/// selection (if any) is highlighted inline. Returns the screen
	/// column/row to park the terminal cursor at.
	pub fn render(frame: &mut Frame, area: Rect, input: &Input) -> (u16, u16) {
		let width = area.width.saturating_sub(4).min(50);
		let rect = Self::centered(width + 2, 3, area);

		frame.render_widget(Clear, rect);

		let (label, color) = match input.mode {
			InputMode::Insert => ("INSERT", Color::Green),
			InputMode::Normal => ("NORMAL", Color::Blue),
			InputMode::Visual => ("VISUAL", Color::Magenta),
			InputMode::Replace => ("REPLACE", Color::Yellow),
		};

		let block = Block::bordered().title(format!(" {} [{label}] ", input.title)).border_style(Style::new().fg(color));
		let inner = block.inner(rect);
		frame.render_widget(block, rect);
		frame.render_widget(Paragraph::new(Self::line(input)), inner);

		(inner.x + input.cursor as u16, inner.y)
	}

	fn line(input: &Input) -> Line<'static> {
		let Some((lo, hi)) = input.selection() else { return Line::from(input.value()) };

		let before: String = input.value[..lo].iter().collect();
		let selected: String = input.value[lo..=hi.min(input.value.len().saturating_sub(1))].iter().collect();
		let after: String = input.value[(hi + 1).min(input.value.len())..].iter().collect();

		Line::from(vec![Span::raw(before), Span::styled(selected, Style::new().add_modifier(Modifier::REVERSED)), Span::raw(after)])
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
