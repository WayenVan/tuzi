use ratatui::style::{Color, Style};

use crate::theme::Theme;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusMode {
	Normal,
	Select,
	Unset,
}

impl StatusMode {
	pub fn color(self, theme: &Theme) -> Color {
		self.style(theme).bg.unwrap_or(Color::Reset)
	}

	pub fn alt_background(self, theme: &Theme) -> Color { self.alt_style(theme).bg.unwrap_or(Color::Reset) }

	pub fn label(self) -> &'static str {
		match self {
			Self::Normal => "NOR",
			Self::Select => "SEL",
			Self::Unset => "UNS",
		}
	}

	pub fn style(self, theme: &Theme) -> Style {
		theme.style(match self { Self::Normal => "status.normal", Self::Select => "status.select", Self::Unset => "status.unset" })
	}

	pub fn alt_style(self, theme: &Theme) -> Style {
		theme.style(match self { Self::Normal => "status.normal_alt", Self::Select => "status.select_alt", Self::Unset => "status.unset_alt" })
	}
}

pub fn permission_style(character: char, theme: &Theme) -> Style {
	theme.style(match character { '-' | '?' => "status.perm_none", 'r' => "status.perm_read", 'w' => "status.perm_write", 'x' | 's' | 'S' | 't' | 'T' => "status.perm_exec", _ => "status.perm_type" })
}

pub struct StatusLine {
	pub mode:        StatusMode,
	pub name:        String,
	pub size:        String,
	pub permissions: String,
	pub error:       Option<String>,
}

impl StatusLine {
	pub fn empty(mode: StatusMode) -> Self {
		Self { mode, name: String::new(), size: "0B".into(), permissions: String::new(), error: None }
	}
}

/// One already-styled piece of a status-style bar. Callers decide what
/// appears and in what order — `StatusBar` just lays `left` out flush-left
/// and `right` flush-right, in the order each list is given. Adding,
/// reordering, or removing content is purely a matter of editing whatever
/// `Vec<Segment>` the caller builds; nothing here or in `StatusBar` needs
/// to change.
pub struct Segment {
	pub text:  String,
	pub style: Style,
}

impl Segment {
	pub fn new(text: impl Into<String>, style: Style) -> Self { Self { text: text.into(), style } }
}

pub fn position_labels(cursor: usize, visible_len: usize) -> (String, String) {
	let cursor = cursor.min(visible_len.saturating_sub(1));
	let percent = if cursor == 0 || visible_len == 0 { 0 } else { (cursor + 1) * 100 / visible_len };
	let percent = match percent {
		0 => "Top".into(),
		100 => "Bot".into(),
		percent => format!("{percent:>2}%"),
	};
	let current = (cursor + 1).min(visible_len);
	(percent, format!("{current:>2}/{visible_len:<2}"))
}

#[cfg(test)]
mod tests {
	use ratatui::style::Color;

	use super::{permission_style, position_labels};
	use crate::theme::Theme;

	#[test]
	fn position_uses_vim_style_edge_labels_and_percentages() {
		assert_eq!(position_labels(0, 0), ("Top".into(), " 0/0 ".into()));
		assert_eq!(position_labels(0, 101), ("Top".into(), " 1/101".into()));
		assert_eq!(position_labels(41, 100), ("42%".into(), "42/100".into()));
		assert_eq!(position_labels(100, 101), ("Bot".into(), "101/101".into()));
	}

	#[test]
	fn permissions_use_the_mocha_yazi_palette() {
		let theme = Theme::default();
		assert_eq!(permission_style('-', &theme).fg, Some(Color::Rgb(0x7f, 0x84, 0x9c)));
		assert_eq!(permission_style('r', &theme).fg, Some(Color::Rgb(0xf9, 0xe2, 0xaf)));
		assert_eq!(permission_style('w', &theme).fg, Some(Color::Rgb(0xf3, 0x8b, 0xa8)));
		assert_eq!(permission_style('x', &theme).fg, Some(Color::Rgb(0xa6, 0xe3, 0xa1)));
		assert_eq!(permission_style('d', &theme).fg, Some(Color::Rgb(0x89, 0xb4, 0xfa)));
	}
}
