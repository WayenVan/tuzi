use devicons::{Theme, icon_for_file};
use ratatui::style::{Color, Style};

use crate::core::Node;

#[derive(Clone, Copy)]
pub struct Icon {
	pub text: char,
	pub style: Style,
}

#[derive(Default)]
pub struct IconTheme;

impl IconTheme {
	pub fn icon_for(&self, node: &Node) -> Icon {
		if node.cha.is_link {
			let color = if node.cha.link_broken { Color::Rgb(0xf4, 0x43, 0x36) } else { Color::Rgb(0x9e, 0x9e, 0x9e) };
			return Icon::new('', color);
		}
		if node.cha.is_dir {
			return if node.expanded {
				Icon::new('', Color::Rgb(0x03, 0xa9, 0xf4))
			} else {
				Icon::new('', Color::Rgb(0x03, 0xa9, 0xf4))
			};
		}

		let matched = icon_for_file(&node.path, &Some(Theme::Dark));
		if matched.icon == '*' {
			Icon::new('', Color::White)
		} else {
			Icon::new(matched.icon, parse_hex(matched.color).unwrap_or(Color::White))
		}
	}
}

impl Icon {
	fn new(text: char, color: Color) -> Self {
		Self { text, style: Style::new().fg(color) }
	}
}

fn parse_hex(value: &str) -> Option<Color> {
	let value = value.strip_prefix('#')?;
	if value.len() != 6 {
		return None;
	}
	let red = u8::from_str_radix(&value[0..2], 16).ok()?;
	let green = u8::from_str_radix(&value[2..4], 16).ok()?;
	let blue = u8::from_str_radix(&value[4..6], 16).ok()?;
	Some(Color::Rgb(red, green, blue))
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
		let theme = IconTheme;
		assert_eq!(theme.icon_for(&node("src", true, false, false)).text, '');
		assert_eq!(theme.icon_for(&node("src", true, false, true)).text, '');
	}

	#[test]
	fn files_use_devicons_with_a_pretty_fallback() {
		let theme = IconTheme;
		assert_ne!(theme.icon_for(&node("main.rs", false, false, false)).text, '');
		assert_eq!(theme.icon_for(&node("unknown-file", false, false, false)).text, '');
	}

	#[test]
	fn a_broken_link_gets_a_red_icon_instead_of_the_usual_grey() {
		let theme = IconTheme;
		let mut broken = node("dangling-link", false, true, false);
		broken.cha.link_broken = true;
		let live = node("live-link", false, true, false);

		assert_eq!(theme.icon_for(&broken).style, Style::new().fg(Color::Rgb(0xf4, 0x43, 0x36)));
		assert_eq!(theme.icon_for(&live).style, Style::new().fg(Color::Rgb(0x9e, 0x9e, 0x9e)));
	}
}
