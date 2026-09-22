use ratatui::{
	Frame,
	layout::Rect,
	text::{Line, Span},
	widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{config::PreviewLayout, core::Node, fs::format_size, preview::PreviewState, theme::Theme};

pub struct PreviewView;
struct DirectoryProvider;

impl PreviewView {
	pub fn render(frame: &mut Frame, area: Rect, node: Option<&Node>, state: &PreviewState, skip: usize, layout: PreviewLayout, theme: &Theme) {
		let border = if layout == PreviewLayout::Vertical { Borders::TOP } else { Borders::LEFT };
		let block = Block::new().borders(border).title(" Preview ").title_style(theme.style("preview.border"));
		let lines = match node {
			Some(node) if node.cha.is_dir => DirectoryProvider::render(node),
			Some(_) => text_lines(state, theme),
			None => Vec::new(),
		};
		frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }).scroll((0, 0)), area);
		if skip > 0 {
			let label = Line::from(format!(" {skip} ")).right_aligned();
			frame.render_widget(Paragraph::new(label), Rect::new(area.x, area.y, area.width.saturating_sub(1), 1));
		}
	}
}

impl DirectoryProvider {
	fn render(node: &Node) -> Vec<Line<'static>> {
		// `is_link` takes precedence: a symlinked directory is expandable (so
		// `is_dir` is true, see `fs::engine::cha_for`), but it should still read
		// as a symlink here rather than an indistinguishable plain directory.
		let kind = if node.cha.is_link {
			if node.cha.link_broken { "symlink (broken)" } else { "symlink" }
		} else if node.cha.is_dir {
			"directory"
		} else {
			"file"
		};
		let mut lines = vec![
			Line::from(node.path.display().to_string()),
			Line::from(""),
			Line::from(format!("Type: {kind}")),
			Line::from(format!("Size: {}", format_size(node.cha.len))),
			Line::from(format!("Mode: {}", node.cha.permissions())),
		];
		if let Some(target) = &node.cha.link_target {
			lines.push(Line::from(format!("Target: {}", target.display())));
		}

		if node.cha.is_dir {
			lines.push(Line::from(""));
			match &node.children {
				Some(children) if children.is_empty() => lines.push(Line::from("Empty directory")),
				Some(children) => lines.extend(children.iter().map(|child| {
					let name = child.path.file_name().map_or_else(|| child.path.display().to_string(), |name| name.to_string_lossy().into_owned());
					Line::from(format!("{} {name}", if child.cha.is_dir { "▸" } else { " " }))
				})),
				None => lines.push(Line::from("Directory contents not loaded")),
			}
		}

		lines
	}
}

fn text_lines(state: &PreviewState, theme: &Theme) -> Vec<Line<'static>> {
	match state {
		PreviewState::Empty => Vec::new(),
		PreviewState::Loading => vec![Line::from("Loading…")],
		PreviewState::Error(error) => vec![Line::from(Span::styled(error.clone(), theme.style("preview.error")))],
		PreviewState::Ready(data) if data.lines.is_empty() && data.eof => {
			vec![Line::from("Empty file")]
		}
		PreviewState::Ready(data) => data.lines.clone(),
	}
}
