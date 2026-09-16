use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Key {
	pub code:      KeyCode,
	pub modifiers: KeyModifiers,
}

impl Key {
	pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self { Self { code, modifiers } }

	pub fn plain(code: KeyCode) -> Self { Self::new(code, KeyModifiers::NONE) }

	pub fn char(c: char) -> Self { Self::plain(KeyCode::Char(c)) }
}

impl From<KeyEvent> for Key {
	fn from(event: KeyEvent) -> Self {
		let mut modifiers = event.modifiers;
		// Terminals disagree on whether Shift remains set once a printable
		// character has already been shifted (`v` -> `V`). The character is
		// authoritative, so normalize that redundant modifier away.
		if matches!(event.code, KeyCode::Char(_)) {
			modifiers.remove(KeyModifiers::SHIFT);
		}
		Self::new(event.code, modifiers)
	}
}

impl fmt::Display for Key {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		if self.modifiers.is_empty() {
			return match self.code {
				KeyCode::Char(' ') => f.write_str("<Space>"),
				KeyCode::Char(c) => write!(f, "{c}"),
				_ => write!(f, "<{:?}>", self.code),
			};
		}

		f.write_str("<")?;
		if self.modifiers.contains(KeyModifiers::CONTROL) {
			f.write_str("C-")?;
		}
		if self.modifiers.contains(KeyModifiers::ALT) {
			f.write_str("A-")?;
		}
		if self.modifiers.contains(KeyModifiers::SUPER) {
			f.write_str("D-")?;
		}
		match self.code {
			KeyCode::Char(c) => write!(f, "{c}>"),
			_ => write!(f, "{:?}>", self.code),
		}
	}
}
