use ratatui::{Frame, layout::Rect, style::{Color, Modifier, Style}, text::{Line, Span}, widgets::{List, ListItem, ListState}};

use crate::{column_mode::ColumnMode, core::{Node, Selection}};

pub struct TreeView;

impl TreeView {
	pub fn render(
		frame: &mut Frame,
		area: Rect,
		rows: &[(usize, &Node)],
		cursor: usize,
		selection: &Selection,
		visual: Option<(usize, usize, bool)>,
		column_mode: ColumnMode,
	) {
		let items = rows.iter().enumerate().map(|(i, (depth, node))| {
			let name = node.path.file_name().map_or_else(|| node.path.display().to_string(), |n| n.to_string_lossy().into_owned());
			let marker = match (node.cha.is_dir, node.expanded) {
				(true, true) => "▾ ",
				(true, false) => "▸ ",
				(false, _) => "  ",
			};
			let selected = selection.contains(&node.path);
			let mark = if selected { "* " } else { "  " };
			let loading = if node.expanded && node.children.is_none() { " (loading…)" } else { "" };
			let left = format!("{}{mark}{marker}{name}{loading}", "  ".repeat(*depth));
			let line = match column_mode.text(node) {
				Some(column) => right_column(left, column, area.width as usize),
				None => Line::from(left),
			};

			let in_visual = visual.is_some_and(|(lo, hi, _)| (lo..=hi).contains(&i));
			let style = match (in_visual, visual.map(|(.., unset)| unset), selected) {
				(true, Some(true), _) => Style::new().fg(Color::Black).bg(Color::Red),
				(true, Some(false), _) => Style::new().fg(Color::Black).bg(Color::Cyan),
				(false, _, true) => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
				_ => Style::new(),
			};
			ListItem::new(line).style(style)
		});

		let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
		let mut state = ListState::default().with_selected(Some(cursor));
		frame.render_stateful_widget(list, area, &mut state);
	}
}

fn right_column(left: String, right: String, width: usize) -> Line<'static> {
	let right_width = Line::from(right.as_str()).width();
	if right_width >= width {
		return Line::from(right);
	}

	let left_limit = width - right_width - 1;
	let left = truncate(left, left_limit);
	let padding = width - Line::from(left.as_str()).width() - right_width;
	Line::from(vec![Span::raw(left), Span::raw(" ".repeat(padding)), Span::raw(right)])
}

fn truncate(text: String, width: usize) -> String {
	if Line::from(text.as_str()).width() <= width {
		return text;
	}
	if width == 0 {
		return String::new();
	}

	let target = width.saturating_sub(1);
	let mut out = String::new();
	for ch in text.chars() {
		let next = Line::from(ch.to_string()).width();
		if Line::from(out.as_str()).width() + next > target {
			break;
		}
		out.push(ch);
	}
	out.push('…');
	out
}
