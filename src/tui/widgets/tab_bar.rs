use ratatui::{Frame, layout::Rect, style::{Color, Modifier, Style}, text::{Line, Span}, widgets::Paragraph};

pub struct TabBar;

impl TabBar {
	/// Takes owned labels so the caller can release its shared borrow of all
	/// tabs before borrowing the active tab mutably for the rest of a frame.
	pub fn render(frame: &mut Frame, area: Rect, tabs: &[(bool, String)]) {
		if tabs.is_empty() || area.width == 0 {
			return;
		}

		const OPEN: &str = "";
		const CLOSE: &str = "";
		let blue = Color::Rgb(0x89, 0xb4, 0xfa);
		let base = Color::Rgb(0x1e, 0x1e, 0x2e);
		let surface = Color::Rgb(0x31, 0x32, 0x44);
		let active = Style::new().fg(base).bg(blue).add_modifier(Modifier::BOLD);
		let inactive = Style::new().fg(blue).bg(surface);
		let separator = Style::new().fg(blue).bg(surface);
		let outer = Style::new().fg(surface);
		let max = area.width.saturating_sub(4) as usize / tabs.len();

		let mut spans = Vec::with_capacity(tabs.len() * 3 + 2);
		spans.push(Span::styled(OPEN, outer));
		for (index, (is_active, name)) in tabs.iter().enumerate() {
			let label = truncate(format!(" {} {name} ", index + 1), max);
			if *is_active {
				spans.push(Span::styled(OPEN, separator));
				spans.push(Span::styled(label, active));
				spans.push(Span::styled(CLOSE, separator));
			} else {
				spans.push(Span::styled(label, inactive));
			}
		}
		spans.push(Span::styled(CLOSE, outer));
		frame.render_widget(Paragraph::new(Line::from(spans)), area);
	}
}

fn truncate(text: String, max: usize) -> String {
	if Line::from(text.as_str()).width() <= max {
		return text;
	}
	if max == 0 {
		return String::new();
	}

	let target = max.saturating_sub(1);
	let mut out = String::new();
	for ch in text.chars() {
		if Line::from(out.as_str()).width() + Line::from(ch.to_string()).width() > target {
			break;
		}
		out.push(ch);
	}
	out.push('…');
	out
}
