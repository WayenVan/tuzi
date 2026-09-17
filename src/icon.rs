use devicons::{Theme, icon_for_file};
use ratatui::style::Style;

use crate::{core::Node, theme::{IconConfig, IconStyle}};

#[derive(Clone)]
pub struct Icon {
	pub text: String,
	pub style: Style,
}

pub struct IconTheme {
	config: IconConfig,
}

impl IconTheme {
	pub fn new(config: IconConfig) -> Self { Self { config } }

	pub fn icon_for(&self, node: &Node) -> Option<Icon> {
		if !self.config.enabled { return None; }
		let path = node.path.to_string_lossy();
		if let Some(rule) = self.config.globs.iter().find(|rule| glob_matches(&rule.pattern, &path)) {
			return Some(Icon::from(&rule.icon));
		}
		let name = node.path.file_name().map(|name| name.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
		let named = if node.cha.is_dir { &self.config.dirs } else { &self.config.files };
		if let Some(rule) = named.iter().find(|rule| rule.name == name) { return Some(Icon::from(&rule.icon)); }
		if !node.cha.is_dir {
			let extension = node.path.extension().map(|value| value.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
			if let Some(rule) = self.config.exts.iter().find(|rule| rule.name == extension) { return Some(Icon::from(&rule.icon)); }
		}
		if node.cha.is_link { return Some(Icon::from(if node.cha.link_broken { &self.config.broken_symlink } else { &self.config.symlink })); }
		if node.cha.is_dir { return Some(Icon::from(if node.expanded { &self.config.directory_open } else { &self.config.directory })); }
		let matched = icon_for_file(&node.path, &Some(Theme::Dark));
		if matched.icon == '*' {
			Some(Icon::from(&self.config.fallback))
		} else {
			Some(Icon { text: matched.icon.to_string(), style: Style::new().fg(crate::theme::parse_color(matched.color).unwrap_or(self.config.fallback.color)) })
		}
	}
}

impl Default for IconTheme {
	fn default() -> Self { Self::new(crate::theme::Theme::default().icon) }
}

impl From<&IconStyle> for Icon {
	fn from(value: &IconStyle) -> Self { Self { text: value.text.clone(), style: Style::new().fg(value.color) } }
}

fn glob_matches(pattern: &str, value: &str) -> bool {
	let pattern: Vec<char> = pattern.chars().collect();
	let value: Vec<char> = value.chars().collect();
	let mut reachable = vec![false; value.len() + 1];
	reachable[0] = true;
	for token in pattern {
		if token == '*' {
			for index in 1..=value.len() { reachable[index] |= reachable[index - 1]; }
		} else {
			for index in (1..=value.len()).rev() { reachable[index] = reachable[index - 1] && (token == '?' || token == value[index - 1]); }
			reachable[0] = false;
		}
	}
	reachable[value.len()]
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use crate::fs::Cha;

	use super::*;

	fn node(path: &str, is_dir: bool, is_link: bool, expanded: bool) -> Node {
		Node {
			path: PathBuf::from(path),
			cha: Cha {
				len: 0,
				is_dir,
				is_link,
				link_target: None,
				link_broken: false,
				modified: None,
				mode: 0,
			},
			expanded,
			children: None,
			loading: false,
			load_error: None,
		}
	}

	#[test]
	fn directories_use_open_and_closed_icons() {
		let theme = IconTheme::default();
		assert_eq!(theme.icon_for(&node("src", true, false, false)).unwrap().text, "");
		assert_eq!(theme.icon_for(&node("src", true, false, true)).unwrap().text, "");
	}

	#[test]
	fn files_use_devicons_with_a_pretty_fallback() {
		let theme = IconTheme::default();
		assert_ne!(theme.icon_for(&node("main.rs", false, false, false)).unwrap().text, "");
		assert_eq!(theme.icon_for(&node("unknown-file", false, false, false)).unwrap().text, "");
	}

	#[test]
	fn a_broken_link_gets_a_red_icon_instead_of_the_usual_grey() {
		let theme = IconTheme::default();
		let mut broken = node("dangling-link", false, true, false);
		broken.cha.link_broken = true;
		let live = node("live-link", false, true, false);

		assert_eq!(theme.icon_for(&broken).unwrap().style, Style::new().fg(ratatui::style::Color::Rgb(0xf4, 0x43, 0x36)));
		assert_eq!(theme.icon_for(&live).unwrap().style, Style::new().fg(ratatui::style::Color::Rgb(0x9e, 0x9e, 0x9e)));
	}

	#[test]
	fn wildcard_matching_supports_globs() {
		assert!(glob_matches("*/src/*.rs", "/tmp/src/main.rs"));
		assert!(!glob_matches("*.toml", "main.rs"));
	}

	#[test]
	fn configured_rules_win_over_devicons_and_icons_can_be_disabled() {
		let mut config = crate::theme::Theme::default().icon;
		config.exts.insert(0, crate::theme::NameRule { name: "rs".into(), icon: crate::theme::IconStyle { text: "RUST".into(), color: ratatui::style::Color::Red } });
		let theme = IconTheme::new(config.clone());
		assert_eq!(theme.icon_for(&node("main.rs", false, false, false)).unwrap().text, "RUST");
		config.enabled = false;
		assert!(IconTheme::new(config).icon_for(&node("main.rs", false, false, false)).is_none());
	}
}
