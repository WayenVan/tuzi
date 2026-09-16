use crossterm::event::KeyCode;

use crate::event::Event;

/// Tree-navigation keymap — while a rename prompt is open, `App::serve()`'s
/// loop hands raw key events to edtui directly instead of calling this at
/// all. Press-vs-release filtering and key extraction happen once, at that
/// same call site, rather than here.
///
/// Stateful only for leader-key chords (`tt` for a new tab, yazi-style): a
/// leader key arms `pending` and consumes that keypress; the *next* key
/// either completes the chord or, if it doesn't match anything, is simply
/// dropped — same as vim does with an unrecognized `g`-prefixed command.
#[derive(Default)]
pub struct Router {
	pending: Option<char>,
}

impl Router {
	pub fn route(&mut self, code: KeyCode) -> Option<Event> {
		if let Some(leader) = self.pending.take() {
			return match (leader, code) {
				('t', KeyCode::Char('t')) => Some(Event::TabNew),
				_ => None,
			};
		}

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
			KeyCode::Char('w') => Event::TabClose,
			KeyCode::Char(']') => Event::TabNext,
			KeyCode::Char('[') => Event::TabPrev,
			KeyCode::Char('t') => {
				self.pending = Some('t');
				return None;
			}
			_ => return None,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn tt_opens_a_new_tab_but_t_alone_does_nothing() {
		let mut router = Router::default();
		assert!(router.route(KeyCode::Char('t')).is_none(), "the leader key alone is just armed, not acted on");
		assert!(matches!(router.route(KeyCode::Char('t')), Some(Event::TabNew)));
	}

	#[test]
	fn an_unrecognized_follow_up_drops_the_chord_without_acting_on_it() {
		let mut router = Router::default();
		router.route(KeyCode::Char('t'));
		assert!(router.route(KeyCode::Char('x')).is_none());

		// and the router isn't left "stuck" waiting — the next key is
		// interpreted fresh, not as another follow-up.
		assert!(matches!(router.route(KeyCode::Char('q')), Some(Event::Quit)));
	}

	#[test]
	fn brackets_switch_tabs_left_and_right() {
		let mut router = Router::default();
		assert!(matches!(router.route(KeyCode::Char('[')), Some(Event::TabPrev)));
		assert!(matches!(router.route(KeyCode::Char(']')), Some(Event::TabNext)));
		assert!(router.route(KeyCode::Tab).is_none());
		assert!(router.route(KeyCode::BackTab).is_none());
	}
}
