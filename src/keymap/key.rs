use std::{fmt, str::FromStr};

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

impl FromStr for Key {
	type Err = String;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		if !value.starts_with('<') || !value.ends_with('>') {
			let mut chars = value.chars();
			let Some(character) = chars.next() else { return Err("key cannot be empty".into()) };
			if chars.next().is_some() { return Err(format!("plain key must be one character: {value}")); }
			return Ok(Self::char(character));
		}

		let inner = &value[1..value.len() - 1];
		if inner.eq_ignore_ascii_case("space") { return Ok(Self::char(' ')); }
		let parts: Vec<_> = inner.split('-').collect();
		let (name, modifiers) = parts.split_last().ok_or_else(|| format!("invalid key: {value}"))?;
		let mut mods = KeyModifiers::NONE;
		for modifier in modifiers {
			match modifier.to_ascii_lowercase().as_str() {
				"c" | "ctrl" => mods |= KeyModifiers::CONTROL,
				"a" | "alt" => mods |= KeyModifiers::ALT,
				"s" | "shift" => mods |= KeyModifiers::SHIFT,
				"d" | "super" => mods |= KeyModifiers::SUPER,
				_ => return Err(format!("unknown modifier '{modifier}' in {value}")),
			}
		}
		let lower = name.to_ascii_lowercase();
		let mut code = match lower.as_str() {
			"esc" | "escape" => KeyCode::Esc,
			"enter" => KeyCode::Enter,
			"tab" => KeyCode::Tab,
			"backtab" => KeyCode::BackTab,
			"backspace" => KeyCode::Backspace,
			"delete" => KeyCode::Delete,
			"insert" => KeyCode::Insert,
			"up" => KeyCode::Up,
			"down" => KeyCode::Down,
			"left" => KeyCode::Left,
			"right" => KeyCode::Right,
			"home" => KeyCode::Home,
			"end" => KeyCode::End,
			"pageup" => KeyCode::PageUp,
			"pagedown" => KeyCode::PageDown,
			_ if lower.starts_with('f') && lower[1..].parse::<u8>().is_ok_and(|number| (1..=24).contains(&number)) => KeyCode::F(lower[1..].parse().unwrap()),
			_ => {
				let mut chars = name.chars();
				let Some(character) = chars.next() else { return Err(format!("missing key code in {value}")) };
				if chars.next().is_some() { return Err(format!("unknown key code '{name}' in {value}")); }
				KeyCode::Char(character)
			},
		};
		if let KeyCode::Char(character) = &mut code {
			if mods.contains(KeyModifiers::SHIFT) { *character = character.to_ascii_uppercase(); }
			mods.remove(KeyModifiers::SHIFT);
		}
		Ok(Self::new(code, mods))
	}
}
