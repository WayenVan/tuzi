use crate::{action::{Action, CursorTarget, DeleteMode, InputKind}, event::Event};

use super::App;

pub struct Dispatcher;

impl Dispatcher {
	pub fn dispatch(app: &mut App, action: Action) {
		match action {
			Action::Quit => app.request_quit(),
			Action::Escape => app.active_tab_mut().escape(),
			Action::MoveCursor(delta) => app.active_tab_mut().move_cursor(delta),
			Action::MovePage(percent) => app.move_page(percent),
			Action::MoveTo(CursorTarget::Top) => app.active_tab_mut().move_to_top(),
			Action::MoveTo(CursorTarget::Bottom) => app.active_tab_mut().move_to_bottom(),
			Action::CdParent => app.active_tab_mut().cd_parent(),
			Action::CdSelected => app.active_tab_mut().cd_selected(),
			Action::CdTrash => app.active_tab_mut().cd_trash(),
			Action::Expand => app.active_tab_mut().expand_selected(),
			Action::ToggleExpand => app.active_tab_mut().toggle_expand_selected(),
			Action::Collapse => app.active_tab_mut().collapse_selected(),
			Action::ToggleSelect => app.active_tab_mut().toggle_selected(),
			Action::VisualSelect { unset } => app.active_tab_mut().enter_visual(unset),
			Action::Delete => app.active_tab_mut().delete_selected(DeleteMode::Trash),
			Action::DeletePermanently => app.active_tab_mut().delete_selected(DeleteMode::Permanent),
			Action::Yank { cut } => app.yank_selected(cut),
			Action::Paste => app.paste(),
			Action::OpenInput(InputKind::Rename) => app.active_tab_mut().start_rename(),
			Action::OpenInput(InputKind::Cd) => app.active_tab_mut().start_cd(),
			Action::OpenInput(InputKind::Create) => app.active_tab_mut().start_create(),
			Action::OpenInput(InputKind::Find { previous }) => app.active_tab_mut().start_find(previous),
			Action::OpenInput(InputKind::Filter) => app.active_tab_mut().start_filter(),
			Action::NewTab => app.new_tab(),
			Action::CloseTab => app.close_tab(),
			Action::SwitchTab(delta) => app.switch_tab(delta),
			Action::SetColumnMode(mode) => app.active_tab_mut().column_mode = mode,
			Action::TogglePreview => app.active_tab_mut().preview.toggle(),
			Action::SeekPreview(units) => app.active_tab_mut().preview.seek(units),
			Action::RepeatFind { opposite } => app.active_tab_mut().repeat_find(opposite),
			Action::Fzf => app.start_fzf(),
			Action::Open { interactive } => app.open_selected(interactive),
			Action::ToggleTasks => app.tasks.visible = !app.tasks.visible,
		}
		app.drain_tab_notices();
	}

	pub fn dispatch_event(app: &mut App, event: Event) {
		match event {
			Event::Redraw => {},
			Event::Changed { tab, path } => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_changed(path);
				}
			}
			Event::Loaded { tab, path, ticket, result, done } => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_loaded(path, ticket, result, done);
				}
			}
			Event::Created { tab, base, value, target, result } => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_created(base, value, target, result);
				}
			}
			Event::CompletionLoaded { tab, input, revision, result } => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_completion_loaded(input, revision, result);
				}
			}
			Event::PreviewLoaded { tab, ticket, key, result } => {
				if let Some(t) = app.tab_mut(tab) {
					t.preview.accept(ticket, key, result);
				}
			}
			Event::OpenResolved { tab, cwd, interactive, result } => app.on_open_resolved(tab, cwd, interactive, result),
			Event::Task(event) => app.on_task_event(event),
			Event::Term(_) => {}
		}
		app.drain_tab_notices();
	}
}
