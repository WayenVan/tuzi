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
			Event::RenameKey(key) => app.handle_rename_key(key),

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
