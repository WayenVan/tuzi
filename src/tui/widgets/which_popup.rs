use ratatui::{Frame, layout::{Constraint, Direction, Layout, Rect}, style::{Color, Modifier, Style}, text::{Line, Span}, widgets::{Block, BorderType, Borders, Clear, Paragraph}};

use crate::keymap::WhichCandidate;

pub struct WhichPopup;

impl WhichPopup {
	pub fn render(frame: &mut Frame, area: Rect, candidates: &[WhichCandidate]) {
		if candidates.is_empty() || area.width < 4 || area.height < 3 {
			return;
		}

		let columns = if area.width >= 90 { 3 } else if area.width >= 50 { 2 } else { 1 };
		let rows = candidates.len().div_ceil(columns);
		let height = (rows as u16 + 2).min(area.height.saturating_sub(1));
		let popup = Rect::new(1.min(area.width), area.bottom().saturating_sub(height + 1), area.width.saturating_sub(2), height);
		if popup.width < 2 || popup.height < 3 {
			return;
		}

		frame.render_widget(Clear, popup);
		let block = Block::new()
			.borders(Borders::ALL)
			.border_type(BorderType::Rounded)
			.border_style(Style::new().fg(Color::Blue));
		let inner = block.inner(popup);
		frame.render_widget(block, popup);

		let widths = vec![Constraint::Ratio(1, columns as u32); columns];
		let chunks = Layout::default().direction(Direction::Horizontal).constraints(widths).split(inner);
		for (index, candidate) in candidates.iter().enumerate() {
			let column = index % columns;
			let row = index / columns;
			if row >= inner.height as usize {
				break;
			}
			let keys = candidate.keys.iter().map(ToString::to_string).collect::<String>();
			let line = Line::from(vec![
				Span::styled(format!("{keys:>8}"), Style::new().fg(Color::LightCyan).add_modifier(Modifier::BOLD)),
				Span::styled(" → ", Style::new().fg(Color::DarkGray)),
				Span::styled(candidate.description.clone(), Style::new().fg(Color::LightMagenta)),
			]);
			let cell = Rect { y: inner.y + row as u16, height: 1, ..chunks[column] };
			frame.render_widget(Paragraph::new(line), cell);
		}
	}
}
