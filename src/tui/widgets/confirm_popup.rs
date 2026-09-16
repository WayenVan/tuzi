use std::path::PathBuf;

use ratatui::{Frame, layout::{Alignment, Constraint, Direction, Layout, Rect}, style::{Color, Modifier, Style}, text::{Line, Span}, widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph}};

use crate::action::DeleteMode;

pub struct ConfirmPopup;

impl ConfirmPopup {
	pub fn render_delete(frame: &mut Frame, area: Rect, targets: &[PathBuf], mode: DeleteMode) {
		if targets.is_empty() || area.width < 4 || area.height < 4 {
			return;
		}
		let width = area.width.clamp(4, 60);
		let list_rows = targets.len().min(6) as u16;
		let height = (list_rows + 6).min(area.height).max(4);
		let popup = Rect::new(
			area.x + area.width.saturating_sub(width) / 2,
			area.y + area.height.saturating_sub(height) / 2,
			width,
			height,
		);

		frame.render_widget(Clear, popup);
		let title = if mode == DeleteMode::Permanent { " Delete permanently? " } else { " Move to Trash? " };
		let block = Block::new()
			.borders(Borders::ALL)
			.border_type(BorderType::Rounded)
			.border_style(Style::new().fg(Color::Red))
			.title(title)
			.title_alignment(Alignment::Center);
		let inner = block.inner(popup);
		frame.render_widget(block, popup);

		let [body, list, buttons] = Layout::default()
			.direction(Direction::Vertical)
			.constraints([Constraint::Length(2), Constraint::Min(0), Constraint::Length(1)])
			.areas(inner);
		let body_text = if mode == DeleteMode::Permanent {
			format!("Permanently delete {} item(s)? This cannot be undone.", targets.len())
		} else {
			format!("Move {} item(s) to the Trash?", targets.len())
		};
		frame.render_widget(Paragraph::new(body_text).alignment(Alignment::Center), body);
		let items = targets.iter().take(6).map(|path| {
			let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
			ListItem::new(Line::from(format!("  {name}")))
		});
		frame.render_widget(List::new(items), list);

		let [yes, no] = Layout::default()
			.direction(Direction::Horizontal)
			.constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
			.areas(buttons);
		frame.render_widget(
			Paragraph::new(Line::from(vec![Span::styled("Yes (y)", Style::new().fg(Color::Red))]))
				.alignment(Alignment::Center),
			yes,
		);
		frame.render_widget(
			Paragraph::new(Span::styled("No (n) [Enter]", Style::new().fg(Color::Black).bg(Color::LightCyan).add_modifier(Modifier::BOLD)))
				.alignment(Alignment::Center),
			no,
		);
	}
}
