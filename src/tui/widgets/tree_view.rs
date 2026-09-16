use ratatui::{Frame, layout::Rect, style::{Color, Modifier, Style}, widgets::{List, ListItem, ListState}};

use crate::core::{Node, Selection};

pub struct TreeView;

impl TreeView {
	pub fn render(
		frame: &mut Frame,
		area: Rect,
		rows: &[(usize, &Node)],
		cursor: usize,
		selection: &Selection,
		visual: Option<(usize, usize, bool)>,
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
			let line = format!("{}{mark}{marker}{name}{loading}", "  ".repeat(*depth));

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
