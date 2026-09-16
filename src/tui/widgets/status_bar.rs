use ratatui::{Frame, layout::Rect, text::{Line, Span}, widgets::Paragraph};

use crate::status::Segment;

pub struct StatusBar;

impl StatusBar {
	/// Lays `left` out flush-left and `right` flush-right within `area`, in
	/// the order each slice is given. Purely mechanical: it has no idea what
	/// "mode" or "a running task" means, so a new kind of content is never a
	/// reason to touch this function — just build another `Segment` where
	/// the rest of the bar is assembled and put it in the list you want.
	pub fn render(frame: &mut Frame, area: Rect, left: &[Segment], right: &[Segment]) {
		let to_line = |segments: &[Segment]| -> Line<'static> {
			Line::from(segments.iter().map(|s| Span::styled(s.text.clone(), s.style)).collect::<Vec<_>>())
		};

		let right_line = to_line(right);
		let right_width = (right_line.width() as u16).min(area.width);
		let left_width = area.width - right_width;

		frame.render_widget(Paragraph::new(to_line(left)), Rect::new(area.x, area.y, left_width, area.height));
		frame.render_widget(Paragraph::new(right_line), Rect::new(area.x + left_width, area.y, right_width, area.height));
	}
}
