use ratatui::{
	Frame,
	layout::{Alignment, Constraint, Layout, Rect},
	text::{Line, Span},
	widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

use crate::{column_mode::format_modified, core::Node, fs::format_size, theme::Theme};

pub struct EntryDetailsPopup;

impl EntryDetailsPopup {
	pub fn render(frame: &mut Frame, area: Rect, node: &Node, scroll: u16, theme: &Theme, popup_width: u16) {
		let width = area.width.saturating_sub(4).min(popup_width.max(40));
		let height = area.height.saturating_sub(4).min(16);
		if width < 4 || height < 4 {
			return;
		}
		let [_, vertical, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(height), Constraint::Fill(1)]).areas(area);
		let [_, popup, _] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(width), Constraint::Fill(1)]).areas(vertical);

		frame.render_widget(Clear, popup);
		let block = Block::new()
			.borders(Borders::ALL)
			.border_type(BorderType::Rounded)
			.title(" Entry Details ")
			.title_alignment(Alignment::Center)
			.border_style(theme.style("popup.border"));
		let inner = block.inner(popup);
		frame.render_widget(block, popup);

		let name = node.path.file_name().map_or_else(|| node.path.display().to_string(), |name| name.to_string_lossy().into_owned());
		let kind = if node.cha.is_link { "Symbolic link" } else if node.cha.is_dir { "Directory" } else { "File" };
		let mut lines = vec![
			field("Name", name, theme),
			field("Path", node.path.display().to_string(), theme),
			field("Type", kind, theme),
			field("Size", format!("{} ({} bytes)", format_size(node.cha.len), node.cha.len), theme),
			field("Permissions", node.cha.permissions(), theme),
			field("Modified", node.cha.modified.map_or_else(|| "Unknown".into(), format_modified), theme),
		];
		if node.cha.is_link {
			lines.push(field("Target", node.cha.link_target.as_ref().map_or_else(|| "Unknown".into(), |target| target.display().to_string()), theme));
			lines.push(field("Target status", if node.cha.link_broken { "Broken" } else { "Available" }, theme));
		}
		if let Some(error) = &node.load_error {
			lines.push(field("Error", error, theme));
		}

		let help = Line::from(Span::styled("j/k scroll   q/Esc close", theme.style("popup.muted"))).alignment(Alignment::Center);
		let [content, help_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
		frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)), content);
		frame.render_widget(Paragraph::new(help), help_area);
	}
}

fn field<'a>(label: &'static str, value: impl Into<std::borrow::Cow<'a, str>>, theme: &Theme) -> Line<'a> {
	Line::from(vec![Span::styled(format!("{label}: "), theme.style("popup.muted")), Span::raw(value.into())])
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use ratatui::{Terminal, backend::TestBackend};

	use super::*;
	use crate::fs::Cha;

	#[test]
	fn renders_complete_entry_name_and_symlink_target() {
		let node = Node::new(PathBuf::from("/tmp/a-very-long-entry-name"), Cha {
			len: 12,
			is_dir: false,
			is_link: true,
			link_target: Some(PathBuf::from("../../a-complete-link-target")),
			link_broken: false,
			modified: None,
			mode: 0o777,
		});
		let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
		terminal.draw(|frame| EntryDetailsPopup::render(frame, frame.area(), &node, 0, &Theme::default(), 60)).unwrap();
		let rendered = terminal.backend().buffer().content().iter().map(|cell| cell.symbol()).collect::<String>();

		assert!(rendered.contains("a-very-long-entry-name"));
		assert!(rendered.contains("../../a-complete-link-target"));
	}
}
