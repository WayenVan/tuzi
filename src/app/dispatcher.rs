use crate::event::Event;

use super::App;

pub struct Dispatcher;

impl Dispatcher {
	pub fn dispatch(app: &mut App, event: Event) {
		match event {
			Event::Quit => app.quit = true,
			Event::MoveDown => app.move_cursor(1),
			Event::MoveUp => app.move_cursor(-1),
			Event::Expand => app.expand_selected(),
			Event::Collapse => app.collapse_selected(),
			Event::ToggleSelect => app.toggle_selected(),
			Event::VisualSelect => app.enter_visual(false),
			Event::VisualUnset => app.enter_visual(true),
			Event::Delete => app.delete_selected(),
			Event::Yank => app.yank_selected(),
			Event::Paste => app.paste(),
			Event::Escape => app.escape(),
			Event::Rename => app.start_rename(),
			Event::InputChar(c) => app.input_insert(c),
			Event::InputReplaceChar(c) => app.input_replace_char(c),
			Event::InputBackspace => app.input_backspace(),
			Event::InputDeleteUnder => app.input_delete_under(),
			Event::InputDeleteToEol => app.input_delete_to_eol(),
			Event::InputDeleteVisual => app.input_delete_visual(),
			Event::InputOpDelete => app.input_op_delete(),
			Event::InputMoveLeft => app.input_move_left(),
			Event::InputMoveRight => app.input_move_right(),
			Event::InputMoveBol => app.input_move_bol(),
			Event::InputMoveEol => app.input_move_eol(),
			Event::InputMoveWordForward => app.input_move_word_forward(),
			Event::InputMoveWordBack => app.input_move_word_back(),
			Event::InputMoveWordEnd => app.input_move_word_end(),
			Event::InputEnterInsert => app.input_enter_insert(),
			Event::InputEnterInsertBol => app.input_enter_insert_bol(),
			Event::InputEnterAppend => app.input_enter_append(),
			Event::InputEnterAppendEol => app.input_enter_append_eol(),
			Event::InputEnterReplace => app.input_enter_replace(),
			Event::InputToggleVisual => app.input_toggle_visual(),
			Event::InputEscape => app.input_escape(),
			Event::InputConfirm => app.confirm_rename(),

			Event::Changed(path) => app.on_changed(path),
			Event::Loaded { path, ticket, result } => app.on_loaded(path, ticket, result),
			Event::Deleted(paths) => app.on_deleted(paths),
			Event::Pasted(target) => app.on_pasted(target),

			// Translated into a logical event by App::serve()'s loop before
			// it ever reaches here; kept as a no-op so the match stays
			// exhaustive if that ever changes.
			Event::Term(_) => {}
		}
	}
}
