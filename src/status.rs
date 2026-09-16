use ratatui::style::{Color, Style};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusMode {
	Normal,
	Select,
	Unset,
}

impl StatusMode {
	pub fn label(self) -> &'static str {
		match self {
			Self::Normal => "NORM",
			Self::Select => "SEL",
			Self::Unset => "UNS",
		}
	}

	pub fn style(self) -> Style {
		match self {
			Self::Normal => Style::new().fg(Color::Black).bg(Color::Blue),
			Self::Select => Style::new().fg(Color::Black).bg(Color::Cyan),
			Self::Unset => Style::new().fg(Color::White).bg(Color::Red),
		}
	}
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
		Self { mode, name: String::new(), size: String::new(), permissions: String::new(), error: None }
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
