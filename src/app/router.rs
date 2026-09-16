use crossterm::event::{KeyCode, KeyEventKind};

use crate::{core::InputMode, event::Event};

pub struct Router;

impl Router {
	/// `mode` is `Some` while a text prompt (e.g. rename) is open — every
	/// key routes to editing it instead of the tree, vim-modal style.
	pub fn route(term_event: crossterm::event::Event, mode: Option<InputMode>) -> Option<Event> {
		let crossterm::event::Event::Key(key) = term_event else { return None };
		if key.kind != KeyEventKind::Press {
			return None;
		}

		if let Some(mode) = mode {
			return Self::route_input(key.code, mode);
		}

		Some(match key.code {
			KeyCode::Char('q') => Event::Quit,
			KeyCode::Esc => Event::Escape,
			KeyCode::Char('j') | KeyCode::Down => Event::MoveDown,
			KeyCode::Char('k') | KeyCode::Up => Event::MoveUp,
			KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => Event::Expand,
			KeyCode::Char('h') | KeyCode::Left => Event::Collapse,
			KeyCode::Char(' ') => Event::ToggleSelect,
			KeyCode::Char('v') => Event::VisualSelect,
			KeyCode::Char('V') => Event::VisualUnset,
			KeyCode::Char('d') => Event::Delete,
			KeyCode::Char('y') => Event::Yank,
			KeyCode::Char('p') => Event::Paste,
			KeyCode::Char('r') => Event::Rename,
			_ => return None,
		})
	}

	fn route_input(code: KeyCode, mode: InputMode) -> Option<Event> {
		// Bindings shared by every mode.
		match code {
			KeyCode::Esc => return Some(Event::InputEscape),
			KeyCode::Enter => return Some(Event::InputConfirm),
			_ => {}
		}

		Some(match mode {
			InputMode::Insert => match code {
				KeyCode::Backspace => Event::InputBackspace,
				KeyCode::Delete => Event::InputDeleteUnder,
				KeyCode::Left => Event::InputMoveLeft,
				KeyCode::Right => Event::InputMoveRight,
				KeyCode::Home => Event::InputMoveBol,
				KeyCode::End => Event::InputMoveEol,
				KeyCode::Char(c) => Event::InputChar(c),
				_ => return None,
			},

			// Replace: literally any character consumes the mode.
			InputMode::Replace => match code {
				KeyCode::Char(c) => Event::InputReplaceChar(c),
				_ => return None,
			},

			InputMode::Normal => match code {
				KeyCode::Char('i') => Event::InputEnterInsert,
				KeyCode::Char('I') => Event::InputEnterInsertBol,
				KeyCode::Char('a') => Event::InputEnterAppend,
				KeyCode::Char('A') => Event::InputEnterAppendEol,
				KeyCode::Char('r') => Event::InputEnterReplace,
				KeyCode::Char('v') => Event::InputToggleVisual,
				KeyCode::Char('D') => Event::InputDeleteToEol,
				KeyCode::Char('d') => Event::InputOpDelete,
				KeyCode::Char('h') | KeyCode::Left => Event::InputMoveLeft,
				KeyCode::Char('l') | KeyCode::Right => Event::InputMoveRight,
				KeyCode::Char('w') => Event::InputMoveWordForward,
				KeyCode::Char('b') => Event::InputMoveWordBack,
				KeyCode::Char('e') => Event::InputMoveWordEnd,
				KeyCode::Char('0') | KeyCode::Home => Event::InputMoveBol,
				KeyCode::Char('$') | KeyCode::End => Event::InputMoveEol,
				KeyCode::Char('x') | KeyCode::Delete => Event::InputDeleteUnder,
				_ => return None,
			},

			// Visual: motions extend the selection; unlike Normal, `d`/`x`
			// both delete it, and there's no operator-pending or replace.
			InputMode::Visual => match code {
				KeyCode::Char('v') => Event::InputToggleVisual,
				KeyCode::Char('h') | KeyCode::Left => Event::InputMoveLeft,
				KeyCode::Char('l') | KeyCode::Right => Event::InputMoveRight,
				KeyCode::Char('w') => Event::InputMoveWordForward,
				KeyCode::Char('b') => Event::InputMoveWordBack,
				KeyCode::Char('e') => Event::InputMoveWordEnd,
				KeyCode::Char('0') | KeyCode::Home => Event::InputMoveBol,
				KeyCode::Char('$') | KeyCode::End => Event::InputMoveEol,
				KeyCode::Char('x') | KeyCode::Char('d') | KeyCode::Delete => Event::InputDeleteVisual,
				_ => return None,
			},
		})
	}
}
