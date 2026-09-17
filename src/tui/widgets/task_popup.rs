use ratatui::{Frame, layout::{Alignment, Constraint, Layout, Rect}, text::{Line, Span}, widgets::{Block, BorderType, Borders, Clear, Gauge, Paragraph}};

use crate::{tasks::{Task, TaskKind, TaskState}, theme::Theme};

pub struct TaskPopup;

impl TaskPopup {
	pub fn render(frame: &mut Frame, area: Rect, tasks: &[Task], cursor: usize, theme: &Theme) {
		let [_, vertical, _] = Layout::vertical([Constraint::Percentage(15), Constraint::Percentage(70), Constraint::Percentage(15)]).areas(area);
		let [_, popup, _] = Layout::horizontal([Constraint::Percentage(15), Constraint::Percentage(70), Constraint::Percentage(15)]).areas(vertical);
		frame.render_widget(Clear, popup);
		let block = Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).title(" Tasks ").title_alignment(Alignment::Center).border_style(theme.style("popup.border"));
		let inner = block.inner(popup);
		frame.render_widget(block, popup);
		if tasks.is_empty() {
			frame.render_widget(Paragraph::new("No tasks").alignment(Alignment::Center).style(theme.style("popup.muted")), inner);
			return;
		}
		for (index, task) in tasks.iter().enumerate() {
			let y = inner.y + index as u16 * 3;
			if y + 1 >= inner.bottom() { break }
			let selected = index == cursor;
			let style = if task.state == TaskState::Failed { theme.style("tasks.failed") } else if selected { theme.style("tasks.selected") } else { theme.style("tasks.normal") };
			let icon = match task.kind { TaskKind::Copy => "", TaskKind::Move => "", TaskKind::Trash => "", TaskKind::Delete => "" };
			frame.render_widget(Paragraph::new(Line::from(vec![Span::styled(format!(" {icon}  "), style), Span::styled(&task.title, style)])), Rect::new(inner.x, y, inner.width, 1));
			if task.state == TaskState::Running {
				frame.render_widget(Gauge::default().ratio((task.percent() / 100.0).clamp(0.0, 1.0)).label(format!("{:3.0}%  {}", task.percent(), task.detail())).gauge_style(theme.style("tasks.progress")), Rect::new(inner.x + 2, y + 1, inner.width.saturating_sub(3), 1));
			} else {
				frame.render_widget(Paragraph::new(task.detail()).style(if task.state == TaskState::Failed { theme.style("tasks.failed") } else { theme.style("popup.muted") }), Rect::new(inner.x + 2, y + 1, inner.width.saturating_sub(3), 1));
			}
		}
		let help = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
		frame.render_widget(Paragraph::new("j/k select   x cancel   w/Esc close").alignment(Alignment::Center).style(theme.style("popup.muted")), help);
	}
}
