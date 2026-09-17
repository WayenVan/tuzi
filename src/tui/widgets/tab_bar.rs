use ratatui::{Frame, layout::Rect, text::{Line, Span}, widgets::Paragraph};

use crate::theme::Theme;

pub struct TabBar;

impl TabBar {
	pub fn hit_test(area: Rect, tabs: &[(bool, String)], x: u16) -> Option<usize> {
		if tabs.is_empty() || x < area.x || x >= area.right() {
			return None;
		}
		let max = area.width.saturating_sub(4) as usize / tabs.len();
		let mut column = area.x + Line::from("").width() as u16;
		for (index, (active, name)) in tabs.iter().enumerate() {
			let label = truncate(format!(" {} {name} ", index + 1), max);
			let width = Line::from(label.as_str()).width() as u16 + if *active { 2 } else { 0 };
			if x >= column && x < column.saturating_add(width) {
				return Some(index);
			}
			column = column.saturating_add(width);
		}
		None
	}

	/// Takes owned labels so the caller can release its shared borrow of all
	/// tabs before borrowing the active tab mutably for the rest of a frame.
	pub fn render(frame: &mut Frame, area: Rect, tabs: &[(bool, String)], theme: &Theme) {
		if tabs.is_empty() || area.width == 0 {
			return;
		}

		const OPEN: &str = "";
		const CLOSE: &str = "";
		let active = theme.style("tabs.active");
		let inactive = theme.style("tabs.inactive");
		let separator = theme.style("tabs.separator");
		let outer = theme.style("tabs.outer");
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hit_test_tracks_rendered_tab_widths() {
		let tabs = vec![(true, "one".into()), (false, "two".into()), (false, "three".into())];
		let area = Rect::new(10, 2, 60, 1);
		assert_eq!(TabBar::hit_test(area, &tabs, 12), Some(0));
		assert_eq!(TabBar::hit_test(area, &tabs, 20), Some(1));
		assert_eq!(TabBar::hit_test(area, &tabs, 28), Some(2));
		assert_eq!(TabBar::hit_test(area, &tabs, 69), None);
	}
}
