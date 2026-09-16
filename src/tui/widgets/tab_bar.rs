use ratatui::{
	Frame,
	layout::Rect,
	style::{Modifier, Style},
	text::{Line, Span},
	widgets::Paragraph,
};

pub struct TabBar;

impl TabBar {
	/// Takes plain `(is_active, name)` pairs rather than `&[Tab]` — the
	/// caller collects those first, so rendering the bar never needs a live
	/// borrow of every tab while it's also holding one tab mutably for the
	/// rest of the frame.
	pub fn render(frame: &mut Frame, area: Rect, tabs: &[(bool, String)]) {
		let mut spans = Vec::with_capacity(tabs.len() * 2);
		for (i, (active, name)) in tabs.iter().enumerate() {
			let label = format!(" {}:{name} ", i + 1);
			let style = if *active { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
			spans.push(Span::styled(label, style));
			spans.push(Span::raw(" "));
		}
		frame.render_widget(Paragraph::new(Line::from(spans)), area);
	}
}
