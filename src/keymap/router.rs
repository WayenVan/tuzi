use crate::command::Command;

use super::{Key, KeyContext, Keymap};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhichCandidate {
	pub keys: Vec<Key>,
	pub description: String,
}

#[derive(Debug, PartialEq)]
pub enum Route {
	Pending(Vec<WhichCandidate>),
	Commands(Vec<Command>),
	Unmatched,
}

#[derive(Default)]
pub struct Router {
	keymap: Keymap,
	pending: Vec<Key>,
}

impl Router {
	pub fn new(keymap: Keymap) -> Self { Self { keymap, pending: Vec::new() } }

	pub fn route(&mut self, context: KeyContext, key: Key) -> Route {
		self.pending.push(key);
		let matched: Vec<_> = self
			.keymap
			.bindings(context)
			.filter(|binding| binding.keys.starts_with(&self.pending))
			.collect();

		if matched.is_empty() {
			self.pending.clear();
			return Route::Unmatched;
		}

		if let Some(binding) = matched.iter().find(|binding| binding.keys.len() == self.pending.len()) {
			let commands = binding.commands.clone();
			self.pending.clear();
			Route::Commands(commands)
		} else {
			let typed = self.pending.len();
			Route::Pending(
				matched
					.into_iter()
					.map(|binding| WhichCandidate {
						keys: binding.keys[typed..].to_vec(),
						description: binding.description.clone(),
					})
					.collect(),
			)
		}
	}
}

#[cfg(test)]
mod tests {
	use crossterm::event::{KeyCode, KeyModifiers};

	use crate::{
		command::{CdTarget, Command, CopyKind, CursorTarget},
		column_mode::ColumnMode,
		fs::{SortBy, SortPolicy},
	};

	use super::*;

	#[test]
	fn matches_single_keys_and_arbitrary_chords() {
		let mut router = Router::default();
		assert_eq!(router.route(KeyContext::Manager, Key::char('j')), Route::Commands(vec![Command::Cursor(CursorTarget::Relative(1))]));
		assert!(matches!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending(_)));
		assert_eq!(
			router.route(KeyContext::Manager, Key::char(' ')),
			Route::Commands(vec![Command::Cd(CdTarget::Interactive)])
		);
	}

	#[test]
	fn colon_opens_the_command_input() {
		let mut router = Router::default();
		assert_eq!(router.route(KeyContext::Manager, Key::char(':')), Route::Commands(vec![Command::CommandPrompt]));
	}

	#[test]
	fn uppercase_k_opens_entry_details_without_replacing_lowercase_movement() {
		let mut router = Router::default();
		assert_eq!(router.route(KeyContext::Manager, Key::char('K')), Route::Commands(vec![Command::EntryDetails]));
		assert_eq!(router.route(KeyContext::Manager, Key::char('k')), Route::Commands(vec![Command::Cursor(CursorTarget::Relative(-1))]));
		assert_eq!(router.route(KeyContext::Manager, Key::char('I')), Route::Commands(vec![Command::ToggleFilenamePeek]));
	}

	#[test]
	fn mismatch_clears_the_pending_sequence() {
		let mut router = Router::default();
		assert!(matches!(router.route(KeyContext::Manager, Key::char('t')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char('x')), Route::Unmatched);
		assert_eq!(router.route(KeyContext::Manager, Key::char('q')), Route::Commands(vec![Command::Quit]));
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

		assert_eq!(router.route(KeyContext::Manager, control('u')), Route::Commands(vec![Command::MovePage(-50)]));
		assert_eq!(router.route(KeyContext::Manager, control('d')), Route::Commands(vec![Command::MovePage(50)]));
		assert_eq!(router.route(KeyContext::Manager, control('b')), Route::Commands(vec![Command::MovePage(-100)]));
		assert_eq!(router.route(KeyContext::Manager, control('f')), Route::Commands(vec![Command::MovePage(100)]));
		assert_eq!(router.route(KeyContext::Manager, Key::char('u')), Route::Unmatched);
		assert_eq!(router.route(KeyContext::Manager, Key::char('d')), Route::Commands(vec![Command::Delete]));
	}

	#[test]
	fn control_o_and_control_i_navigate_directory_history() {
		let mut router = Router::default();
		let control = |c| Key::new(KeyCode::Char(c), KeyModifiers::CONTROL);
		assert_eq!(router.route(KeyContext::Manager, control('o')), Route::Commands(vec![Command::HistoryBack]));
		assert_eq!(router.route(KeyContext::Manager, control('i')), Route::Commands(vec![Command::HistoryForward]));
		assert_eq!(router.route(KeyContext::Manager, Key::plain(KeyCode::Tab)), Route::Commands(vec![Command::HistoryForward]));
	}

	#[test]
	fn space_prefix_exposes_fzf_and_symlink_paste() {
		let mut router = Router::default();
		assert_eq!(router.route(KeyContext::Manager, Key::char(';')), Route::Commands(vec![Command::ToggleSelect]));
		assert!(matches!(router.route(KeyContext::Manager, Key::char(' ')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char(' ')), Route::Commands(vec![Command::Fzf]));

		assert!(matches!(router.route(KeyContext::Manager, Key::char(' ')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char('-')), Route::Commands(vec![Command::PasteLink { absolute: false }]));

		assert!(matches!(router.route(KeyContext::Manager, Key::char(' ')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char('_')), Route::Commands(vec![Command::PasteLink { absolute: true }]));
		assert_eq!(router.route(KeyContext::Manager, Key::char('P')), Route::Unmatched);
	}

	#[test]
	fn z_prefix_controls_tree_folding_and_cursor_centering() {
		let mut router = Router::default();
		for (key, command) in [
			('c', Command::CollapseSubtree),
			('m', Command::CollapseSiblings),
			('M', Command::CollapseAll),
			('z', Command::CenterCursor),
		] {
			assert!(matches!(router.route(KeyContext::Manager, Key::char('z')), Route::Pending(_)));
			assert_eq!(router.route(KeyContext::Manager, Key::char(key)), Route::Commands(vec![command]));
		}
	}

	#[test]
	fn shares_the_g_prefix_between_top_and_directory_navigation() {
		let mut router = Router::default();
		assert!(matches!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending(_)));
		assert_eq!(
			router.route(KeyContext::Manager, Key::char('g')),
			Route::Commands(vec![Command::Cursor(CursorTarget::Top)])
		);

		assert!(matches!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending(_)));
		assert_eq!(
			router.route(KeyContext::Manager, Key::char(' ')),
			Route::Commands(vec![Command::Cd(CdTarget::Interactive)])
		);
		assert!(matches!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char('h')), Route::Commands(vec![Command::Cd(CdTarget::Path("..".into()))]));
		assert!(matches!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char('=')), Route::Commands(vec![Command::Cd(CdTarget::Home)]));
		assert!(matches!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char('l')), Route::Commands(vec![Command::Cd(CdTarget::Selected)]));
		assert_eq!(
			router.route(KeyContext::Manager, Key::char('G')),
			Route::Commands(vec![Command::Cursor(CursorTarget::Bottom)])
		);
	}

	#[test]
	fn m_prefix_selects_the_column_mode() {
		let mut router = Router::default();
		for (key, mode) in [
			('n', ColumnMode::None),
			('s', ColumnMode::Size),
			('p', ColumnMode::Permissions),
			('m', ColumnMode::Modified),
		] {
			assert!(matches!(router.route(KeyContext::Manager, Key::char('m')), Route::Pending(_)));
			assert_eq!(
				router.route(KeyContext::Manager, Key::char(key)),
				Route::Commands(vec![Command::SetColumnMode(mode)])
			);
		}
	}

	#[test]
	fn hidden_and_directory_shortcuts_match_yazi() {
		let mut router = Router::default();
		assert_eq!(router.route(KeyContext::Manager, Key::char('.')), Route::Commands(vec![Command::ToggleHidden]));

		assert!(matches!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char('~')), Route::Commands(vec![Command::Cd(CdTarget::Path("~".into()))]));

		assert!(matches!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char('c')), Route::Commands(vec![Command::Cd(CdTarget::Config)]));

		assert!(matches!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char('d')), Route::Commands(vec![Command::Cd(CdTarget::Path("~/Downloads".into()))]));

		assert!(matches!(router.route(KeyContext::Manager, Key::char('g')), Route::Pending(_)));
		assert_eq!(router.route(KeyContext::Manager, Key::char('D')), Route::Commands(vec![Command::Cd(CdTarget::Path("~/Desktop".into()))]));
	}

	#[test]
	fn comma_prefix_selects_yazi_style_sorting() {
		let mut router = Router::default();
		for (key, by, reverse) in [
			('a', SortBy::Name, false),
			('A', SortBy::Name, true),
			('m', SortBy::Modified, false),
			('M', SortBy::Modified, true),
			('s', SortBy::Size, false),
			('S', SortBy::Size, true),
			('e', SortBy::Extension, false),
			('E', SortBy::Extension, true),
		] {
			assert!(matches!(router.route(KeyContext::Manager, Key::char(',')), Route::Pending(_)));
			assert_eq!(
				router.route(KeyContext::Manager, Key::char(key)),
				Route::Commands(vec![Command::SetSort(SortPolicy::new(by, reverse))])
			);
		}
	}

	#[test]
	fn c_prefix_exposes_yazi_style_clipboard_copies() {
		let mut router = Router::default();
		for (key, kind) in [
			('c', CopyKind::Path),
			('C', CopyKind::Url),
			('d', CopyKind::DirectoryPath),
			('D', CopyKind::DirectoryUrl),
			('f', CopyKind::Filename),
			('n', CopyKind::Stem),
		] {
			assert!(matches!(router.route(KeyContext::Manager, Key::char('c')), Route::Pending(_)));
			assert_eq!(router.route(KeyContext::Manager, Key::char(key)), Route::Commands(vec![Command::Copy(kind)]));
		}
	}

	#[test]
	fn pending_route_exposes_remaining_keys_and_descriptions() {
		let mut router = Router::default();
		let Route::Pending(candidates) = router.route(KeyContext::Manager, Key::char('m')) else {
			panic!("m should open the column-mode prefix");
		};

		assert_eq!(candidates.len(), 4);
		assert!(
			candidates
				.iter()
				.any(|candidate| candidate.keys == [Key::char('s')] && candidate.description == "Show size column")
		);
		assert!(
			candidates
				.iter()
				.any(|candidate| candidate.keys == [Key::char('n')] && candidate.description == "Hide column")
		);
	}

	#[test]
	fn control_p_toggles_the_preview() {
		let mut router = Router::default();
		let key = Key::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
		assert_eq!(router.route(KeyContext::Manager, key), Route::Commands(vec![Command::TogglePreview]));
	}

	#[test]
	fn alt_j_and_k_scroll_the_preview() {
		let mut router = Router::default();
		let alt = |c| Key::new(KeyCode::Char(c), KeyModifiers::ALT);
		assert_eq!(router.route(KeyContext::Manager, alt('j')), Route::Commands(vec![Command::SeekPreview(1)]));
		assert_eq!(router.route(KeyContext::Manager, alt('k')), Route::Commands(vec![Command::SeekPreview(-1)]));
	}

	#[test]
	fn slash_and_question_open_find_and_n_repeats_it() {
		let mut router = Router::default();
		assert_eq!(
			router.route(KeyContext::Manager, Key::char('/')),
			Route::Commands(vec![Command::Find { previous: false }])
		);
		assert_eq!(
			router.route(KeyContext::Manager, Key::char('?')),
			Route::Commands(vec![Command::Find { previous: true }])
		);
		assert_eq!(
			router.route(KeyContext::Manager, Key::char('n')),
			Route::Commands(vec![Command::RepeatFind { opposite: false }])
		);
		assert_eq!(
			router.route(KeyContext::Manager, Key::char('N')),
			Route::Commands(vec![Command::RepeatFind { opposite: true }])
		);
	}

	#[test]
	fn o_opens_and_uppercase_o_chooses_an_opener() {
		let mut router = Router::default();
		assert_eq!(
			router.route(KeyContext::Manager, Key::char('o')),
			Route::Commands(vec![Command::Open { interactive: false }])
		);
		assert_eq!(
			router.route(KeyContext::Manager, Key::char('O')),
			Route::Commands(vec![Command::Open { interactive: true }])
		);
	}
}
