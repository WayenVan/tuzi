use crate::{
	command::{CdTarget, Command, CursorTarget, DeleteMode},
	dds::Body,
	event::Event,
	fs::FsChange,
};

use super::App;

pub struct Dispatcher;

impl App {
	pub fn execute(&mut self, command: Command) {
		match command {
			Command::Quit => self.request_quit(),
			Command::Escape => self.active_tab_mut().escape(),
			Command::Cursor(CursorTarget::Relative(delta)) => self.active_tab_mut().move_cursor(delta),
			Command::MovePage(percent) => self.move_page(percent),
			Command::Cursor(CursorTarget::Top) => self.active_tab_mut().move_to_top(),
			Command::Cursor(CursorTarget::Bottom) => self.active_tab_mut().move_to_bottom(),
			Command::Cd(CdTarget::Home) => {
				let home = self.home.clone();
				if let Err(error) = self.active_tab_mut().cd(home) {
					self.active_tab_mut().raise(crate::notice::NoticeLevel::Error, error.to_string());
				}
			}
			Command::Cd(CdTarget::Interactive) => self.active_tab_mut().start_cd(),
			Command::Cd(CdTarget::Selected) => self.active_tab_mut().cd_selected(),
			Command::Cd(CdTarget::Trash) => self.active_tab_mut().cd_trash(),
			Command::Cd(CdTarget::Config) => self.active_tab_mut().cd_config(),
			Command::Cd(CdTarget::Path(path)) => if let Err(error) = self.active_tab_mut().cd_path(&path) { self.active_tab_mut().raise(crate::notice::NoticeLevel::Error, error.to_string()); },
			Command::HistoryBack => self.active_tab_mut().history_back(),
			Command::HistoryForward => self.active_tab_mut().history_forward(),
			Command::Expand => self.active_tab_mut().expand_selected(),
			Command::ToggleExpand => self.active_tab_mut().toggle_expand_selected(),
			Command::Collapse => self.active_tab_mut().collapse_selected(),
			Command::CollapseSubtree => self.active_tab_mut().collapse_subtree(),
			Command::CollapseSiblings => self.active_tab_mut().collapse_siblings(),
			Command::CollapseAll => self.active_tab_mut().collapse_all(),
			Command::CenterCursor => self.center_cursor(),
			Command::ToggleSelect => self.active_tab_mut().toggle_selected(),
			Command::VisualSelect { unset } => self.active_tab_mut().enter_visual(unset),
			Command::Delete => self.request_delete(DeleteMode::Trash),
			Command::DeletePermanently => self.request_delete(DeleteMode::Permanent),
			Command::Yank { cut } => self.yank_selected(cut),
			Command::Paste => self.paste(),
			Command::PasteLink { absolute } => self.paste_link(absolute),
			Command::Copy(kind) => self.copy_to_system_clipboard(kind),
			Command::Rename(None) => self.active_tab_mut().start_rename(),
			Command::Rename(Some(name)) => self.active_tab_mut().rename_selected(name),
			Command::Create(None) => self.active_tab_mut().start_create(),
			Command::Create(Some(path)) => self.active_tab_mut().create_path(path),
			Command::Find { previous } => self.active_tab_mut().start_find(previous),
			Command::Filter => self.active_tab_mut().start_filter(),
			Command::CommandPrompt => self.active_tab_mut().start_command(),
			Command::NewTab => self.new_tab(),
			Command::CloseTab => self.close_tab(),
			Command::SwitchTab(delta) => self.switch_tab(delta),
			Command::SwitchTabTo(id) => self.switch_tab_to(id),
			Command::SetColumnMode(mode) => self.active_tab_mut().column_mode = mode,
			Command::SetSort(policy) => self.active_tab_mut().set_sort(policy),
			Command::ToggleHidden => self.active_tab_mut().toggle_hidden(),
			Command::TogglePreview => self.active_tab_mut().preview.toggle(),
			Command::SeekPreview(units) => self.active_tab_mut().preview.seek(units),
			Command::RepeatFind { opposite } => self.active_tab_mut().repeat_find(opposite),
			Command::Fzf => self.start_fzf(),
			Command::Zoxide => self.start_zoxide(),
			Command::Open { interactive } => self.open_selected(interactive),
			Command::ToggleTasks => self.tasks.visible = !self.tasks.visible,
			Command::EntryDetails => {
				self.entry_details = true;
				self.entry_details_scroll = 0;
			}
			Command::ToggleFilenamePeek => self.filename_peek = !self.filename_peek,
			Command::UpdateTab { path, selection } => self.update_tab(path, selection),
			Command::Reveal(path) => self.reveal_path(path),
			Command::SetHome(path) => self.set_home(path),
			Command::RestoreState(state) => self.restore_state(state),
			Command::GetState { query_id } => self.reply_state(query_id),
			Command::GetTabs { query_id } => self.reply_tabs(query_id),
			Command::Emit { kind, data, parent } => self.emit(Body::Custom { kind, data }, parent),
		}
		self.drain_tab_notices();
	}
}

impl Dispatcher {
	pub fn dispatch_event(app: &mut App, event: Event) {
		match event {
			Event::Redraw => {}
			Event::Changed { tab, path } => {
				if let Some(t) = app.staged_tab_mut(tab) {
					t.on_changed(path);
				} else if let Some(t) = app.tab_mut(tab) {
					t.on_changed(path);
				}
			}
			Event::FilesChanged { tab, parent, changes } => {
				for change in &changes {
					if let FsChange::Delete { path } = change {
						app.forget_clipboard_path(path);
					}
				}
				if let Some(t) = app.staged_tab_mut(tab) {
					t.on_files_changed(parent, changes);
				} else if let Some(t) = app.tab_mut(tab) {
					t.on_files_changed(parent, changes);
				}
			}
			Event::Loaded {
				tab,
				path,
				ticket,
				result,
				done,
			} => {
				if app.is_staged_tab(tab) {
					app.on_staged_loaded(tab, path, ticket, result, done);
				} else if let Some(t) = app.tab_mut(tab) {
					t.on_loaded(path, ticket, result, done);
				}
			}
			Event::Created {
				tab,
				base,
				value,
				target,
				result,
			} => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_created(base, value, target, result);
				}
			}
			Event::Linked { tab, target, result } => {
				if let Some(t) = app.tab_mut(tab) {
					t.on_linked(target, result);
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
			Event::Visited(path) => app.record_visit(path),
			Event::Task(event) => app.on_task_event(event),
			Event::DdsPublish(body) => app.publish(body),
			Event::DdsDeliver(body) => {
				if let Body::Sync { peers } = &body {
					app.update_controller_peers(peers);
				}
				for command in app.pubsub.deliver(&body) {
					app.execute(command);
				}
			}
			Event::DdsRejected(error) => app.active_tab_mut().raise(crate::notice::NoticeLevel::Warn, error),
			Event::Term(_) => {}
		}
		app.drain_tab_notices();
	}
}
