use ratatui::{Frame, layout::Rect, text::{Line, Span}, widgets::Paragraph};

use crate::theme::Theme;

/// Powerline caps around the whole bar and around the active tab.
const OPEN: &str = "\u{e0b6}";
const CLOSE: &str = "\u{e0b4}";
/// Replace a cap on the side that has tabs scrolled out of view.
const MORE_LEFT: &str = "‹";
const MORE_RIGHT: &str = "›";

/// If squeezing every tab into the bar would leave fewer columns than this per
/// tab, the bar scrolls instead of shrinking labels further.
const FIT_MIN: usize = 8;
/// While scrolling, an inactive tab is never wider than this.
const SCROLL_LABEL_MAX: usize = 16;

pub struct TabBar;

/// One visible tab. `width` is what it occupies in the bar, including the
/// active tab's own caps. The first `index_len` bytes of `label` are the tab
/// number (drawn quieter than the name).
#[derive(Debug, Eq, PartialEq)]
struct Slot {
	index: usize,
	active: bool,
	label: String,
	index_len: usize,
	width: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct Layout {
	slots: Vec<Slot>,
	more_left: bool,
	more_right: bool,
}

impl Layout {
	fn empty() -> Self {
		Self { slots: Vec::new(), more_left: false, more_right: false }
	}
}

fn text_width(text: &str) -> usize {
	Line::from(text).width()
}

/// Decides which tabs are visible and how wide each one is. `render` and
/// `hit_test` both read this, so they cannot disagree about where a tab is.
///
/// - Everything fits: each label keeps its natural width.
/// - Too wide, but each tab can keep at least `FIT_MIN` columns: labels are
///   capped at one shared width, so short names stay whole and only long
///   ones are truncated.
/// - Otherwise the bar scrolls: a run of tabs around the active one is shown
///   and a `‹` / `›` marks each side that hides more. The window depends only
///   on the active tab and the width, so no scroll position is stored.
fn layout(width: u16, tabs: &[(bool, String)]) -> Layout {
	let caps = text_width(OPEN) + text_width(CLOSE);
	let width = width as usize;
	// The bar's own caps and the active tab's caps are always drawn.
	let Some(avail) = width.checked_sub(caps * 2).filter(|avail| *avail > 0) else { return Layout::empty() };
	if tabs.is_empty() {
		return Layout::empty();
	}

	let labels: Vec<String> = tabs.iter().enumerate().map(|(index, (_, name))| format!(" {} {name} ", index + 1)).collect();
	let natural: Vec<usize> = labels.iter().map(|label| text_width(label)).collect();
	let n = tabs.len();

	let cap = fit_cap(&natural, avail);
	let fits = natural.iter().sum::<usize>() <= avail;
	if fits || cap >= FIT_MIN {
		let slots = (0..n).map(|i| slot(i, tabs, &labels[i], natural[i].min(cap), caps)).collect();
		return Layout { slots, more_left: false, more_right: false };
	}

	let active = tabs.iter().position(|(active, _)| *active).unwrap_or(0);
	let widths: Vec<usize> = (0..n)
		.map(|i| if i == active { natural[i].min((avail / 2).max(FIT_MIN)).min(avail) } else { natural[i].min(SCROLL_LABEL_MAX) })
		.collect();
	let (mut first, mut last, mut used) = (active, active, widths[active]);
	loop {
		let mut grew = false;
		if last + 1 < n && used + widths[last + 1] <= avail {
			last += 1;
			used += widths[last];
			grew = true;
		}
		if first > 0 && used + widths[first - 1] <= avail {
			first -= 1;
			used += widths[first];
			grew = true;
		}
		if !grew {
			break;
		}
	}
	Layout {
		slots: (first..=last).map(|i| slot(i, tabs, &labels[i], widths[i], caps)).collect(),
		more_left: first > 0,
		more_right: last + 1 < n,
	}
}

fn slot(index: usize, tabs: &[(bool, String)], label: &str, max: usize, caps: usize) -> Slot {
	let active = tabs[index].0;
	let label = truncate(label.to_owned(), max);
	let width = text_width(&label) + if active { caps } else { 0 };
	// Truncation only ever shortens the name, but a label squeezed below the
	// number itself is then all number.
	let number = format!(" {} ", index + 1);
	let index_len = if label.starts_with(&number) { number.len() } else { label.len() };
	Slot { index, active, label, index_len, width }
}

/// The largest shared label width such that no label exceeds it and the total
/// fits in `avail`. Labels already narrower than it are left alone.
fn fit_cap(natural: &[usize], avail: usize) -> usize {
	let total = |cap: usize| natural.iter().map(|width| (*width).min(cap)).sum::<usize>();
	let (mut low, mut high) = (0, natural.iter().copied().max().unwrap_or(0));
	if total(high) <= avail {
		return high;
	}
	while low < high {
		let mid = (low + high).div_ceil(2);
		if total(mid) <= avail {
			low = mid;
		} else {
			high = mid - 1;
		}
	}
	low
}

impl TabBar {
	pub fn hit_test(area: Rect, tabs: &[(bool, String)], x: u16) -> Option<usize> {
		if tabs.is_empty() || x < area.x || x >= area.right() {
			return None;
		}
		let layout = layout(area.width, tabs);
		let mut column = area.x as usize + text_width(if layout.more_left { MORE_LEFT } else { OPEN });
		let x = x as usize;
		for slot in &layout.slots {
			if x >= column && x < column + slot.width {
				return Some(slot.index);
			}
			column += slot.width;
		}
		None
	}

	/// Takes owned labels so the caller can release its shared borrow of all
	/// tabs before borrowing the active tab mutably for the rest of a frame.
	pub fn render(frame: &mut Frame, area: Rect, tabs: &[(bool, String)], theme: &Theme) {
		if tabs.is_empty() || area.width == 0 {
			return;
		}

		let layout = layout(area.width, tabs);
		if layout.slots.is_empty() {
			return;
		}
		let active = theme.style("tabs.active");
		let inactive = theme.style("tabs.inactive");
		let separator = theme.style("tabs.separator");
		let outer = theme.style("tabs.outer");
		// Patched onto the tab's own style, so the number keeps its tab's
		// background and only the foreground (and boldness) differs.
		let index_active = active.patch(theme.style("tabs.index_active"));
		let index_inactive = inactive.patch(theme.style("tabs.index_inactive"));

		let mut spans = Vec::with_capacity(layout.slots.len() * 4 + 2);
		spans.push(Span::styled(if layout.more_left { MORE_LEFT } else { OPEN }, outer));
		for slot in layout.slots {
			let (number, name) = slot.label.split_at(slot.index_len);
			let (number, name) = (number.to_owned(), name.to_owned());
			if slot.active {
				spans.push(Span::styled(OPEN, separator));
				spans.push(Span::styled(number, index_active));
				spans.push(Span::styled(name, active));
				spans.push(Span::styled(CLOSE, separator));
			} else {
				spans.push(Span::styled(number, index_inactive));
				spans.push(Span::styled(name, inactive));
			}
		}
		spans.push(Span::styled(if layout.more_right { MORE_RIGHT } else { CLOSE }, outer));
		frame.render_widget(Paragraph::new(Line::from(spans)), area);
	}
}

fn truncate(text: String, max: usize) -> String {
	if Line::from(text.as_str()).width() <= max {
		return text;
	}
	if max == 0 {
		return String::new();
	}

	let target = max.saturating_sub(1);
	let mut out = String::new();
	for ch in text.chars() {
		if Line::from(out.as_str()).width() + Line::from(ch.to_string()).width() > target {
			break;
		}
		out.push(ch);
	}
	out.push('…');
	out
}

#[cfg(test)]
mod tests {
	use ratatui::{Terminal, backend::TestBackend};

	use super::*;

	fn tabs(count: usize, active: usize, name: impl Fn(usize) -> String) -> Vec<(bool, String)> {
		(0..count).map(|i| (i == active, name(i))).collect()
	}

	/// Names of every flavour: short, long, and double-width.
	fn mixed_name(i: usize) -> String {
		match i % 4 {
			0 => "src".into(),
			1 => "a-rather-long-directory-name".into(),
			2 => "配置文件目录".into(),
			_ => format!("t{i}"),
		}
	}

	fn used_width(layout: &Layout) -> usize {
		let left = if layout.more_left { MORE_LEFT } else { OPEN };
		let right = if layout.more_right { MORE_RIGHT } else { CLOSE };
		text_width(left) + layout.slots.iter().map(|slot| slot.width).sum::<usize>() + text_width(right)
	}

	#[test]
	fn hit_test_tracks_rendered_tab_widths() {
		let tabs = vec![(true, "one".into()), (false, "two".into()), (false, "three".into())];
		let area = Rect::new(10, 2, 60, 1);
		assert_eq!(TabBar::hit_test(area, &tabs, 12), Some(0));
		assert_eq!(TabBar::hit_test(area, &tabs, 20), Some(1));
		assert_eq!(TabBar::hit_test(area, &tabs, 28), Some(2));
		assert_eq!(TabBar::hit_test(area, &tabs, 69), None);
	}

	#[test]
	fn the_markers_take_exactly_the_room_of_the_caps_they_replace() {
		assert_eq!(text_width(MORE_LEFT), text_width(OPEN));
		assert_eq!(text_width(MORE_RIGHT), text_width(CLOSE));
	}

	#[test]
	fn tabs_that_fit_keep_their_natural_width() {
		let tabs = tabs(3, 0, |i| ["a", "a-fairly-long-name", "b"][i].into());
		let layout = layout(80, &tabs);
		assert!(!layout.more_left && !layout.more_right);
		let labels: Vec<_> = layout.slots.iter().map(|slot| slot.label.as_str()).collect();
		assert_eq!(labels, [" 1 a ", " 2 a-fairly-long-name ", " 3 b "], "nothing is shortened while there is room");
	}

	#[test]
	fn a_shared_cap_shortens_only_the_long_names() {
		let tabs = tabs(3, 1, |i| ["a", "an-extremely-long-directory-name-here", "b"][i].into());
		let layout = layout(30, &tabs);
		assert!(!layout.more_left && !layout.more_right, "still one screenful, so no scrolling");
		assert_eq!(layout.slots[0].label, " 1 a ");
		assert_eq!(layout.slots[2].label, " 3 b ");
		assert!(layout.slots[1].label.ends_with('…'), "{:?}", layout.slots[1].label);
		assert!(used_width(&layout) <= 30);
		assert!(used_width(&layout) >= 28, "the long name takes the room the short ones leave: {}", used_width(&layout));
	}

	#[test]
	fn many_tabs_scroll_around_the_active_one() {
		let names = |i: usize| format!("dir{i}");
		let first = layout(60, &tabs(32, 0, names));
		assert!(!first.more_left && first.more_right, "at the start only the right side is hidden");
		let middle = layout(60, &tabs(32, 15, names));
		assert!(middle.more_left && middle.more_right);
		let last = layout(60, &tabs(32, 31, names));
		assert!(last.more_left && !last.more_right, "at the end only the left side is hidden");
		for layout in [&first, &middle, &last] {
			assert!(layout.slots.len() >= 3, "a useful run is shown, not a single tab");
			assert!(used_width(layout) <= 60);
		}
		assert!(middle.slots.iter().any(|slot| slot.index == 15 && slot.active));
	}

	#[test]
	fn the_scroll_window_depends_only_on_the_active_tab_and_width() {
		let names = |i: usize| format!("dir{i}");
		assert_eq!(layout(60, &tabs(32, 15, names)), layout(60, &tabs(32, 15, names)));
		let a = layout(60, &tabs(32, 15, names));
		let b = layout(60, &tabs(32, 16, names));
		assert_ne!(a.slots.first().map(|s| s.index), b.slots.first().map(|s| s.index), "the window follows the active tab");
	}

	#[test]
	fn an_overlong_active_tab_still_leaves_room_for_neighbours() {
		let tabs = tabs(20, 10, |i| if i == 10 { "x".repeat(200) } else { format!("d{i}") });
		let layout = layout(60, &tabs);
		let active = layout.slots.iter().find(|slot| slot.active).unwrap();
		assert!(active.label.starts_with(" 11 "), "the index stays visible: {:?}", active.label);
		assert!(active.label.ends_with('…'));
		assert!(layout.slots.len() >= 3, "neighbours are shown next to it");
		assert!(used_width(&layout) <= 60);
	}

	#[test]
	fn the_bar_never_overflows_and_always_shows_the_active_tab() {
		for width in 0..=120u16 {
			for count in 0..=40usize {
				for active in [0, count / 2, count.saturating_sub(1)] {
					let tabs = tabs(count, active, mixed_name);
					let layout = layout(width, &tabs);
					if layout.slots.is_empty() {
						continue;
					}
					assert!(used_width(&layout) <= width as usize, "width {width}, {count} tabs, active {active}: uses {}", used_width(&layout));
					assert!(layout.slots.iter().any(|slot| slot.index == active && slot.active), "width {width}, {count} tabs, active {active}");
					let indexes: Vec<_> = layout.slots.iter().map(|slot| slot.index).collect();
					assert!(indexes.windows(2).all(|pair| pair[1] == pair[0] + 1), "the visible tabs are contiguous: {indexes:?}");
					assert_eq!(layout.more_left, indexes[0] > 0);
					assert_eq!(layout.more_right, *indexes.last().unwrap() + 1 < count);
				}
			}
		}
	}

	#[test]
	fn a_bar_too_narrow_to_draw_anything_draws_nothing() {
		assert!(layout(0, &tabs(3, 0, mixed_name)).slots.is_empty());
		assert!(layout(4, &tabs(3, 0, mixed_name)).slots.is_empty());
		assert!(layout(80, &[]).slots.is_empty());
	}

	#[test]
	fn clicks_land_on_the_tab_that_was_drawn_there() {
		for width in [12u16, 30, 60, 100] {
			for count in [1usize, 5, 12, 32] {
				for active in [0, count / 2, count - 1] {
					let tabs = tabs(count, active, mixed_name);
					let area = Rect::new(3, 0, width, 1);
					let layout = layout(width, &tabs);
					let mut column = 3 + text_width(if layout.more_left { MORE_LEFT } else { OPEN }) as u16;
					assert_eq!(TabBar::hit_test(area, &tabs, area.x), None, "the left cap is not a tab");
					for slot in &layout.slots {
						for x in column..column + slot.width as u16 {
							assert_eq!(TabBar::hit_test(area, &tabs, x), Some(slot.index), "width {width}, {count} tabs, active {active}, column {x}");
						}
						column += slot.width as u16;
					}
					for x in column..area.right() {
						assert_eq!(TabBar::hit_test(area, &tabs, x), None, "the right cap and the empty rest are not tabs");
					}
					assert_eq!(TabBar::hit_test(area, &tabs, area.right()), None);
				}
			}
		}
	}

	#[test]
	fn rendering_matches_the_layout_column_for_column() {
		let theme = Theme::default();
		for (width, count, active) in [(60u16, 5usize, 2usize), (60, 32, 15), (30, 32, 0), (30, 32, 31), (100, 12, 6)] {
			let tabs = tabs(count, active, mixed_name);
			let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
			terminal.draw(|frame| TabBar::render(frame, frame.area(), &tabs, &theme)).unwrap();
			// A double-width symbol owns two cells; the second one only holds a blank.
			let mut row = String::new();
			let mut x = 0;
			while x < width {
				let symbol = terminal.backend().buffer()[(x, 0)].symbol().to_owned();
				x += text_width(&symbol).max(1) as u16;
				row.push_str(&symbol);
			}
			let layout = layout(width, &tabs);
			for slot in &layout.slots {
				assert!(row.contains(slot.label.trim_end_matches('…')), "width {width}: {:?} missing from {row:?}", slot.label);
			}
			assert_eq!(row.contains(MORE_LEFT), layout.more_left, "width {width}, {count} tabs, active {active}");
			assert_eq!(row.contains(MORE_RIGHT), layout.more_right, "width {width}, {count} tabs, active {active}");
			assert_eq!(row.contains(OPEN), true, "the active tab's cap is drawn");
		}
	}

	#[test]
	fn the_number_is_split_from_the_name_even_when_the_name_is_cut() {
		let full = layout(80, &tabs(3, 0, |i| ["src", "docs", "tests"][i].into()));
		for slot in &full.slots {
			assert_eq!(&slot.label[..slot.index_len], format!(" {} ", slot.index + 1));
		}
		let cut = layout(30, &tabs(3, 1, |i| ["a", "an-extremely-long-directory-name-here", "b"][i].into()));
		let long = &cut.slots[1];
		assert!(long.label.ends_with('…'));
		assert_eq!(&long.label[..long.index_len], " 2 ", "truncation only shortens the name");
	}

	#[test]
	fn a_label_squeezed_to_its_number_keeps_the_number_whole() {
		// Room for the number and an ellipsis: the ellipsis is the name part.
		let slot = &layout(8, &tabs(1, 0, |_| "name".into())).slots[0];
		assert_eq!(slot.label, " 1 …");
		assert_eq!(&slot.label[..slot.index_len], " 1 ");
	}

	#[test]
	fn a_label_squeezed_below_its_number_is_all_number() {
		// Not even the number fits, so there is no name part at all.
		let slot = &layout(6, &tabs(1, 0, |_| "name".into())).slots[0];
		assert_eq!(slot.index_len, slot.label.len());
		assert_eq!(&slot.label[slot.index_len..], "");
	}

	#[test]
	fn numbers_are_quieter_than_names_and_keep_their_tabs_background() {
		let theme = Theme::default();
		let tabs = tabs(3, 1, |i| ["one", "two", "three"][i].into());
		let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
		terminal.draw(|frame| TabBar::render(frame, frame.area(), &tabs, &theme)).unwrap();
		let buffer = terminal.backend().buffer();

		let layout = layout(60, &tabs);
		let mut column = text_width(OPEN) as u16;
		for slot in &layout.slots {
			let label_start = column + if slot.active { text_width(OPEN) as u16 } else { 0 };
			let digit = &buffer[(label_start + 1, 0)];
			let name = &buffer[(label_start + slot.index_len as u16, 0)];
			let (tab_style, index_style) = if slot.active {
				(theme.style("tabs.active"), theme.style("tabs.index_active"))
			} else {
				(theme.style("tabs.inactive"), theme.style("tabs.index_inactive"))
			};
			assert_eq!(digit.symbol(), (slot.index + 1).to_string(), "tab {}: this is the number cell", slot.index);
			assert_eq!(digit.fg, index_style.fg.unwrap(), "tab {}: number uses the index foreground", slot.index);
			assert_eq!(name.fg, tab_style.fg.unwrap(), "tab {}: name keeps the tab foreground", slot.index);
			assert_ne!(digit.fg, name.fg, "tab {}: the number is visibly quieter", slot.index);
			assert_eq!(digit.bg, name.bg, "tab {}: both sit on the tab's own background", slot.index);
			assert_eq!(digit.bg, tab_style.bg.unwrap(), "tab {}", slot.index);
			// Boldness comes from the tab: the active tab is bold, number and name alike.
			let bold = ratatui::style::Modifier::BOLD;
			assert_eq!(digit.modifier.contains(bold), slot.active, "tab {}: the number is bold exactly when its tab is", slot.index);
			assert_eq!(digit.modifier.contains(bold), name.modifier.contains(bold), "tab {}: number and name share boldness", slot.index);
			column += slot.width as u16;
		}
	}
}
