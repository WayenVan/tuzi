use ratatui::style::{Color, Style};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusMode {
	Normal,
	Select,
	Unset,
}

impl StatusMode {
	pub fn color(self) -> Color {
		match self {
			Self::Normal => Color::Rgb(0x89, 0xb4, 0xfa),
			Self::Select => Color::Rgb(0x94, 0xe2, 0xd5),
			Self::Unset => Color::Rgb(0xf2, 0xcd, 0xcd),
		}
	}

	pub const fn alt_background(self) -> Color { Color::Rgb(0x31, 0x32, 0x44) }

	pub fn label(self) -> &'static str {
		match self {
			Self::Normal => "NOR",
			Self::Select => "SEL",
			Self::Unset => "UNS",
		}
	}

	pub fn style(self) -> Style {
		Style::new().fg(Color::Rgb(0x1e, 0x1e, 0x2e)).bg(self.color())
	}

	pub fn alt_style(self) -> Style { Style::new().fg(self.color()).bg(self.alt_background()) }
}

pub fn permission_style(character: char) -> Style {
	let color = match character {
		'-' | '?' => Color::Rgb(0x7f, 0x84, 0x9c),
		'r' => Color::Rgb(0xf9, 0xe2, 0xaf),
		'w' => Color::Rgb(0xf3, 0x8b, 0xa8),
		'x' | 's' | 'S' | 't' | 'T' => Color::Rgb(0xa6, 0xe3, 0xa1),
		_ => Color::Rgb(0x89, 0xb4, 0xfa),
	};
	Style::new().fg(color)
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

	#[test]
	fn position_uses_vim_style_edge_labels_and_percentages() {
		assert_eq!(position_labels(0, 0), ("Top".into(), " 0/0 ".into()));
		assert_eq!(position_labels(0, 101), ("Top".into(), " 1/101".into()));
		assert_eq!(position_labels(41, 100), ("42%".into(), "42/100".into()));
		assert_eq!(position_labels(100, 101), ("Bot".into(), "101/101".into()));
	}

	#[test]
	fn permissions_use_the_mocha_yazi_palette() {
		assert_eq!(permission_style('-').fg, Some(Color::Rgb(0x7f, 0x84, 0x9c)));
		assert_eq!(permission_style('r').fg, Some(Color::Rgb(0xf9, 0xe2, 0xaf)));
		assert_eq!(permission_style('w').fg, Some(Color::Rgb(0xf3, 0x8b, 0xa8)));
		assert_eq!(permission_style('x').fg, Some(Color::Rgb(0xa6, 0xe3, 0xa1)));
		assert_eq!(permission_style('d').fg, Some(Color::Rgb(0x89, 0xb4, 0xfa)));
	}
}
