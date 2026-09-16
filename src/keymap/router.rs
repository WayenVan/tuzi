use crate::action::Action;

use super::{Key, KeyContext, Keymap};

#[derive(Debug, Eq, PartialEq)]
pub enum Route {
	Pending,
	Actions(Vec<Action>),
	Unmatched,
}

#[derive(Default)]
pub struct Router {
	keymap:  Keymap,
	pending: Vec<Key>,
}

impl Router {
	pub fn hint(&self) -> &str { self.keymap.hint() }

	pub fn route(&mut self, context: KeyContext, key: Key) -> Route {
		self.pending.push(key);
		let mut matched = self
			.keymap
			.bindings(context)
			.filter(|binding| binding.keys.starts_with(&self.pending));

		let Some(first) = matched.next() else {
			self.pending.clear();
			return Route::Unmatched;
		};

		if first.keys.len() == self.pending.len() {
			let actions = first.actions.clone();
			self.pending.clear();
			Route::Actions(actions)
		} else {
			Route::Pending
		}
	}
}

#[cfg(test)]
mod tests {
	use crossterm::event::{KeyCode, KeyModifiers};

	use crate::action::{Action, CursorTarget, InputKind};

	use super::*;

	#[test]
	fn matches_single_keys_and_arbitrary_chords() {
		let mut router = Router::default();
		assert_eq!(router.route(KeyContext::Manager, Key::char('j')), Route::Actions(vec![Action::MoveCursor(1)]));
		assert_eq!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending);
		assert_eq!(
			router.route(KeyContext::Manager, Key::char(' ')),
			Route::Actions(vec![Action::OpenInput(InputKind::Cd)])
		);
	}

	#[test]
	fn mismatch_clears_the_pending_sequence() {
		let mut router = Router::default();
		assert_eq!(router.route(KeyContext::Manager, Key::char('t')), Route::Pending);
		assert_eq!(router.route(KeyContext::Manager, Key::char('x')), Route::Unmatched);
		assert_eq!(router.route(KeyContext::Manager, Key::char('q')), Route::Actions(vec![Action::Quit]));
	}

	#[test]
	fn normalizes_shift_for_printable_characters() {
		let key = crossterm::event::KeyEvent::new(KeyCode::Char('V'), crossterm::event::KeyModifiers::SHIFT);
		assert_eq!(Key::from(key), Key::char('V'));
	}

	#[test]
	fn maps_control_page_keys_without_claiming_plain_u_or_d() {
		let mut router = Router::default();
		let control = |c| Key::new(KeyCode::Char(c), KeyModifiers::CONTROL);

		assert_eq!(router.route(KeyContext::Manager, control('u')), Route::Actions(vec![Action::MovePage(-50)]));
		assert_eq!(router.route(KeyContext::Manager, control('d')), Route::Actions(vec![Action::MovePage(50)]));
		assert_eq!(router.route(KeyContext::Manager, control('b')), Route::Actions(vec![Action::MovePage(-100)]));
		assert_eq!(router.route(KeyContext::Manager, control('f')), Route::Actions(vec![Action::MovePage(100)]));
		assert_eq!(router.route(KeyContext::Manager, Key::char('u')), Route::Unmatched);
		assert_eq!(router.route(KeyContext::Manager, Key::char('d')), Route::Actions(vec![Action::Delete]));
	}

	#[test]
	fn semicolon_toggles_selection_and_plain_space_is_unbound() {
		let mut router = Router::default();
		assert_eq!(router.route(KeyContext::Manager, Key::char(';')), Route::Actions(vec![Action::ToggleSelect]));
		assert_eq!(router.route(KeyContext::Manager, Key::char(' ')), Route::Unmatched);
	}

	#[test]
	fn shares_the_g_prefix_between_top_and_directory_navigation() {
		let mut router = Router::default();
		assert_eq!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending);
		assert_eq!(router.route(KeyContext::Manager, Key::char('g')), Route::Actions(vec![Action::MoveTo(CursorTarget::Top)]));

		assert_eq!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending);
		assert_eq!(router.route(KeyContext::Manager, Key::char(' ')), Route::Actions(vec![Action::OpenInput(InputKind::Cd)]));
		assert_eq!(router.route(KeyContext::Manager, Key::char('G')), Route::Actions(vec![Action::MoveTo(CursorTarget::Bottom)]));
	}
}
