use ratatui::{Frame, layout::Rect, style::{Color, Modifier, Style}, text::{Line, Span}, widgets::{List, ListItem, ListState}};

use crate::{column_mode::ColumnMode, core::{Node, Selection, Visual}, finder::Finder, icon::{Icon, IconTheme}};

pub struct TreeView;

pub struct TreeViewState<'a> {
	pub cursor:        usize,
	pub selection:     &'a Selection,
	pub visual:        Option<Visual>,
	pub clipboard:     &'a [std::path::PathBuf],
	pub clipboard_cut: bool,
	pub column_mode:   ColumnMode,
	pub icon_theme:    &'a IconTheme,
	pub finder:        Option<&'a Finder>,
	/// The tab's persisted scroll offset — read to seed this frame's list,
	/// then written back with whatever ratatui settled on, so it only
	/// shifts when the cursor would otherwise leave the viewport.
	pub scroll:        &'a mut usize,
}

impl TreeView {
	pub fn render(frame: &mut Frame, area: Rect, rows: &[(usize, &Node)], state: TreeViewState<'_>) {
		let items = rows.iter().enumerate().map(|(index, (depth, node))| {
			let name = node.path.file_name().map_or_else(|| node.path.display().to_string(), |n| n.to_string_lossy().into_owned());
			let mut icon = state.icon_theme.icon_for(node);
			if index == state.cursor {
				icon.style = Style::new();
			}
			// A pending visual range previews the outcome of committing it
			// (Esc) rather than the current selection: rows inside it show
			// as selected for a plain visual, or unselected for a visual
			// unset, even before `commit_visual` actually touches `selection`.
			let visual_preview = state.visual.and_then(|visual| {
				let (lo, hi) = visual.range(state.cursor);
				(lo..=hi).contains(&index).then_some(!visual.unset)
			});
			let selected = visual_preview.unwrap_or_else(|| state.selection.contains(&node.path));
			let marker_style = if selected {
				Some(Style::new().fg(Color::LightYellow).bg(Color::LightYellow))
			} else if state.clipboard.contains(&node.path) && state.clipboard_cut {
				Some(Style::new().fg(Color::LightRed).bg(Color::LightRed))
			} else if state.clipboard.contains(&node.path) {
				Some(Style::new().fg(Color::LightGreen).bg(Color::LightGreen))
			} else {
				None
			};
			let loading = if node.expanded && node.children.is_none() { " (loading…)" } else { "" };
			let matches = state.finder.map_or_else(Vec::new, |finder| finder.ranges(&name));
			let line = row_line(
				"  ".repeat(*depth),
				marker_style,
				icon,
				format!("{name}{loading}"),
				matches,
				state.column_mode.text(node),
				area.width as usize,
			);

			ListItem::new(line)
		});

		let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
		let mut list_state = ListState::default().with_selected(Some(state.cursor)).with_offset(*state.scroll);
		frame.render_stateful_widget(list, area, &mut list_state);
		*state.scroll = list_state.offset();
	}
}

fn row_line(
	indent: String,
	marker: Option<Style>,
	icon: Icon,
	body: String,
	matches: Vec<std::ops::Range<usize>>,
	right: Option<String>,
	width: usize,
) -> Line<'static> {
	let right_width = right.as_deref().map_or(0, |text| Line::from(text).width());
	if right_width >= width {
		return Line::from(right.unwrap_or_default());
	}
	let left_limit = if right.is_some() { width - right_width - 1 } else { width };
	let prefix_width = Line::from(indent.as_str()).width() + 4;
	if prefix_width > left_limit {
		return Line::from(truncate(format!("{indent}  {} {body}", icon.text), left_limit));
	}
	let body = truncate(body, left_limit - prefix_width);
	let left_width = prefix_width + Line::from(body.as_str()).width();
	let padding = right.as_ref().map_or(0, |_| width - left_width - right_width);
	let marker = marker.map_or_else(|| Span::raw(" "), |style| Span::styled("│", style));
	let mut spans = vec![
		Span::raw(indent),
		marker,
		Span::raw(" "),
		Span::styled(icon.text.to_string(), icon.style),
		Span::raw(" "),
	];
	spans.extend(highlight_matches(body, &matches));
	if let Some(right) = right {
		spans.push(Span::raw(" ".repeat(padding)));
		spans.push(Span::raw(right));
	}
	Line::from(spans)
}

fn highlight_matches(body: String, matches: &[std::ops::Range<usize>]) -> Vec<Span<'static>> {
	let chars: Vec<char> = body.chars().collect();
	if matches.is_empty() {
		return vec![Span::raw(body)];
	}
	let matched = |index| matches.iter().any(|range| range.contains(&index));
	let mut spans = Vec::new();
	let mut start = 0;
	while start < chars.len() {
		let styled = matched(start);
		let mut end = start + 1;
		while end < chars.len() && matched(end) == styled {
			end += 1;
		}
		let text: String = chars[start..end].iter().collect();
		if styled {
			spans.push(Span::styled(
				text,
				Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED),
			));
		} else {
			spans.push(Span::raw(text));
		}
		start = end;
	}
	spans
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
