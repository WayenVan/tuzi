use std::{env, path::{Path, PathBuf}};

use ratatui::{Frame, layout::Rect, style::{Color, Style}, text::{Line, Span}, widgets::Paragraph};

pub struct WinBar;

pub struct WinBarState<'a> {
	pub path:   &'a Path,
	pub finder: Option<&'a str>,
	pub filter: Option<&'a str>,
}

impl WinBar {
	/// Mirrors yazi's header cwd: the active tab's absolute root path sits
	/// above the tab strip and is clipped by the terminal at the right edge.
	pub fn render(frame: &mut Frame, area: Rect, state: WinBarState<'_>) {
		let path = pretty_path(state.path, home_dir().as_deref());
		let suffix = flags(state.finder, state.filter);
		let (path, suffix) = fit(&path, &suffix, area.width as usize);
		frame.render_widget(Paragraph::new(Line::from(vec![
			Span::styled(path, Style::new().fg(Color::Cyan)),
			Span::styled(suffix, Style::new().fg(Color::Cyan)),
		])), area);
	}
}

fn flags(finder: Option<&str>, filter: Option<&str>) -> String {
	let mut flags = Vec::new();
	if let Some(query) = filter { flags.push(format!("filter: {query}")); }
	if let Some(query) = finder { flags.push(format!("find: {query}")); }
	if flags.is_empty() { String::new() } else { format!(" ({})", flags.join(", ")) }
}

fn fit(path: &str, suffix: &str, width: usize) -> (String, String) {
	let suffix_width = display_width(suffix);
	if suffix_width >= width {
		return (String::new(), take_left(suffix, width));
	}
	let available = width - suffix_width;
	if display_width(path) <= available {
		(path.to_owned(), suffix.to_owned())
	} else {
		(format!("…{}", take_right(path, available.saturating_sub(1))), suffix.to_owned())
	}
}

fn display_width(value: &str) -> usize { Line::from(value).width() }

fn take_left(value: &str, width: usize) -> String {
	let mut out = String::new();
	for ch in value.chars() {
		if display_width(&out) + display_width(&ch.to_string()) > width { break }
		out.push(ch);
	}
	out
}

fn take_right(value: &str, width: usize) -> String {
	let mut chars = Vec::new();
	let mut used = 0;
	for ch in value.chars().rev() {
		let w = display_width(&ch.to_string());
		if used + w > width { break }
		chars.push(ch);
		used += w;
	}
	chars.into_iter().rev().collect()
}

fn home_dir() -> Option<PathBuf> {
	env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")).map(PathBuf::from)
}

fn pretty_path(path: &Path, home: Option<&Path>) -> String {
	let Some(home) = home else { return path.display().to_string() };
	let Ok(relative) = path.strip_prefix(home) else { return path.display().to_string() };
	if relative.as_os_str().is_empty() {
		"~".to_owned()
	} else {
		format!("~{}{}", std::path::MAIN_SEPARATOR, relative.display())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn abbreviates_home_and_its_descendants_only() {
		let home = Path::new("/home/alice");
		assert_eq!(pretty_path(home, Some(home)), "~");
		assert_eq!(pretty_path(Path::new("/home/alice/projects/tuzi"), Some(home)), "~/projects/tuzi");
		assert_eq!(pretty_path(Path::new("/home/alice2"), Some(home)), "/home/alice2");
		assert_eq!(pretty_path(Path::new("/tmp"), None), "/tmp");
	}

	#[test]
	fn formats_find_and_filter_flags_like_yazi() {
		assert_eq!(flags(None, None), "");
		assert_eq!(flags(Some("task"), None), " (find: task)");
		assert_eq!(flags(None, Some("rs")), " (filter: rs)");
		assert_eq!(flags(Some("任务"), Some("源")), " (filter: 源, find: 任务)");
	}

	#[test]
	fn narrow_width_keeps_the_status_suffix() {
		assert_eq!(fit("~/projects/tuzi", " (filter: rs)", 20), ("…s/tuzi".into(), " (filter: rs)".into()));
	}
}
