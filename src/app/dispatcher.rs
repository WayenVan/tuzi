use crate::event::Event;

use super::App;

pub struct Dispatcher;

impl Dispatcher {
	pub fn dispatch(app: &mut App, event: Event) {
		match event {
			Event::Quit => app.quit = true,
			Event::TabNew => app.new_tab(),
			Event::TabClose => app.close_tab(),
			Event::TabNext => app.next_tab(),
			Event::TabPrev => app.prev_tab(),

			// Keyboard-driven actions always act on whichever tab is
			// currently on screen.
			Event::MoveDown => app.active_tab_mut().move_cursor(1),
			Event::MoveUp => app.active_tab_mut().move_cursor(-1),
			Event::Expand => app.active_tab_mut().expand_selected(),
			Event::Collapse => app.active_tab_mut().collapse_selected(),
			Event::ToggleSelect => app.active_tab_mut().toggle_selected(),
			Event::VisualSelect => app.active_tab_mut().enter_visual(false),
			Event::VisualUnset => app.active_tab_mut().enter_visual(true),
			Event::Delete => app.active_tab_mut().delete_selected(),
			Event::Yank => app.active_tab_mut().yank_selected(),
			Event::Paste => app.active_tab_mut().paste(),
			Event::Escape => app.active_tab_mut().escape(),
			Event::Rename => app.active_tab_mut().start_rename(),
			Event::CdInteractive => app.active_tab_mut().start_cd(),
			Event::InputKey(key) => app.active_tab_mut().handle_input_key(key),

			// Background events carry the id of the tab that requested
			// them, which may not be the active one — and may not even
			// exist anymore, if that tab was closed in the meantime.
			Event::Changed { tab, path } => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_changed(path);
				}
			}
			Event::Loaded { tab, path, ticket, result } => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_loaded(path, ticket, result);
				}
			}
			Event::Deleted { tab, paths } => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_deleted(paths);
				}
			}
			Event::Pasted { tab, target } => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_pasted(target);
				}
			}
			Event::CompletionLoaded { tab, input, revision, result } => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_completion_loaded(input, revision, result);
				}
			}

			// Translated into a logical event by App::serve()'s loop before
			// it ever reaches here; kept as a no-op so the match stays
			// exhaustive if that ever changes.
			Event::Term(_) => {}
		}
	}
}
