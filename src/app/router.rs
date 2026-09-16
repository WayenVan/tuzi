use crossterm::event::KeyCode;

use crate::event::Event;

pub struct Router;

impl Router {
	/// Tree-navigation keymap only — while a rename prompt is open,
	/// `App::serve()`'s loop hands raw key events to edtui directly instead
	/// of calling this at all. Press-vs-release filtering and key extraction
	/// happen once, at that same call site, rather than here.
	pub fn route(code: KeyCode) -> Option<Event> {
		Some(match code {
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
}
