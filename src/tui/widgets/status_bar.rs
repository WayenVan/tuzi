use ratatui::{Frame, layout::Rect, style::{Color, Modifier, Style}, text::{Line, Span}, widgets::Paragraph};

use crate::status::{StatusLine, StatusMode};

pub struct StatusBar;

impl StatusBar {
	pub fn render(frame: &mut Frame, area: Rect, status: &StatusLine) {
		let mode_style = match status.mode {
			StatusMode::Normal => Style::new().fg(Color::Black).bg(Color::Blue),
			StatusMode::Select => Style::new().fg(Color::Black).bg(Color::Cyan),
			StatusMode::Unset => Style::new().fg(Color::White).bg(Color::Red),
		};
		let mut spans = vec![
			Span::styled(format!(" {} ", status.mode.label()), mode_style.add_modifier(Modifier::BOLD)),
			Span::raw(" "),
		];
		if let Some(error) = &status.error {
			spans.push(Span::styled(error.clone(), Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)));
		} else if !status.name.is_empty() {
			spans.extend([
				Span::styled(status.name.clone(), Style::new().fg(Color::Gray)),
				Span::styled(format!("  {}  {}", status.size, status.permissions), Style::new().fg(Color::DarkGray)),
			]);
		}
		frame.render_widget(Paragraph::new(Line::from(spans)), area);
	}
}
