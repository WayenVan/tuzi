use ratatui::{
	Frame,
	layout::Rect,
	style::{Modifier, Style},
	text::{Line, Span},
	widgets::{Clear, List, ListItem, ListState, Paragraph},
};

use crate::{
	column_mode::ColumnMode,
	core::{Filter, Node, Selection, Visual},
	finder::Finder,
	icon::{Icon, IconTheme},
	theme::Theme,
};

pub struct TreeView;

pub struct TreeViewState<'a> {
	pub cursor: usize,
	pub focused: bool,
	pub selection: &'a Selection,
	pub visual: Option<Visual>,
	pub clipboard: &'a [std::path::PathBuf],
	pub clipboard_cut: bool,
	pub column_mode: ColumnMode,
	pub icon_theme: &'a IconTheme,
	pub theme: &'a Theme,
	pub finder: Option<&'a Finder>,
	pub filter: Option<&'a Filter>,
	pub filename_peek: bool,
	/// The tab's persisted scroll offset — read to seed this frame's list,
	/// then written back with whatever ratatui settled on, so it only
	/// shifts when the cursor would otherwise leave the viewport.
	pub scroll: &'a mut usize,
}

impl TreeView {
	pub fn render(frame: &mut Frame, area: Rect, rows: &[(usize, &Node)], row_offset: usize, state: TreeViewState<'_>) {
		*state.scroll = row_offset;
		let mut peek = None;
		let items: Vec<_> = rows.iter().enumerate().map(|(visible_index, (depth, node))| {
			let index = row_offset + visible_index;
			let name = node.path.file_name().map_or_else(|| node.path.display().to_string(), |n| n.to_string_lossy().into_owned());
			let icon = state.icon_theme.icon_for(node);
			let is_cursor_row = index == state.cursor && state.focused;
			// A pending visual range previews the outcome of committing it
			// (Esc) rather than the current selection: rows inside it show
			// as selected for a plain visual, or unselected for a visual
			// unset, even before `commit_visual` actually touches `selection`.
			let visual_preview = state.visual.and_then(|visual| {
				let (lo, hi) = visual.range(state.cursor);
				(lo..=hi).contains(&index).then_some(!visual.unset)
			});
			let marker_style = marker_style(visual_preview, state.selection.contains(&node.path), state.clipboard.contains(&node.path), state.clipboard_cut, state.theme);
			// A load failure is a standing problem with this node, not a
			// transient toast, so it's pinned to the row itself — checked
			// ahead of the loading indicator since a collapsed, failed node
			// isn't "loading" anymore, just broken until retried.
			let suffix = if let Some(error) = &node.load_error {
				Some((format!(" (Error: {error})"), state.theme.style("mgr.error")))
			} else if node.loading {
				Some((" (loading…)".to_string(), state.theme.style("mgr.loading")))
			} else if let Some(target) = &node.cha.link_target {
				Some((format!(" -> {}", target.display()), if node.cha.link_broken { state.theme.style("mgr.error") } else { state.theme.style("mgr.symlink") }))
			} else {
				None
			};
			let name_style = (node.cha.is_link && node.cha.link_broken).then(|| state.theme.style("mgr.error"));
			// An active filter already decided this row belongs in the tree;
			// highlighting why doubles as a hint once `find` isn't also
			// pointing at the same name.
			let matches = state.finder.map(|finder| finder.ranges(&name)).or_else(|| state.filter.map(|filter| filter.ranges(&name))).unwrap_or_default();
			if state.filename_peek && index == state.cursor {
				let right = state.column_mode.text(node);
				if let Some(remainder) = name_remainder(&indent_for(*depth), icon.as_ref(), &name, right.as_deref(), area.width as usize) {
					peek = Some((visible_index as u16, remainder));
				}
			}
			let line = row_line("  ".repeat(*depth), marker_style, icon, name, name_style, matches, suffix, state.column_mode.text(node), area.width as usize, is_cursor_row, state.theme);

			ListItem::new(line)
		}).collect();

		let list = List::new(items).highlight_style(cursor_style(state.focused, state.theme));
		let selected = state.cursor.checked_sub(row_offset).filter(|index| *index < rows.len());
		let mut list_state = ListState::default().with_selected(selected);
		frame.render_stateful_widget(list, area, &mut list_state);
		if let Some((row, remainder)) = peek {
			render_filename_peek(frame, area, row, &remainder, state.theme);
		}
	}
}

fn indent_for(depth: usize) -> String { "  ".repeat(depth) }

fn cursor_style(focused: bool, theme: &Theme) -> Style {
	if focused {
		Style::new().add_modifier(Modifier::REVERSED)
	} else {
		// A concrete color for now; keeping it in this one style boundary
		// makes it straightforward to source from the theme config later.
		theme.style("mgr.cursor_unfocused")
	}
}

fn marker_style(visual_preview: Option<bool>, selected: bool, clipboard: bool, clipboard_cut: bool, theme: &Theme) -> Option<Style> {
	match visual_preview {
		// Match the SEL status segment and take precedence over an older
		// yellow selection marker (or a clipboard marker) on the same row.
		Some(true) => Some(theme.style("mgr.marker_visual")),
		// Visual-unset previews the row with no marker, even if it was
		// selected before entering the range.
		Some(false) => None,
		None if selected => Some(theme.style("mgr.marker_selected")),
		None if clipboard && clipboard_cut => Some(theme.style("mgr.marker_cut")),
		None if clipboard => Some(theme.style("mgr.marker_copy")),
		None => None,
	}
}

pub(crate) fn viewport(len: usize, cursor: usize, scroll: usize, height: usize) -> std::ops::Range<usize> {
	if len == 0 || height == 0 {
		return 0..0;
	}
	let cursor = cursor.min(len - 1);
	let max_scroll = len.saturating_sub(height);
	let mut start = scroll.min(max_scroll);
	if cursor < start {
		start = cursor;
	} else if cursor >= start + height {
		start = cursor + 1 - height;
	}
	start..(start + height).min(len)
}

#[allow(clippy::too_many_arguments)]
fn row_line(
	indent: String,
	marker: Option<Style>,
	icon: Option<Icon>,
	body: String,
	name_style: Option<Style>,
	matches: Vec<std::ops::Range<usize>>,
	suffix: Option<(String, Style)>,
	right: Option<String>,
	width: usize,
	is_cursor_row: bool,
	theme: &Theme,
) -> Line<'static> {
	let right_width = right.as_deref().map_or(0, |text| Line::from(text).width());
	if right_width >= width {
		return Line::from(right.unwrap_or_default());
	}
	let left_limit = if right.is_some() { width - right_width - 1 } else { width };
	let icon_width = icon.as_ref().map_or(0, |icon| Line::from(icon.text.as_str()).width() + 1);
	let prefix_width = Line::from(indent.as_str()).width() + 2 + icon_width;
	if prefix_width > left_limit {
		let suffix_text = suffix.map_or_else(String::new, |(text, _)| text);
		let icon_text = icon.as_ref().map_or_else(String::new, |icon| format!("{} ", icon.text));
		return Line::from(truncate(format!("{indent}  {icon_text}{body}{suffix_text}"), left_limit));
	}
	let available = left_limit - prefix_width;
	// The name is the row's primary content; suffixes such as a symlink
	// target are supplementary and may only consume whatever remains.  In
	// particular, a long target must never make the name disappear.
	let body = truncate(body, available);
	let body_width = Line::from(body.as_str()).width();
	let suffix = suffix.and_then(|(text, style)| {
		let text = truncate(text, available.saturating_sub(body_width));
		(!text.is_empty() && text != "…").then_some((text, style))
	});
	let suffix_width = suffix.as_ref().map_or(0, |(text, _)| Line::from(text.as_str()).width());
	let left_width = prefix_width + Line::from(body.as_str()).width() + suffix_width;
	let padding = right.as_ref().map_or(0, |_| width.saturating_sub(left_width + right_width));
	let marker = marker.map_or_else(|| Span::raw(" "), |style| Span::styled("│", style));
	let mut spans = vec![Span::raw(indent), marker, Span::raw(" ")];
	if let Some(icon) = icon {
		spans.push(Span::styled(icon.text, icon.style));
		spans.push(Span::raw(" "));
	}
	spans.extend(highlight_matches(body, &matches, name_style.unwrap_or_default(), theme));
	if let Some((text, style)) = suffix {
		spans.push(Span::styled(text, style));
	}
	// The list's own reversed `highlight_style` already marks this row; an
	// explicit fg/bg on any of these spans would `patch` on top of it
	// instead of being swapped along with everything else, leaving a
	// mismatched-looking patch behind. The marker (index 1) is exempt: its
	// fg == bg trick already survives a reversal untouched, and it's the
	// one signal (multi-select/clipboard/visual) worth keeping visible even
	// where the cursor currently sits.
	if is_cursor_row {
		for span in spans.iter_mut().skip(2) {
			span.style.fg = None;
			span.style.bg = None;
		}
	}
	if let Some(right) = right {
		spans.push(Span::raw(" ".repeat(padding)));
		spans.push(Span::raw(right));
	}
	Line::from(spans)
}

fn highlight_matches(body: String, matches: &[std::ops::Range<usize>], name_style: Style, theme: &Theme) -> Vec<Span<'static>> {
	let chars: Vec<char> = body.chars().collect();
	if matches.is_empty() {
		return vec![Span::styled(body, name_style)];
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
			spans.push(Span::styled(text, theme.style("mgr.find_match")));
		} else {
			spans.push(Span::styled(text, name_style));
		}
		start = end;
	}
	spans
}

fn name_remainder(indent: &str, icon: Option<&Icon>, name: &str, right: Option<&str>, width: usize) -> Option<String> {
	let right_width = right.map_or(0, |text| Line::from(text).width());
	if right_width >= width {
		return Some(name.to_string());
	}
	let left_limit = if right.is_some() { width - right_width - 1 } else { width };
	let icon_width = icon.map_or(0, |icon| Line::from(icon.text.as_str()).width() + 1);
	let prefix_width = Line::from(indent).width() + 2 + icon_width;
	let available = left_limit.saturating_sub(prefix_width);
	if Line::from(name).width() <= available {
		return None;
	}
	let target = available.saturating_sub(1);
	let mut used = 0;
	let mut split = name.len();
	for (index, ch) in name.char_indices() {
		let next = Line::from(ch.to_string()).width();
		if used + next > target {
			split = index;
			break;
		}
		used += next;
	}
	(split < name.len()).then(|| name[split..].to_string())
}

fn render_filename_peek(frame: &mut Frame, area: Rect, cursor_row: u16, remainder: &str, theme: &Theme) {
	if area.width == 0 || area.height <= 1 {
		return;
	}
	let text = format!("…{remainder}");
	let width = Line::from(text.as_str()).width().min(area.width as usize).max(1) as u16;
	let lines = wrap_text(&text, width as usize);
	let below = area.height.saturating_sub(cursor_row + 1);
	let above = cursor_row;
	let wanted = lines.len().min(u16::MAX as usize) as u16;
	let (y, height) = if wanted <= below || below >= above {
		(area.y + cursor_row + 1, wanted.min(below))
	} else {
		(area.y + cursor_row - wanted.min(above), wanted.min(above))
	};
	if height == 0 {
		return;
	}
	let popup = Rect::new(area.right() - width, y, width, height);
	frame.render_widget(Clear, popup);
	frame.render_widget(Paragraph::new(lines).style(cursor_style(true, theme)), popup);
}

fn wrap_text(text: &str, width: usize) -> Vec<Line<'static>> {
	let mut lines = Vec::new();
	let mut current = String::new();
	let mut current_width = 0;
	for ch in text.chars() {
		let ch_width = Line::from(ch.to_string()).width();
		if !current.is_empty() && current_width + ch_width > width {
			lines.push(Line::from(std::mem::take(&mut current)));
			current_width = 0;
		}
		current.push(ch);
		current_width += ch_width;
	}
	if !current.is_empty() { lines.push(Line::from(current)); }
	lines
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

#[cfg(test)]
mod tests {
	use ratatui::style::{Color, Modifier, Style};

	use super::{cursor_style, highlight_matches, marker_style, name_remainder, row_line, viewport, wrap_text};
	use crate::icon::Icon;
	use crate::theme::Theme;

	#[test]
	fn unfocused_cursor_mutes_only_the_background() {
		let theme = Theme::default();
		let focused = cursor_style(true, &theme);
		let unfocused = cursor_style(false, &theme);
		assert!(focused.add_modifier.contains(Modifier::REVERSED));
		assert_eq!(unfocused.bg, Some(Color::Rgb(0x31, 0x32, 0x44)));
		assert!(!unfocused.add_modifier.contains(Modifier::REVERSED));
		assert!(!unfocused.add_modifier.contains(Modifier::DIM), "text must retain its original brightness");
	}

	#[test]
	#[allow(clippy::single_range_in_vec_init, reason = "one highlighted range is exactly what's under test")]
	fn broken_link_names_get_the_name_style_outside_any_matched_range() {
		let theme = Theme::default();
		let red = Style::new().fg(Color::Red);
		let spans = highlight_matches("dangling".to_string(), &[], red, &theme);
		assert_eq!(spans, vec![ratatui::text::Span::styled("dangling", red)]);

		// A find/filter match still wins the highlight over the broken-link
		// styling for the matched substring itself.
		let matched_ranges = vec![0..3];
		let spans = highlight_matches("dangling".to_string(), &matched_ranges, red, &theme);
		assert_eq!(spans[0].style, Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED));
		assert_eq!(spans[1].style, red, "the rest of the name keeps the broken-link color");
	}

	#[test]
	fn cursor_row_strips_colors_from_everything_but_the_marker() {
		let theme = Theme::default();
		let marker = Some(Style::new().fg(Color::LightYellow).bg(Color::LightYellow));
		let icon = Icon {
			text: "i".into(),
			style: Style::new().fg(Color::Blue),
		};
		let suffix = Some((" -> target".to_string(), Style::new().fg(Color::Gray)));

		let line = row_line(String::new(), marker, Some(icon), "name".to_string(), None, Vec::new(), suffix, None, 80, true, &theme);

		assert_eq!(
			line.spans[1].style,
			Style::new().fg(Color::LightYellow).bg(Color::LightYellow),
			"the marker's fg==bg trick survives a reversal untouched, so it's exempt"
		);
		for span in &line.spans[2..] {
			assert_eq!(span.style.fg, None, "an explicit fg would patch on top of the reversed highlight instead of being swapped with it");
			assert_eq!(span.style.bg, None);
		}
	}

	#[test]
	fn a_non_cursor_row_keeps_its_own_colors() {
		let theme = Theme::default();
		let icon = Icon {
			text: "i".into(),
			style: Style::new().fg(Color::Blue),
		};
		let line = row_line(String::new(), None, Some(icon), "name".to_string(), None, Vec::new(), None, None, 80, false, &theme);
		assert_eq!(line.spans[3].style, Style::new().fg(Color::Blue));
	}

	#[test]
	fn a_long_symlink_target_never_displaces_the_file_name() {
		let theme = Theme::default();
		let suffix = Some((" -> target".to_string(), Style::new().fg(Color::Gray)));
		let line = row_line(String::new(), None, None, "name".to_string(), None, Vec::new(), suffix, None, 12, false, &theme);
		let text = line.spans.iter().map(|span| span.content.as_ref()).collect::<String>();

		assert_eq!(text, "  name -> t…");
		assert_eq!(line.width(), 12);
	}

	#[test]
	fn a_symlink_target_is_hidden_when_only_the_file_name_fits() {
		let theme = Theme::default();
		let suffix = Some((" -> target".to_string(), Style::new().fg(Color::Gray)));
		let line = row_line(String::new(), None, None, "name".to_string(), None, Vec::new(), suffix, None, 6, false, &theme);
		let text = line.spans.iter().map(|span| span.content.as_ref()).collect::<String>();

		assert_eq!(text, "  name");
	}

	#[test]
	fn filename_peek_contains_exactly_the_part_omitted_from_the_row() {
		assert_eq!(name_remainder("", None, "abcdefgh", None, 8).as_deref(), Some("fgh"));
		assert_eq!(name_remainder("", None, "abcdefgh", None, 10), None);
	}

	#[test]
	fn filename_peek_wraps_without_losing_unicode_characters() {
		let lines = wrap_text("…甲乙丙丁", 5);
		let text = lines.iter().flat_map(|line| line.spans.iter()).map(|span| span.content.as_ref()).collect::<String>();
		assert_eq!(text, "…甲乙丙丁");
		assert!(lines.len() > 1);
	}

	#[test]
	fn visual_marker_uses_sel_color_and_overrides_older_markers() {
		let theme = Theme::default();
		let cyan = Some(Style::new().fg(Color::Cyan).bg(Color::Cyan));
		assert_eq!(marker_style(Some(true), true, true, true, &theme), cyan);
		assert_eq!(marker_style(Some(false), true, true, true, &theme), None);
		assert_eq!(
			marker_style(None, true, true, true, &theme),
			Some(Style::new().fg(Color::LightYellow).bg(Color::LightYellow)),
			"a committed visual selection remains visible over an older cut marker"
		);
		assert_eq!(
			marker_style(None, true, true, false, &theme),
			Some(Style::new().fg(Color::LightYellow).bg(Color::LightYellow)),
			"a committed visual selection remains visible over an older copy marker"
		);
		assert_eq!(marker_style(None, true, false, false, &theme), Some(Style::new().fg(Color::LightYellow).bg(Color::LightYellow)));
	}

	#[test]
	fn viewport_keeps_the_cursor_visible_without_formatting_the_whole_tree() {
		assert_eq!(viewport(56_000, 0, 0, 30), 0..30);
		assert_eq!(viewport(56_000, 29, 0, 30), 0..30);
		assert_eq!(viewport(56_000, 30, 0, 30), 1..31);
		assert_eq!(viewport(56_000, 55_999, 0, 30), 55_970..56_000);
		assert_eq!(viewport(56_000, 10, 100, 30), 10..40);
	}

	#[test]
	fn viewport_handles_empty_and_short_lists() {
		assert_eq!(viewport(0, 0, 0, 30), 0..0);
		assert_eq!(viewport(10, 9, 100, 30), 0..10);
		assert_eq!(viewport(10, 0, 0, 0), 0..0);
	}
}
