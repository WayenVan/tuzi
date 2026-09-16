use std::{env, path::{Path, PathBuf}};

use ratatui::{Frame, layout::Rect, style::{Color, Style}, widgets::Paragraph};

pub struct WinBar;

impl WinBar {
	/// Mirrors yazi's header cwd: the active tab's absolute root path sits
	/// above the tab strip and is clipped by the terminal at the right edge.
	pub fn render(frame: &mut Frame, area: Rect, path: &Path) {
		let path = pretty_path(path, home_dir().as_deref());
		frame.render_widget(Paragraph::new(path).style(Style::new().fg(Color::Cyan)), area);
	}
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
}
