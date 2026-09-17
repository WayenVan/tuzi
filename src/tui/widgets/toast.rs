use ratatui::{Frame, layout::{Alignment, Rect}, widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap}};

use crate::{notice::{Notice, NoticeLevel}, theme::Theme};

/// Stacks the most recent toasts in the top-right corner. No slide-in/out
/// animation — a notice is either on screen or it's been pruned; `App`
/// schedules the redraw that makes the latter happen on time.
pub struct Toast;

impl Toast {
	pub fn render(frame: &mut Frame, area: Rect, notices: &[Notice], theme: &Theme) {
		if notices.is_empty() || area.width < 8 {
			return;
		}
		let width = area.width.clamp(8, 48);
		let height = 5u16;
		let gap = 1u16;

		let mut y = area.y;
		for notice in notices.iter().rev().take(3) {
			if y + height > area.bottom() {
				break;
			}
			let rect = Rect::new(area.right().saturating_sub(width), y, width, height);
			let (style, title) = match notice.level {
				NoticeLevel::Info => (theme.style("notify.info"), " Info "),
				NoticeLevel::Warn => (theme.style("notify.warn"), " Warning "),
				NoticeLevel::Error => (theme.style("notify.error"), " Error "),
			};

			frame.render_widget(Clear, rect);
			let block = Block::new()
				.borders(Borders::ALL)
				.border_type(BorderType::Rounded)
				.border_style(style)
				.title(title)
				.title_alignment(Alignment::Center);
			frame.render_widget(
				Paragraph::new(notice.message.as_str()).wrap(Wrap { trim: true }).style(style).block(block),
				rect,
			);

			y += height + gap;
		}
	}
}
