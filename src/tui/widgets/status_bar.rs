use ratatui::{Frame, layout::Rect, style::{Color, Modifier, Style}, widgets::Paragraph};

pub struct StatusBar;

impl StatusBar {
	pub fn render(frame: &mut Frame, area: Rect, text: &str, warn: bool) {
		let style = if warn {
			Style::new().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)
		} else {
			Style::new().fg(Color::DarkGray)
		};
		frame.render_widget(Paragraph::new(text).style(style), area);
	}
}
