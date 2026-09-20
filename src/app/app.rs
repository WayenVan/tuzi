use std::{collections::{HashSet, VecDeque}, io, path::{Path, PathBuf}, sync::Arc};

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{
	command::{CopyKind, DeleteMode},
	config::{Config, PreviewLayout},
	dds::{self, Body},
	event::Event,
	icon::IconTheme,
	keymap::{Key, KeyContext, Keymap, Route, Router, WhichCandidate},
	notice::{Notice, NoticeLevel},
	opener::OpenPicker,
	process::ProcessRequest,
	scheduler::OpenScheduler,
	tasks::{TaskEvent, TaskKind, TaskManager},
	theme::Theme,
	tui::TerminalSession,
};

use super::{
	restore::{SessionRestoreStep, StagedSession},
	Dispatcher, Tab,
};

pub struct App {
	/// Fixed session home shared by all tabs.
	pub(super) home: PathBuf,
	pub(super) config: Arc<Config>,
	pub tabs: Vec<Tab>,
	pub active: usize,
	pub quit: bool,
	/// Armed by `request_quit` when a task is still running — quitting
	/// straight away would abandon whatever `.tuzi-part-*` temp file a
	/// copy was mid-write on, so this asks first instead of just doing it.
	pub(super) pending_quit: bool,
	next_tab_id: usize,
	/// A replacement session being built off-screen. Its tab IDs are drawn
	/// from the same monotonic sequence as visible tabs, but it is not
	/// rendered or reachable by user commands until the atomic commit.
	pub(super) staged_session: Option<StagedSession>,
	/// Set when the most recent asynchronous restore failed. Startup reads
	/// this to turn the same executor failure into a process error; runtime
	/// restores additionally surface it as a warning toast.
	pub(super) restore_error: Option<String>,
	/// The yanked files, shared by every tab: yank in one, paste in another.
	pub(super) clipboard: Vec<PathBuf>,
	pub(super) clipboard_cut: bool,
	pub(super) tree_rows: usize,
	pub(super) terminal_focused: bool,
	pub(super) mouse: MouseState,
	pub(super) which: Vec<WhichCandidate>,
	pub(super) icon_theme: IconTheme,
	pub(super) theme: Theme,
	pub(super) open: OpenScheduler,
	pub(super) open_picker: Option<OpenPicker>,
	pub(super) entry_details: bool,
	pub(super) entry_details_scroll: u16,
	pub(super) filename_peek: bool,
	pub(super) processes: VecDeque<ProcessRequest>,
	pub(super) tx: mpsc::UnboundedSender<Event>,
	pub(super) pubsub: dds::Registry,
	/// The TUI's long-lived DDS peer. Tests that exercise only local app
	/// behavior leave this as `None` and keep using the in-process bus.
	pub(super) dds_client: Option<dds::Client>,
	pub(super) controller: Option<ControllerLink>,
	pub tasks: TaskManager,
	/// One-off toasts (invalid cd, refused delete, a failed external
	/// process, …) — global, not tied to whichever tab is active, and
	/// timeout-driven rather than something the user dismisses.
	pub(super) notices: Vec<Notice>,
}

pub(super) struct ControllerLink {
	pub launch: dds::DdsLaunch,
	pub online: bool,
	pub abilities: HashSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncomingTabUpdate {
	#[serde(default)]
	path:      Option<PathBuf>,
	#[serde(default)]
	selection: Vec<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncomingReveal {
	path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncomingSetHome {
	path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IncomingSwitchTab {
	tab_id: usize,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MouseState {
	pub tabs:            Rect,
	pub body:            Rect,
	pub tree:            Rect,
	pub preview:         Option<Rect>,
	pub tree_row_offset: usize,
	pub preview_percent: u16,
	pub preview_layout:  PreviewLayout,
	pub resizing:        bool,
}

impl Default for MouseState {
	fn default() -> Self {
		Self {
			tabs: Rect::default(), body: Rect::default(), tree: Rect::default(), preview: None,
			tree_row_offset: 0, preview_percent: 40, preview_layout: PreviewLayout::Horizontal, resizing: false,
		}
	}
}

impl App {
	/// Picks the session home: an explicit `--home` wins over the startup
	/// snapshot's `home`, which wins over the start PATH. The snapshot's home
	/// is always removed so the restore that follows cannot override an
	/// explicit choice.
	fn resolve_startup_home(explicit: Option<PathBuf>, state: &mut Option<crate::session_state::SessionState>, path: &std::path::Path) -> io::Result<PathBuf> {
		let snapshot = state.as_mut().and_then(|state| state.home.take());
		let home = crate::fs::absolute_lexical(&explicit.or(snapshot).unwrap_or_else(|| path.to_path_buf()))?;
		if !std::fs::metadata(&home)?.is_dir() {
			return Err(io::Error::new(io::ErrorKind::InvalidInput, "home is not a directory"));
		}
		Ok(home)
	}

	pub async fn serve(path: PathBuf, home: Option<PathBuf>, config: Config, keymap: Keymap, theme: Theme, state: Option<crate::session_state::SessionState>, dds_launch: Option<dds::DdsLaunch>) -> io::Result<()> {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let config = Arc::new(config);
		let mut state = state.map(crate::session_state::validate_and_normalize).transpose()
			.map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid startup session state: {error}")))?;
		let home = Self::resolve_startup_home(home, &mut state, &path)?;
		let pubsub = Self::new_registry();
		if dds_launch.is_some() && !config.dds.enabled {
			return Err(io::Error::new(io::ErrorKind::InvalidInput, "controlled launch requires DDS, but dds.enabled is false"));
		}
		let (dds_client, dds_error) = match config.dds.enabled {
			true => match Self::connect_dds(tx.clone(), pubsub.abilities(), dds_launch.as_ref().map(|launch| launch.parent)).await {
				Ok(client) => (Some(client), None),
				Err(error) if dds_launch.is_some() => return Err(io::Error::new(error.kind(), format!("controlled launch could not connect to DDS: {error}"))),
				Err(error) => (None, Some(error)),
			},
			false => (None, None),
		};

		let initial_path = state.as_ref().map_or(path, |state| state.tabs[0].cwd.clone());
		let first = Tab::open_configured(0, initial_path, tx.clone(), config.clone())?;
		let controller = dds_launch.clone().map(|launch| ControllerLink { launch, online: true, abilities: HashSet::new() });
		let mut app = Self {
			home,
			config: config.clone(),
			tabs: vec![first],
			active: 0,
			quit: false,
			pending_quit: false,
			next_tab_id: 1,
			staged_session: None,
			restore_error: None,
			clipboard: Vec::new(),
			clipboard_cut: false,
			tree_rows: 0,
			terminal_focused: true,
			mouse: MouseState { preview_percent: config.preview.ratio, ..MouseState::default() },
			which: Vec::new(),
			icon_theme: IconTheme::new(theme.icon.clone()),
			theme,
			open: OpenScheduler::new(tx.clone()),
			open_picker: None,
			entry_details: false,
			entry_details_scroll: 0,
			filename_peek: config.ui.filename_peek,
			processes: VecDeque::new(),
			tasks: TaskManager::configured(tx.clone(), config.tasks.clone()),
			notices: Vec::new(),
			pubsub,
			dds_client,
			controller,
			tx,
		};
		if let Some(error) = dds_error {
			app.notices.push(Notice::new(
				NoticeLevel::Warn,
				format!("DDS unavailable; continuing with local events only: {error}"),
				std::time::Duration::from_secs(5),
			));
		}
		if let Some(state) = state {
			app.begin_normalized_restore(state)?;
			app.finish_startup_restore(&mut rx).await?;
		}
		let mut terminal = TerminalSession::start()?;
		let mut router = Router::new(keymap);
		if let Some(launch) = dds_launch {
			app.announce_attach(&launch);
		}

		app.render(terminal.terminal())?;
		loop {
			let event = tokio::select! {
				biased;
				term_event = terminal.next_event() => match term_event? {
					Some(event) => Event::Term(event),
					None => break,
				},
				background_event = rx.recv() => match background_event {
					Some(event) => event,
					None => break,
				},
			};
			// A burst of background events (e.g. a filesystem watcher storm
			// spanning several directories) drains here before the next
			// render, instead of redrawing once per event.
			let mut dirty = app.handle_event(event, &mut router);
			while !app.quit {
				let Ok(event) = rx.try_recv() else { break };
				dirty |= app.handle_event(event, &mut router);
			}
			if !dirty {
				continue;
			}
			if app.quit {
				break;
			}
			app.run_pending_processes(&mut terminal).await?;
			app.render(terminal.terminal())?;
		}

		// Give the parent the final restorable view before the DDS client exits.
		if let (Some(controller), Some(client)) = (&app.controller, app.dds_client.take()) {
			if controller.online {
				if let Ok(state) = app.snapshot_state() {
					client.publish_to(controller.launch.parent, Body::SessionEnd { state });
				}
			}
			let _ = tokio::time::timeout(std::time::Duration::from_millis(500), client.flush()).await;
		}

		Ok(())
	}

	async fn connect_dds(tx: mpsc::UnboundedSender<Event>, abilities: Vec<String>, parent: Option<dds::PeerId>) -> io::Result<dds::Client> {
		Self::connect_dds_at(tx, &dds::socket_path(), abilities, parent).await
	}

	async fn connect_dds_at(tx: mpsc::UnboundedSender<Event>, socket_path: &Path, abilities: Vec<String>, parent: Option<dds::PeerId>) -> io::Result<dds::Client> {
		let supported: HashSet<_> = abilities.iter().cloned().collect();
		let (client, mut inbox) = dds::Client::connect(socket_path, abilities).await?;
		let self_id = client.id();
		tokio::spawn(async move {
			while let Some(payload) = inbox.recv().await {
				if let Err(error) = Self::validate_dds_message(parent, self_id, &supported, &payload) {
					let _ = tx.send(Event::DdsRejected(error));
					continue;
				}
				if tx.send(Event::DdsDeliver(payload.body)).is_err() {
					break;
				}
			}
		});
		Ok(client)
	}

	fn validate_dds_message(parent: Option<dds::PeerId>, self_id: dds::PeerId, supported: &HashSet<String>, payload: &dds::Payload) -> Result<(), String> {
		let Some(parent) = parent else { return Ok(()) };
		let is_control = payload.receiver == self_id || matches!(payload.body.kind(), "update-tab" | "switch-tab" | "restore-state" | "reveal" | "set-home" | "get-state" | "get-tabs");
		if !is_control { return Ok(()) }
		if payload.sender != parent {
			return Err(format!("rejected DDS control message from unauthorized peer {}", payload.sender));
		}
		let kind = payload.body.kind();
		if !supported.contains(kind) && !supported.contains(dds::WILDCARD_ABILITY) {
			return Err(format!("rejected unsupported DDS control operation '{kind}'"));
		}
		if matches!(kind, "update-tab" | "switch-tab" | "restore-state" | "reveal" | "set-home" | "get-state" | "get-tabs") && payload.receiver != self_id {
			return Err(format!("rejected broadcast DDS {kind} request"));
		}
		if kind == "update-tab" {
			let Body::Custom { data, .. } = &payload.body else {
				return Err("rejected malformed DDS update-tab message".into());
			};
			serde_json::from_value::<IncomingTabUpdate>(data.clone())
				.map_err(|error| format!("rejected invalid DDS update-tab content: {error}"))?;
		} else if kind == "switch-tab" {
			let Body::Custom { data, .. } = &payload.body else {
				return Err("rejected malformed DDS switch-tab message".into());
			};
			serde_json::from_value::<IncomingSwitchTab>(data.clone())
				.map_err(|error| format!("rejected invalid DDS switch-tab content: {error}"))?;
		} else if kind == "reveal" {
			let Body::Custom { data, .. } = &payload.body else {
				return Err("rejected malformed DDS reveal message".into());
			};
			let reveal = serde_json::from_value::<IncomingReveal>(data.clone())
				.map_err(|error| format!("rejected invalid DDS reveal content: {error}"))?;
			if !reveal.path.is_absolute() {
				return Err("rejected invalid DDS reveal content: path must be absolute".into());
			}
		} else if kind == "set-home" {
			let Body::Custom { data, .. } = &payload.body else {
				return Err("rejected malformed DDS set-home message".into());
			};
			let set_home = serde_json::from_value::<IncomingSetHome>(data.clone())
				.map_err(|error| format!("rejected invalid DDS set-home content: {error}"))?;
			if !set_home.path.is_absolute() {
				return Err("rejected invalid DDS set-home content: path must be absolute".into());
			}
		} else if kind == "restore-state" {
			let Body::Custom { data, .. } = &payload.body else {
				return Err("rejected malformed DDS restore-state message".into());
			};
			let state = serde_json::from_value::<crate::session_state::SessionState>(data.clone())
				.map_err(|error| format!("rejected invalid DDS restore-state content: {error}"))?;
			crate::session_state::validate_and_normalize(state)
				.map_err(|error| format!("rejected invalid DDS restore-state content: {error}"))?;
		}
		Ok(())
	}

	fn announce_attach(&self, launch: &dds::DdsLaunch) {
		self.dds_client.as_ref().expect("controlled launch requires a DDS client").publish_to(
			launch.parent,
			Body::Attach { token: launch.token.clone() },
		);
	}

	pub(super) fn update_controller_peers(&mut self, peers: &[dds::PeerInfo]) {
		let Some(controller) = &mut self.controller else { return };
		let parent = peers.iter().find(|peer| peer.id == controller.launch.parent);
		controller.online = parent.is_some();
		controller.abilities = parent.map_or_else(HashSet::new, |peer| peer.abilities.iter().cloned().collect());
	}

	fn new_registry() -> dds::Registry {
		let mut registry = dds::Registry::new();
		registry.sub(
			"core",
			"get-state",
			Box::new(|body| {
				let Body::GetState { query_id } = body else { return Vec::new() };
				vec![crate::command::Command::GetState { query_id: *query_id }]
			}),
		);
		registry.sub(
			"core",
			"get-tabs",
			Box::new(|body| {
				let Body::GetTabs { query_id } = body else { return Vec::new() };
				vec![crate::command::Command::GetTabs { query_id: *query_id }]
			}),
		);
		registry.sub(
			"core",
			"update-tab",
			Box::new(|body| {
				let Body::Custom { data, .. } = body else { return Vec::new() };
				let Ok(state) = serde_json::from_value::<IncomingTabUpdate>(data.clone()) else { return Vec::new() };
				vec![crate::command::Command::UpdateTab { path: state.path, selection: state.selection }]
			}),
		);
		registry.sub(
			"core",
			"switch-tab",
			Box::new(|body| {
				let Body::Custom { data, .. } = body else { return Vec::new() };
				let Ok(target) = serde_json::from_value::<IncomingSwitchTab>(data.clone()) else { return Vec::new() };
				vec![crate::command::Command::SwitchTabTo(target.tab_id)]
			}),
		);
		registry.sub(
			"core",
			"reveal",
			Box::new(|body| {
				let Body::Custom { data, .. } = body else { return Vec::new() };
				let Ok(reveal) = serde_json::from_value::<IncomingReveal>(data.clone()) else { return Vec::new() };
				vec![crate::command::Command::Reveal(reveal.path)]
			}),
		);
		registry.sub(
			"core",
			"set-home",
			Box::new(|body| {
				let Body::Custom { data, .. } = body else { return Vec::new() };
				let Ok(set_home) = serde_json::from_value::<IncomingSetHome>(data.clone()) else { return Vec::new() };
				vec![crate::command::Command::SetHome(set_home.path)]
			}),
		);
		registry.sub(
			"core",
			"restore-state",
			Box::new(|body| {
				let Body::Custom { data, .. } = body else { return Vec::new() };
				let Ok(state) = serde_json::from_value::<crate::session_state::SessionState>(data.clone()) else { return Vec::new() };
				vec![crate::command::Command::RestoreState(state)]
			}),
		);
		registry
	}

	fn handle_event(&mut self, event: Event, router: &mut Router) -> bool {
		let before = self.hovered_path();
		let dirty = match event {
			Event::Term(crossterm::event::Event::Key(key)) if key.kind == KeyEventKind::Press => self.handle_key(key, router),
			Event::Term(crossterm::event::Event::Mouse(mouse)) => self.handle_mouse(mouse),
			Event::Term(crossterm::event::Event::FocusGained) => self.set_terminal_focus(true),
			Event::Term(crossterm::event::Event::FocusLost) => self.set_terminal_focus(false),
			Event::Term(crossterm::event::Event::Resize(_, _)) => true,
			Event::Term(_) => false,
			event => {
				Dispatcher::dispatch_event(self, event);
				true
			}
		};
		let after = self.hovered_path();
		if after != before {
			self.publish(Body::Hover { path: after });
		}
		dirty
	}

	fn hovered_path(&self) -> Option<PathBuf> {
		self.active_tab().visible_at(self.active_tab().cursor).map(|(_, node)| node.path.clone())
	}

	fn set_terminal_focus(&mut self, focused: bool) -> bool {
		let changed = self.terminal_focused != focused;
		self.terminal_focused = focused;
		changed
	}

	fn handle_mouse(&mut self, event: MouseEvent) -> bool {
		if !self.config.ui.mouse { return false; }
		// Like Yazi, overlays own the input layer: do not let a click leak
		// through to the manager underneath them.
		if self.pending_quit || self.tasks.visible || self.open_picker.is_some() || self.entry_details || self.active_tab().pending_delete.is_some() || self.active_tab().input.is_some() || !self.which.is_empty() {
			self.mouse.resizing = false;
			return false;
		}

		let point = (event.column, event.row);
		match event.kind {
			MouseEventKind::Down(MouseButton::Left) => {
				if let Some(preview) = self.mouse.preview
					&& match self.mouse.preview_layout {
						PreviewLayout::Vertical => event.row == preview.y,
						PreviewLayout::Auto | PreviewLayout::Horizontal => event.column == preview.x,
					}
					&& contains(self.mouse.body, point)
				{
					self.mouse.resizing = true;
					return true;
				}
				if contains(self.mouse.tabs, point) {
					let labels = self.tab_labels();
					if let Some(index) = crate::tui::widgets::TabBar::hit_test(self.mouse.tabs, &labels, event.column) {
						self.active = self.tabs[index].id;
						return true;
					}
				}
				self.point_tree_cursor(point)
			}
			MouseEventKind::Down(MouseButton::Right) => {
				if !self.point_tree_cursor(point) {
					return false;
				}
				self.active_tab_mut().toggle_expand_selected();
				true
			}
			MouseEventKind::Up(MouseButton::Left) => {
				let dirty = self.mouse.resizing;
				self.mouse.resizing = false;
				dirty
			}
			MouseEventKind::Drag(MouseButton::Left) if self.mouse.resizing => self.resize_preview(event.column, event.row),
			MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
				let step = if matches!(event.kind, MouseEventKind::ScrollUp) { -1 } else { 1 };
				if self.mouse.preview.is_some_and(|area| contains(area, point)) {
					self.active_tab_mut().preview.seek(step);
					true
				} else if contains(self.mouse.tree, point) {
					self.active_tab_mut().move_cursor(step as isize);
					true
				} else {
					false
				}
			}
			_ => false,
		}
	}

	fn point_tree_cursor(&mut self, point: (u16, u16)) -> bool {
		if !contains(self.mouse.tree, point) {
			return false;
		}
		let cursor = self.mouse.tree_row_offset + (point.1 - self.mouse.tree.y) as usize;
		if cursor >= self.active_tab().visible_len() {
			return false;
		}
		let delta = cursor as isize - self.active_tab().cursor as isize;
		self.active_tab_mut().move_cursor(delta);
		true
	}

	fn resize_preview(&mut self, column: u16, row: u16) -> bool {
		let (tree, extent) = match self.mouse.preview_layout {
			PreviewLayout::Vertical => (row.saturating_sub(self.mouse.body.y).min(self.mouse.body.height), self.mouse.body.height),
			PreviewLayout::Auto | PreviewLayout::Horizontal => (column.saturating_sub(self.mouse.body.x).min(self.mouse.body.width), self.mouse.body.width),
		};
		if extent == 0 { return false }
		let tree_percent = (tree as u32 * 100 / extent as u32).clamp(20, 80) as u16;
		self.mouse.preview_percent = 100 - tree_percent;
		true
	}

	pub(super) fn tab_labels(&self) -> Vec<(bool, String)> {
		self.tabs.iter().map(|tab| {
			let name = tab.tree.root.path.file_name().map_or_else(|| tab.tree.root.path.display().to_string(), |name| name.to_string_lossy().into_owned());
			(tab.id == self.active, name)
		}).collect()
	}

	fn handle_key(&mut self, key: crossterm::event::KeyEvent, router: &mut Router) -> bool {
		if self.pending_quit {
			let confirmed = match key.code {
				KeyCode::Char('y') => Some(true),
				KeyCode::Enter | KeyCode::Esc | KeyCode::Char('n') => Some(false),
				KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(false),
				_ => None,
			};
			let Some(confirmed) = confirmed else {
				return false;
			};
			self.resolve_pending_quit(confirmed);
			return true;
		}
		if self.tasks.visible {
			return match key.code {
				KeyCode::Up | KeyCode::Char('k') => {
					self.tasks.move_cursor(-1);
					true
				}
				KeyCode::Down | KeyCode::Char('j') => {
					self.tasks.move_cursor(1);
					true
				}
				KeyCode::Char('x') => {
					self.tasks.cancel_selected();
					true
				}
				KeyCode::Esc | KeyCode::Char('w') | KeyCode::Char('q') => {
					self.tasks.visible = false;
					true
				}
				_ => false,
			};
		}
		if self.entry_details {
			return match key.code {
				KeyCode::Up | KeyCode::Char('k') => {
					self.entry_details_scroll = self.entry_details_scroll.saturating_sub(1);
					true
				}
				KeyCode::Down | KeyCode::Char('j') => {
					self.entry_details_scroll = self.entry_details_scroll.saturating_add(1);
					true
				}
				KeyCode::Esc | KeyCode::Char('q') => {
					self.entry_details = false;
					true
				}
				_ => false,
			};
		}
		if self.open_picker.is_some() {
			return match key.code {
				KeyCode::Up | KeyCode::Char('k') => {
					self.move_open_picker(-1);
					true
				}
				KeyCode::Down | KeyCode::Char('j') => {
					self.move_open_picker(1);
					true
				}
				KeyCode::Enter => {
					self.submit_open_picker();
					true
				}
				KeyCode::Esc | KeyCode::Char('q') => {
					self.open_picker = None;
					true
				}
				_ => false,
			};
		}
		if self.active_tab().pending_delete.is_some() {
			let submit = match key.code {
				KeyCode::Char('y') => Some(true),
				KeyCode::Enter | KeyCode::Esc | KeyCode::Char('n') => Some(false),
				KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(false),
				_ => None,
			};
			let Some(submit) = submit else { return false };
			if let Some((targets, mode)) = self.active_tab_mut().take_pending_delete(submit) {
				self.enqueue_delete(targets, mode);
			}
			return true;
		}
		if self.active_tab().input.is_some() {
			if let Some(command) = self.active_tab_mut().handle_input_key(key) { self.execute(command); }
			return true;
		}

		match router.route(KeyContext::Manager, Key::from(key)) {
			Route::Commands(commands) => {
				self.which.clear();
				for command in commands {
					self.execute(command);
				}
				true
			}
			Route::Pending(candidates) => {
				self.which = if self.config.ui.which_key { candidates } else { Vec::new() };
				true
			}
			Route::Unmatched if self.which.is_empty() => false,
			Route::Unmatched => {
				self.which.clear();
				true
			}
		}
	}

	async fn run_pending_processes(&mut self, terminal: &mut TerminalSession) -> io::Result<()> {
		while let Some(request) = self.processes.pop_front() {
			let blocking = request.mode().blocks_terminal();
			if blocking {
				terminal.suspend();
			}
			let completion = request.execute().await;
			if blocking {
				terminal.resume()?;
			}
			self.on_process_completion(completion);
		}
		Ok(())
	}

	pub(super) fn active_tab(&self) -> &Tab {
		self.tabs.iter().find(|t| t.id == self.active).expect("active always names an existing tab")
	}

	pub(super) fn active_tab_mut(&mut self) -> &mut Tab {
		self.tabs.iter_mut().find(|t| t.id == self.active).expect("active always names an existing tab")
	}

	/// For routing a background event tagged with a tab id — unlike
	/// `active_tab_mut`, `None` is a normal outcome (the tab it was headed
	/// for closed before the event arrived), not a bug.
	pub(super) fn tab_mut(&mut self, id: usize) -> Option<&mut Tab> {
		self.tabs.iter_mut().find(|t| t.id == id)
	}

	pub(super) fn staged_tab_mut(&mut self, id: usize) -> Option<&mut Tab> {
		self.staged_session.as_mut()?.tab_mut(id)
	}

	pub(super) fn is_staged_tab(&self, id: usize) -> bool {
		self.staged_session.as_ref().is_some_and(|session| session.contains(id))
	}

	/// Starts building a complete replacement session without touching the
	/// visible tabs. Validation happens before the previous staged attempt is
	/// superseded, and allocated IDs are never reused after a failed attempt.
	pub(super) fn begin_restore(&mut self, state: crate::session_state::SessionState) -> Result<(), String> {
		let state = crate::session_state::validate_and_normalize(state)?;
		self.begin_normalized_restore(state).map_err(|error| error.to_string())
	}

	fn begin_normalized_restore(&mut self, state: crate::session_state::SessionState) -> io::Result<()> {
		let first_id = self.next_tab_id;
		self.next_tab_id = self.next_tab_id.checked_add(state.tabs.len())
			.ok_or_else(|| io::Error::other("tab ID space exhausted"))?;
		let staged = StagedSession::open(first_id, state, self.tx.clone(), self.config.clone())
			.map_err(io::Error::other)?;
		self.restore_error = None;
		self.staged_session = Some(staged);
		Ok(())
	}

	async fn finish_startup_restore(&mut self, rx: &mut mpsc::UnboundedReceiver<Event>) -> io::Result<()> {
		while self.staged_session.is_some() {
			let event = rx.recv().await.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "event channel closed during startup session restore"))?;
			Dispatcher::dispatch_event(self, event);
		}
		match self.restore_error.take() {
			Some(error) => Err(io::Error::other(format!("startup session restore failed: {error}"))),
			None => Ok(()),
		}
	}

	pub(super) fn on_staged_loaded(&mut self, tab: usize, path: PathBuf, ticket: u64, result: io::Result<Vec<(PathBuf, crate::fs::Cha)>>, done: bool) {
		let step = self.staged_session.as_mut().expect("staged tab must belong to a staged session")
			.on_loaded(tab, path, ticket, result, done);
		match step {
			SessionRestoreStep::Pending => {}
			SessionRestoreStep::Complete => {
				let mut staged = self.staged_session.take().unwrap();
				// Home and tabs are one atomic commit: nothing above this
				// point has touched App state, so a failed restore keeps both.
				if let Some(home) = staged.take_home() {
					self.home = home;
				}
				let (tabs, active) = staged.into_tabs();
				self.tabs = tabs;
				self.active = active;
			}
			SessionRestoreStep::Failed(error) => {
				self.staged_session = None;
				self.restore_error = Some(error.clone());
				self.notices.push(Notice::new(
					NoticeLevel::Warn,
					format!("Cannot restore session: {error}"),
					std::time::Duration::from_secs(8),
				));
			}
		}
	}

	/// Opens a new tab rooted at wherever the active one currently is,
	/// yazi-style (`tt`), and switches to it.
	pub fn new_tab(&mut self) {
		let path = self.active_tab().tree.root.path.clone();
		let id = self.next_tab_id;
		self.next_tab_id += 1;
		if let Ok(tab) = Tab::open_configured(id, path, self.tx.clone(), self.config.clone()) {
			self.tabs.push(tab);
			self.active = id;
		}
	}

	/// Closes the active tab and lands on whichever one now sits in its
	/// place (or the last tab, if it was the rightmost). Refuses to close
	/// the last remaining tab — there's always at least one.
	pub fn close_tab(&mut self) {
		if self.tabs.len() <= 1 {
			return;
		}
		let Some(pos) = self.tabs.iter().position(|t| t.id == self.active) else {
			return;
		};
		self.tabs.remove(pos);
		self.active = self.tabs[pos.min(self.tabs.len() - 1)].id;
	}

	pub fn switch_tab(&mut self, delta: isize) {
		let Some(pos) = self.tabs.iter().position(|t| t.id == self.active) else {
			return;
		};
		let next = (pos as isize + delta).rem_euclid(self.tabs.len() as isize) as usize;
		self.active = self.tabs[next].id;
	}

	pub fn switch_tab_to(&mut self, id: usize) {
		if self.tabs.iter().any(|tab| tab.id == id) {
			self.active = id;
		} else {
			self.active_tab_mut().raise(NoticeLevel::Warn, format!("Tab {id} no longer exists"));
		}
	}

	/// Quits outright when nothing's running; otherwise arms a confirmation
	/// instead of just doing it — see `pending_quit`.
	pub fn request_quit(&mut self) {
		if self.tasks.tasks.is_empty() {
			self.quit = true;
		} else {
			self.pending_quit = true;
		}
	}

	/// Resolves an armed quit confirmation. Declining just clears it —
	/// there's nothing to take or hand off, unlike a confirmed delete.
	pub(super) fn resolve_pending_quit(&mut self, confirmed: bool) {
		self.pending_quit = false;
		if confirmed {
			self.quit = true;
		}
	}

	pub fn move_page(&mut self, percent: i8) {
		let rows = self.tree_rows.max(1) as isize;
		let mut delta = rows * percent as isize / 100;
		if delta == 0 && percent != 0 {
			delta = percent.signum() as isize;
		}
		self.active_tab_mut().move_cursor(delta);
	}

	pub fn center_cursor(&mut self) {
		let rows = self.tree_rows.max(1);
		let tab = self.active_tab_mut();
		let max_scroll = tab.visible_len().saturating_sub(rows);
		tab.scroll = tab.cursor.saturating_sub(rows / 2).min(max_scroll);
	}

	/// Yanks the active tab's targets onto the shared clipboard, so any tab
	/// can paste them afterward.
	pub fn yank_selected(&mut self, cut: bool) {
		let targets = self.active_tab_mut().take_yank_targets();
		if targets.is_empty() {
			return;
		}
		self.clipboard = targets;
		self.clipboard_cut = cut;
		self.publish(Body::Yank { paths: self.clipboard.clone(), cut });
	}

	/// Publishes an implicit event locally. An explicit broadcast rule makes
	/// it public; otherwise an interested controlling parent receives it
	/// directly. Only one external route is used for each event.
	pub(super) fn publish(&self, body: Body) {
		let _ = self.tx.send(Event::DdsDeliver(body.clone()));
		let Some(client) = &self.dds_client else { return };
		if self.config.dds.broadcast.iter().any(|kind| kind == body.kind()) {
			client.publish(body);
		} else if let Some(controller) = &self.controller
			&& controller.online
			&& (controller.abilities.contains(body.kind()) || controller.abilities.contains(dds::WILDCARD_ABILITY))
		{
			client.publish_to(controller.launch.parent, body);
		}
	}

	/// An `emit` command is an explicit request to publish, so it bypasses
	/// the implicit-event allowlist while still respecting `dds.enabled`.
	/// Local subscribers always see it. Externally it is either a public
	/// broadcast or, with `parent`, a direct message to the controlling
	/// parent that never falls back to broadcasting.
	pub(super) fn emit(&mut self, body: Body, parent: bool) {
		let _ = self.tx.send(Event::DdsDeliver(body.clone()));
		if !parent {
			if let Some(client) = &self.dds_client {
				client.publish(body);
			}
			return;
		}
		let Some(controller) = &self.controller else {
			self.active_tab_mut().raise(NoticeLevel::Error, "emit --parent requires a controlling parent");
			return;
		};
		if !controller.online {
			self.active_tab_mut().raise(NoticeLevel::Error, "controller unavailable");
			return;
		}
		if let Some(client) = &self.dds_client {
			client.publish_to(controller.launch.parent, body);
		}
	}

	pub(super) fn update_tab(&mut self, path: Option<PathBuf>, selection: Vec<PathBuf>) {
		if let Some(path) = path
			&& let Err(error) = self.active_tab_mut().cd(path)
		{
			self.notices.push(Notice::new(
				NoticeLevel::Error,
				format!("Cannot apply DDS state: {error}"),
				std::time::Duration::from_secs(8),
			));
			return;
		}
		self.active_tab_mut().set_selection(selection);
	}

	/// Changes the session home used by `g=` in every tab. An invalid target
	/// leaves the current home untouched and reports why.
	pub(super) fn set_home(&mut self, path: PathBuf) {
		let checked = crate::fs::absolute_lexical(&path).and_then(|home| match std::fs::metadata(&home) {
			Ok(metadata) if metadata.is_dir() => Ok(home),
			Ok(_) => Err(io::Error::other("not a directory")),
			Err(error) => Err(error),
		});
		match checked {
			Ok(home) => self.home = home,
			Err(error) => self.notices.push(Notice::new(
				NoticeLevel::Warn,
				format!("Cannot set home to {}: {error}", path.display()),
				std::time::Duration::from_secs(8),
			)),
		}
	}

	pub(super) fn reveal_path(&mut self, path: PathBuf) {
		if let Err(error) = self.active_tab_mut().reveal(path) {
			self.active_tab_mut().raise(NoticeLevel::Error, format!("Cannot reveal path: {error}"));
		}
	}

	pub(super) fn restore_state(&mut self, state: crate::session_state::SessionState) {
		if let Err(error) = self.begin_restore(state) {
			self.notices.push(Notice::new(
				NoticeLevel::Warn,
				format!("Cannot restore session: {error}"),
				std::time::Duration::from_secs(8),
			));
		}
	}

	pub(super) fn snapshot_state(&self) -> Result<crate::session_state::SessionState, String> {
		let active_tab = self.tabs.iter().position(|tab| tab.id == self.active)
			.ok_or_else(|| "active tab is missing".to_owned())?;
		let state = crate::session_state::SessionState {
			version: crate::session_state::SESSION_STATE_VERSION,
			active_tab,
			// A home that has since vanished is left out rather than making
			// the whole snapshot (and `tuzi-exit`) fail.
			home: self.home.is_dir().then(|| self.home.clone()),
			tabs: self.tabs.iter().map(Tab::snapshot_state).collect(),
		};
		crate::session_state::validate_and_normalize(state)
	}

	pub(super) fn reply_state(&self, query_id: u64) {
		let (Some(controller), Some(client)) = (&self.controller, &self.dds_client) else { return };
		let body = match self.snapshot_state() {
			Ok(state) => Body::State { query_id, state },
			Err(error) => Body::StateError { query_id, error },
		};
		client.publish_to(controller.launch.parent, body);
	}

	pub(super) fn reply_tabs(&self, query_id: u64) {
		let (Some(controller), Some(client)) = (&self.controller, &self.dds_client) else { return };
		let tabs = self.tabs.iter().map(|tab| dds::TabInfo { id: tab.id, cwd: tab.tree.root.path.clone() }).collect();
		client.publish_to(controller.launch.parent, Body::Tabs { query_id, active_tab_id: self.active, tabs });
	}

	pub fn request_delete(&mut self, mode: DeleteMode) {
		let confirm = match mode {
			DeleteMode::Trash => self.config.confirm.trash,
			DeleteMode::Permanent => self.config.confirm.delete,
		};
		if let Some((targets, mode)) = self.active_tab_mut().delete_selected_configured(mode, confirm) {
			self.enqueue_delete(targets, mode);
		}
	}

	/// Pastes the shared clipboard into the active tab. A cut clipboard is
	/// consumed on the first successful paste; a copy can be pasted again.
	pub fn paste(&mut self) {
		if self.clipboard.is_empty() {
			return;
		}
		let paths = self.clipboard.clone();
		let cut = self.clipboard_cut;
		let tab = self.active;
		let Some(target) = self.active_tab().paste_destination() else {
			return;
		};
		let conflicts = self.tasks.enqueue_with_policy(paths, target, cut, tab, self.config.fs.paste_conflict);
		if conflicts > 0 { self.active_tab_mut().raise(NoticeLevel::Warn, format!("Skipped {conflicts} conflicting paste target(s)")); }
		if cut && conflicts == 0 {
			self.clipboard.clear();
			self.clipboard_cut = false;
		}
	}

	/// Symlinks the clipboard into the active tab instead of copying or
	/// moving it — near-instant, so unlike `paste` it skips the task queue
	/// entirely. Never consumes the clipboard: the source is left untouched
	/// either way, so there's nothing for a cut to finish.
	pub fn paste_link(&mut self, absolute: bool) {
		if self.clipboard.is_empty() {
			return;
		}
		let paths = self.clipboard.clone();
		let Some(target) = self.active_tab().paste_destination() else {
			return;
		};
		self.active_tab_mut().fs_scheduler.link(paths, target, absolute);
	}

	pub fn copy_to_system_clipboard(&mut self, kind: CopyKind) {
		let content = self.active_tab_mut().copy_text(kind);
		if content.is_empty() {
			self.active_tab_mut().raise(NoticeLevel::Warn, "Nothing to copy");
			return;
		}
		crate::clipboard::set(content);
	}

	pub(super) fn on_task_event(&mut self, event: TaskEvent) {
		let Some((tab, kind, subject)) = self.tasks.accept(event) else {
			return;
		};
		if matches!(kind, TaskKind::Trash | TaskKind::Delete) {
			self.forget_clipboard_path(&subject);
		}
		self.publish(Body::TaskDone { kind, ok: true });
		let Some(tab) = self.tab_mut(tab) else { return };
		match kind {
			TaskKind::Copy | TaskKind::Move => tab.on_pasted(subject),
			TaskKind::Trash | TaskKind::Delete => tab.on_deleted(vec![subject]),
		}
	}

	/// Clipboard markers identify filesystem objects only by path. Forget a
	/// deleted path and its descendants so a replacement created at the same
	/// location cannot inherit a stale copy/cut marker.
	pub(super) fn forget_clipboard_path(&mut self, deleted: &Path) {
		self.clipboard.retain(|path| path != deleted && !path.starts_with(deleted));
		if self.clipboard.is_empty() {
			self.clipboard_cut = false;
		}
	}

	/// Sends a confirmed delete to the task queue: trash by default, or a
	/// permanent removal for `DeleteMode::Permanent`. Both become
	/// observable, cancelable tasks the same way paste does.
	fn enqueue_delete(&mut self, targets: Vec<PathBuf>, mode: DeleteMode) {
		let tab = self.active;
		match mode {
			DeleteMode::Trash => self.tasks.enqueue_trash(targets, tab),
			DeleteMode::Permanent => self.tasks.enqueue_delete(targets, tab),
		}
	}

	/// Queues a toast. No animation, no dismiss key — it just sits until its
	/// level's timeout elapses, at which point the scheduled redraw below
	/// notices `prune_notices` dropped it, even if nothing else happens in
	/// the meantime.
	pub fn push_notice(&mut self, level: NoticeLevel, message: impl Into<String>) {
		let seconds = match level {
			NoticeLevel::Info => self.config.notify.info_timeout,
			NoticeLevel::Warn => self.config.notify.warn_timeout,
			NoticeLevel::Error => self.config.notify.error_timeout,
		};
		let notice = Notice::new(level, message, std::time::Duration::from_secs(seconds));
		let wakeup = notice.remaining();
		self.notices.push(notice);

		let tx = self.tx.clone();
		tokio::spawn(async move {
			tokio::time::sleep(wakeup).await;
			let _ = tx.send(Event::Redraw);
		});
	}

	pub(super) fn prune_notices(&mut self) {
		self.notices.retain(|n| !n.expired());
	}

	/// Moves whatever one-off message each tab has queued for itself (an
	/// invalid cd, a refused delete, …) into the toast queue. Tabs can't
	/// push a toast directly — only `App` owns `notices` — so this outbox
	/// is drained after every dispatch, regardless of which tab set it.
	pub(super) fn drain_tab_notices(&mut self) {
		let pending: Vec<(NoticeLevel, String)> = self.tabs.iter_mut().filter_map(|tab| tab.pending_notice.take()).collect();
		for (level, message) in pending {
			self.push_notice(level, message);
		}
	}
}

fn contains(area: Rect, (x, y): (u16, u16)) -> bool {
	x >= area.x && x < area.right() && y >= area.y && y < area.bottom()
}

#[cfg(test)]
mod tests {
	use std::{fs, path::Path};
	use crossterm::event::KeyEvent;

	use crate::{
		command::Command,
		column_mode::ColumnMode,
		config::DdsOpen,
		session_state::{SESSION_STATE_VERSION, SessionState, TabState},
	};

	use super::*;

	async fn app(root: &Path) -> (App, mpsc::UnboundedReceiver<Event>) {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let first = Tab::open(0, root.to_path_buf(), tx.clone()).unwrap();
		let mut app = App {
			home: crate::fs::absolute_lexical(root).unwrap(),
			config: Arc::new(Config::default()),
			tabs: vec![first],
			active: 0,
			quit: false,
			pending_quit: false,
			next_tab_id: 1,
			staged_session: None,
			restore_error: None,
			clipboard: Vec::new(),
			clipboard_cut: false,
			tree_rows: 0,
			terminal_focused: true,
			mouse: MouseState::default(),
			which: Vec::new(),
			icon_theme: IconTheme::default(),
			theme: Theme::default(),
			open: OpenScheduler::new(tx.clone()),
			open_picker: None,
			entry_details: false,
			entry_details_scroll: 0,
			filename_peek: false,
			processes: VecDeque::new(),
			tasks: TaskManager::new(tx.clone()),
			notices: Vec::new(),
			pubsub: App::new_registry(),
			dds_client: None,
			controller: None,
			tx,
		};

		// drain the root tab's initial listing so it's got visible rows
		pump(&mut app, &mut rx).await;

		(app, rx)
	}

	#[tokio::test]
	async fn gh_returns_active_tab_to_fixed_session_home() {
		let root = std::env::temp_dir().join(format!("tuzi-session-home-{}", std::process::id()));
		fs::create_dir_all(root.join("child")).unwrap();
		let (mut app, mut rx) = app(&root).await;
		app.active_tab_mut().cd(root.join("child")).unwrap();
		pump(&mut app, &mut rx).await;
		app.new_tab();
		pump(&mut app, &mut rx).await;
		let mut router = Router::default();
		app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE), &mut router);
		app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), &mut router);
		pump(&mut app, &mut rx).await;
		assert_eq!(app.active_tab().tree.root.path, app.home);
		assert_eq!(app.tabs[0].tree.root.path, root.join("child"));
		fs::remove_dir_all(root).unwrap();
	}

	async fn pump(app: &mut App, rx: &mut mpsc::UnboundedReceiver<Event>) {
		loop {
			let event = rx.recv().await.unwrap();
			let task_finished = matches!(&event, Event::Task(TaskEvent::Finished { .. }));
			let task_event = matches!(&event, Event::Task(_));
			let listing_pending = matches!(&event, Event::Loaded { done: false, .. });
			let publish_event = matches!(&event, Event::DdsPublish(_));
			// A DDS publish (e.g. from `cd`/`yank`/rename) has no subscriber
			// yet in these tests, but it's still queued ahead of whatever
			// "real" event the test is waiting for — keep draining past it.
			let pubsub_event = matches!(&event, Event::DdsDeliver(_) | Event::DdsRejected(_));
			Dispatcher::dispatch_event(app, event);
			if (!task_event || task_finished) && !listing_pending && !publish_event && !pubsub_event {
				break;
			}
		}
	}

	async fn pump_restore(app: &mut App, rx: &mut mpsc::UnboundedReceiver<Event>) {
		while app.staged_session.is_some() {
			let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await
				.expect("restore timed out").expect("restore event channel closed");
			Dispatcher::dispatch_event(app, event);
		}
	}

	fn session(active_tab: usize, tabs: Vec<TabState>) -> SessionState {
		SessionState { version: SESSION_STATE_VERSION, active_tab, home: None, tabs }
	}

	#[tokio::test]
	async fn staged_session_replaces_all_tabs_atomically_and_preserves_order_and_active_tab() {
		let old = std::env::temp_dir().join("tuzi-app-test-restore-old");
		let first = std::env::temp_dir().join("tuzi-app-test-restore-first");
		let second = std::env::temp_dir().join("tuzi-app-test-restore-second");
		for path in [&old, &first, &second] { let _ = fs::remove_dir_all(path); fs::create_dir_all(path).unwrap(); }
		fs::create_dir_all(first.join("src/deep")).unwrap();
		fs::write(first.join("src/deep/cursor"), "").unwrap();
		fs::write(second.join("selected"), "").unwrap();
		let (mut app, mut rx) = app(&old).await;
		let old_id = app.active;
		app.begin_restore(session(1, vec![
			TabState {
				cwd: first.clone(), cursor: Some(first.join("src/deep/cursor")), selection: Vec::new(), expanded: Vec::new(),
			},
			TabState {
				cwd: second.clone(), cursor: None, selection: vec![second.join("selected")], expanded: Vec::new(),
			},
		])).unwrap();
		assert_eq!(app.tabs.len(), 1, "old session stays visible while staging");
		assert_eq!(app.active, old_id);
		pump_restore(&mut app, &mut rx).await;
		assert_eq!(app.tabs.len(), 2);
		assert_eq!(app.tabs[0].tree.root.path, first.canonicalize().unwrap());
		assert_eq!(app.tabs[1].tree.root.path, second.canonicalize().unwrap());
		assert_eq!(app.active, app.tabs[1].id);
		assert_eq!(app.tabs[0].visible_at(app.tabs[0].cursor).unwrap().1.path, first.canonicalize().unwrap().join("src/deep/cursor"));
		assert!(app.tabs[1].selection.contains(&second.canonicalize().unwrap().join("selected")));
		for path in [old, first, second] { fs::remove_dir_all(path).unwrap(); }
	}

	#[tokio::test]
	async fn one_staged_tab_failure_rolls_back_the_entire_session() {
		let old = std::env::temp_dir().join("tuzi-app-test-restore-rollback-old");
		let replacement = std::env::temp_dir().join("tuzi-app-test-restore-rollback-new");
		for path in [&old, &replacement] { let _ = fs::remove_dir_all(path); fs::create_dir_all(path).unwrap(); }
		let (mut app, _rx) = app(&old).await;
		let old_root = app.active_tab().tree.root.path.clone();
		let staged_id = app.next_tab_id;
		app.begin_restore(session(0, vec![TabState {
			cwd: replacement.clone(), cursor: None, selection: Vec::new(), expanded: Vec::new(),
		}])).unwrap();
		Dispatcher::dispatch_event(&mut app, Event::Loaded {
			tab: staged_id,
			path: replacement.canonicalize().unwrap(),
			ticket: 0,
			result: Err(io::Error::new(io::ErrorKind::PermissionDenied, "synthetic failure")),
			done: true,
		});
		assert!(app.staged_session.is_none());
		assert_eq!(app.tabs.len(), 1);
		assert_eq!(app.active_tab().tree.root.path, old_root);
		assert!(app.notices.iter().any(|notice| notice.message.contains("synthetic failure")));
		for path in [old, replacement] { fs::remove_dir_all(path).unwrap(); }
	}

	#[tokio::test]
	async fn stale_listing_from_a_failed_restore_cannot_touch_a_new_attempt() {
		let old = std::env::temp_dir().join("tuzi-app-test-restore-stale-old");
		let failed = std::env::temp_dir().join("tuzi-app-test-restore-stale-failed");
		let replacement = std::env::temp_dir().join("tuzi-app-test-restore-stale-new");
		for path in [&old, &failed, &replacement] { let _ = fs::remove_dir_all(path); fs::create_dir_all(path).unwrap(); }
		let (mut app, mut rx) = app(&old).await;
		let failed_id = app.next_tab_id;
		app.begin_restore(session(0, vec![TabState { cwd: failed.clone(), cursor: None, selection: Vec::new(), expanded: Vec::new() }])).unwrap();
		Dispatcher::dispatch_event(&mut app, Event::Loaded {
			tab: failed_id, path: failed.canonicalize().unwrap(), ticket: 0,
			result: Err(io::Error::other("failed attempt")), done: true,
		});
		let replacement_id = app.next_tab_id;
		app.begin_restore(session(0, vec![TabState { cwd: replacement.clone(), cursor: None, selection: Vec::new(), expanded: Vec::new() }])).unwrap();
		assert_ne!(failed_id, replacement_id);
		Dispatcher::dispatch_event(&mut app, Event::Loaded {
			tab: failed_id, path: failed.canonicalize().unwrap(), ticket: 0, result: Ok(Vec::new()), done: true,
		});
		assert!(app.staged_session.is_some(), "stale event must not finish or cancel the new attempt");
		pump_restore(&mut app, &mut rx).await;
		assert_eq!(app.active_tab().tree.root.path, replacement.canonicalize().unwrap());
		for path in [old, failed, replacement] { fs::remove_dir_all(path).unwrap(); }
	}

	#[tokio::test]
	async fn startup_restore_returns_an_error_instead_of_entering_the_tui_on_failure() {
		let old = std::env::temp_dir().join("tuzi-app-test-startup-restore-old");
		let replacement = std::env::temp_dir().join("tuzi-app-test-startup-restore-new");
		for path in [&old, &replacement] { let _ = fs::remove_dir_all(path); fs::create_dir_all(path).unwrap(); }
		let (mut app, mut rx) = app(&old).await;
		let staged_id = app.next_tab_id;
		app.begin_restore(session(0, vec![TabState {
			cwd: replacement.clone(), cursor: None, selection: Vec::new(), expanded: Vec::new(),
		}])).unwrap();
		app.tx.send(Event::Loaded {
			tab: staged_id, path: replacement.canonicalize().unwrap(), ticket: 0,
			result: Err(io::Error::other("startup failure")), done: true,
		}).unwrap();
		let error = app.finish_startup_restore(&mut rx).await.unwrap_err();
		assert!(error.to_string().contains("startup failure"));
		for path in [old, replacement] { fs::remove_dir_all(path).unwrap(); }
	}

	fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
		MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
	}

	#[tokio::test]
	async fn terminal_focus_events_redraw_only_when_focus_changes() {
		let root = std::env::temp_dir().join("tuzi-app-test-terminal-focus");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;
		let mut router = Router::default();

		assert!(app.terminal_focused);
		assert!(app.handle_event(Event::Term(crossterm::event::Event::FocusLost), &mut router));
		assert!(!app.terminal_focused);
		assert!(!app.handle_event(Event::Term(crossterm::event::Event::FocusLost), &mut router));
		assert!(app.handle_event(Event::Term(crossterm::event::Event::FocusGained), &mut router));
		assert!(app.terminal_focused);

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn disabled_mouse_and_which_key_do_not_expose_ui_overlays() {
		let root = std::env::temp_dir().join("tuzi-app-test-disabled-ui-input");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;
		let config = Arc::make_mut(&mut app.config);
		config.ui.mouse = false;
		config.ui.which_key = false;
		assert!(!app.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0)));

		let mut router = Router::default();
		assert!(app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE), &mut router));
		assert!(app.which.is_empty());
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn entry_details_is_modal_and_closes_with_q_or_escape() {
		let root = std::env::temp_dir().join("tuzi-app-test-entry-details");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;
		let mut router = Router::default();

		assert!(app.handle_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT), &mut router));
		assert!(app.entry_details);
		assert!(app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), &mut router));
		assert!(!app.entry_details);

		app.handle_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT), &mut router);
		assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut router));
		assert!(!app.entry_details);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn mouse_click_and_wheel_move_the_tree_cursor() {
		let root = std::env::temp_dir().join("tuzi-app-test-mouse-tree");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("a"), b"a").unwrap();
		fs::write(root.join("b"), b"b").unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;
		app.mouse.tree = Rect::new(4, 3, 40, 8);
		app.mouse.tree_row_offset = 0;

		assert!(app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 8, 4)));
		assert_eq!(app.active_tab().cursor, 1);
		assert!(app.handle_mouse(mouse(MouseEventKind::ScrollDown, 8, 4)));
		assert_eq!(app.active_tab().cursor, 2);

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn preview_resize_uses_columns_side_by_side_and_rows_when_stacked() {
		let root = std::env::temp_dir().join("tuzi-app-test-preview-resize");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;
		app.mouse.body = Rect::new(10, 5, 100, 50);

		app.mouse.preview_layout = PreviewLayout::Horizontal;
		assert!(app.resize_preview(70, 0));
		assert_eq!(app.mouse.preview_percent, 40);

		app.mouse.preview_layout = PreviewLayout::Vertical;
		assert!(app.resize_preview(0, 35));
		assert_eq!(app.mouse.preview_percent, 40);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn modal_layer_prevents_mouse_clicks_reaching_the_tree() {
		let root = std::env::temp_dir().join("tuzi-app-test-mouse-modal");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("a"), b"a").unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;
		app.mouse.tree = Rect::new(0, 3, 40, 8);
		app.pending_quit = true;

		assert!(!app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 4)));
		assert_eq!(app.active_tab().cursor, 0);

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn right_click_toggles_a_directory_without_entering_it() {
		let root = std::env::temp_dir().join("tuzi-app-test-mouse-right-click");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("dir")).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;
		app.mouse.tree = Rect::new(0, 3, 40, 8);
		let original_root = app.active_tab().tree.root.path.clone();

		assert!(app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), 2, 4)));
		assert_eq!(app.active_tab().tree.root.path, original_root);
		assert!(app.active_tab().visible_at(1).unwrap().1.expanded);
		assert!(app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), 2, 4)));
		assert!(!app.active_tab().visible_at(1).unwrap().1.expanded);

		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn yank_then_paste_copies_into_the_directory_under_the_cursor() {
		let root = std::env::temp_dir().join("tuzi-app-test-paste");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("src")).unwrap();
		fs::create_dir_all(root.join("dst/sub")).unwrap();
		fs::write(root.join("src/leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.active_tab_mut().move_cursor(1); // onto "dst" or "src", sorted alphabetically: dst, src
		app.active_tab_mut().move_cursor(1); // onto "src"
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // src.children = [leaf.txt]
		app.active_tab_mut().move_cursor(1); // onto "src/leaf.txt"
		app.active_tab_mut().selection.insert(root.join("src/leaf.txt"));
		app.yank_selected(false);
		assert_eq!(app.clipboard, vec![root.join("src/leaf.txt")]);
		assert!(
			app.active_tab().selection.is_empty(),
			"yanking converts selected markers into clipboard markers"
		);

		app.active_tab_mut().move_cursor(-2); // back onto "dst"
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // dst.children = [sub]; cursor stays on "dst" itself, now expanded

		app.paste();
		pump(&mut app, &mut rx).await;

		assert!(root.join("dst/leaf.txt").exists());
		assert!(!root.join("dst/sub/leaf.txt").exists());
		assert_eq!(
			app.clipboard,
			vec![root.join("src/leaf.txt")],
			"a copy stays on the clipboard for another paste"
		);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn cut_then_paste_moves_the_file_and_clears_the_marker_state() {
		let root = std::env::temp_dir().join("tuzi-app-test-cut-paste");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("dst/sub")).unwrap();
		fs::write(root.join("source.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.clipboard = vec![root.join("source.txt")];
		app.clipboard_cut = true;
		app.active_tab_mut().move_cursor(1); // dst
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // dst.children = [sub]; cursor stays on "dst" itself, now expanded
		app.paste();
		pump(&mut app, &mut rx).await;

		assert!(!root.join("source.txt").exists());
		assert!(root.join("dst/source.txt").exists());
		assert!(app.clipboard.is_empty());
		assert!(!app.clipboard_cut);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn confirmed_delete_clears_clipboard_marker_before_same_path_is_recreated() {
		let root = std::env::temp_dir().join("tuzi-app-test-delete-permanent");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.clipboard = vec![root.join("leaf.txt")];
		app.active_tab_mut().move_cursor(1); // onto "leaf.txt"
		app.active_tab_mut().delete_selected(DeleteMode::Permanent);
		assert!(app.active_tab().pending_delete.is_some(), "arms the confirmation without deleting yet");
		assert!(root.join("leaf.txt").exists());

		let (targets, mode) = app.active_tab_mut().take_pending_delete(true).unwrap();
		app.enqueue_delete(targets, mode);
		pump(&mut app, &mut rx).await;

		assert!(!root.join("leaf.txt").exists());
		assert!(app.clipboard.is_empty(), "deleting the copied object clears its marker");
		fs::write(root.join("leaf.txt"), b"replacement").unwrap();
		assert!(app.clipboard.is_empty(), "a new object at the same path does not inherit the old marker");
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn quitting_with_no_tasks_running_quits_immediately() {
		let root = std::env::temp_dir().join("tuzi-app-test-quit-clean");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;

		app.request_quit();

		assert!(app.quit);
		assert!(!app.pending_quit);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn quitting_with_a_running_task_asks_first_and_respects_the_answer() {
		let root = std::env::temp_dir().join("tuzi-app-test-quit-guard");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("dst")).unwrap();
		fs::write(root.join("a"), b"a").unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;

		// Enqueuing pushes the Task synchronously; nothing here awaits, so
		// it's still there for request_quit to see regardless of whether
		// the spawned copy itself has run yet.
		app.tasks.enqueue(vec![root.join("a")], root.join("dst"), false, 0);
		app.request_quit();
		assert!(!app.quit, "asks first instead of quitting outright while a task is running");
		assert!(app.pending_quit);

		app.resolve_pending_quit(false);
		assert!(!app.quit, "declining just clears the confirmation");
		assert!(!app.pending_quit);

		app.request_quit();
		app.resolve_pending_quit(true);
		assert!(app.quit, "confirming quits anyway");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_refused_operation_surfaces_as_a_toast_via_dispatch() {
		let root = std::env::temp_dir().join("tuzi-app-test-toast");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		assert!(app.notices.is_empty());

		// Cursor starts on the tab's own tree root, so this is refused
		// outright — Tab queues the refusal, and an ordinary dispatch (not
		// a manual drain) is what's supposed to turn it into a toast.
		app.execute(Command::Delete);

		assert!(app.active_tab().pending_delete.is_none(), "nothing was armed to confirm");
		assert_eq!(app.notices.len(), 1);
		assert_eq!(app.notices[0].message, "The current tree root cannot be deleted");
		assert_eq!(app.notices[0].level, NoticeLevel::Warn);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_directory_load_failure_pins_to_the_node_not_a_toast() {
		use std::os::unix::fs::PermissionsExt;

		let root = std::env::temp_dir().join("tuzi-app-test-load-failure-no-toast");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("locked")).unwrap();
		let root = root.canonicalize().unwrap();
		fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.active_tab_mut().move_cursor(1); // onto "locked"
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // Loaded(locked) -> permission denied

		assert!(
			app.notices.is_empty(),
			"a directory load failure is pinned to its node, not turned into a toast"
		);

		fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn yanking_in_one_tab_pastes_in_another() {
		let root = std::env::temp_dir().join("tuzi-app-test-cross-tab-paste");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("src")).unwrap();
		fs::create_dir_all(root.join("dst/sub")).unwrap();
		fs::write(root.join("src/leaf.txt"), b"hi").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.active_tab_mut().move_cursor(2); // onto "src" (dst, src sorted alphabetically)
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // src.children = [leaf.txt]
		app.active_tab_mut().move_cursor(1); // onto "src/leaf.txt"
		app.yank_selected(false);
		assert_eq!(app.clipboard, vec![root.join("src/leaf.txt")]);

		app.new_tab(); // a second tab, also rooted at `root`
		pump(&mut app, &mut rx).await; // its own initial listing
		assert_eq!(
			app.clipboard,
			vec![root.join("src/leaf.txt")],
			"the clipboard isn't tied to the tab that filled it"
		);

		app.active_tab_mut().move_cursor(1); // onto "dst" in the new tab
		app.active_tab_mut().expand_selected();
		pump(&mut app, &mut rx).await; // dst.children = [sub]; cursor stays on "dst" itself, now expanded
		app.paste();
		pump(&mut app, &mut rx).await;

		assert!(
			root.join("dst/leaf.txt").exists(),
			"pasting in a different tab than the one that yanked should still work"
		);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn new_tab_opens_at_the_same_directory_and_becomes_active() {
		let root = std::env::temp_dir().join("tuzi-app-test-newtab");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.new_tab();

		assert_eq!(app.tabs.len(), 2);
		assert_eq!(app.active, 1, "the new tab's id, not a vec index");
		assert_eq!(app.active_tab().tree.root.path, root);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn closing_the_active_tab_lands_on_a_stable_id_not_a_shifted_index() {
		let root = std::env::temp_dir().join("tuzi-app-test-closetab");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await; // tab 0
		app.new_tab(); // tab 1, active
		app.new_tab(); // tab 2, active
		assert_eq!(app.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), [0, 1, 2]);

		app.close_tab(); // closes tab 2 (active)
		assert_eq!(app.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), [0, 1]);
		assert_eq!(app.active, 1, "lands on the tab that's now in tab 2's old spot");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn close_tab_refuses_to_close_the_last_one() {
		let root = std::env::temp_dir().join("tuzi-app-test-lasttab");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.close_tab();
		assert_eq!(app.tabs.len(), 1, "always at least one tab left");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn relative_tab_switch_wraps_around() {
		let root = std::env::temp_dir().join("tuzi-app-test-cycletab");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await; // 0
		app.new_tab(); // 1
		app.new_tab(); // 2
		app.active = 0;

		app.switch_tab(1);
		assert_eq!(app.active, 1);
		app.switch_tab(1);
		assert_eq!(app.active, 2);
		app.switch_tab(1);
		assert_eq!(app.active, 0, "wraps back to the first");

		app.switch_tab(-1);
		assert_eq!(app.active, 2, "wraps the other way too");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn each_tab_keeps_its_own_column_mode() {
		let root = std::env::temp_dir().join("tuzi-app-test-column-mode");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.execute(Command::SetColumnMode(ColumnMode::Size));
		app.new_tab();
		assert_eq!(app.active_tab().column_mode, ColumnMode::None, "new tabs start with the default mode");

		app.execute(Command::SetColumnMode(ColumnMode::Permissions));
		app.switch_tab(-1);
		assert_eq!(app.active_tab().column_mode, ColumnMode::Size);
		app.switch_tab(1);
		assert_eq!(app.active_tab().column_mode, ColumnMode::Permissions);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn preview_visibility_is_off_by_default_and_local_to_each_tab() {
		let root = std::env::temp_dir().join("tuzi-app-test-preview-visibility");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		assert!(!app.active_tab().preview.visible);
		app.execute(Command::TogglePreview);
		assert!(app.active_tab().preview.visible);

		app.new_tab();
		assert!(!app.active_tab().preview.visible, "new tabs hide preview by default");
		app.switch_tab(-1);
		assert!(app.active_tab().preview.visible, "each tab retains its own preview setting");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn command_prompt_executes_through_the_same_app_entry_point() {
		let root = std::env::temp_dir().join("tuzi-app-test-command-prompt");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;
		let mut router = Router::default();

		assert!(app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE), &mut router));
		assert!(app.active_tab().input.is_some());
		for ch in "preview toggle".chars() {
			app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &mut router);
		}
		app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut router);
		assert!(app.active_tab().preview.visible);
		assert!(app.active_tab().input.is_none());

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn command_prompt_accepts_a_quoted_directory_path() {
		let root = std::env::temp_dir().join("tuzi-app-test-command-path");
		fs::create_dir_all(root.join("child path")).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, _rx) = app(&root).await;
		let mut router = Router::default();

		app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE), &mut router);
		for ch in "cd \"child path\"".chars() {
			app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &mut router);
		}
		app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut router);
		assert_eq!(app.active_tab().tree.root.path, root.join("child path"));

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn command_prompt_completes_like_the_directory_prompt() {
		let root = std::env::temp_dir().join("tuzi-app-test-command-completion");
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (mut app, mut rx) = app(&root).await;
		let mut router = Router::default();

		app.handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE), &mut router);
		for ch in "open --i".chars() {
			app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &mut router);
		}
		pump(&mut app, &mut rx).await;
		assert_eq!(app.active_tab().input.as_ref().unwrap().completion.as_ref().unwrap().candidates, ["open --interactive"]);
		app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut router);
		assert_eq!(app.active_tab().input.as_ref().unwrap().value(), "open --interactive");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn fzf_directory_changes_root_and_nested_file_is_revealed_lazily() {
		let root = std::env::temp_dir().join("tuzi-app-test-fzf");
		let nested = root.join("a/b");
		fs::create_dir_all(&nested).unwrap();
		fs::write(nested.join("target.txt"), b"target").unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.apply_fzf_output(0, &root, false, b"a/b/target.txt\n");
		for _ in 0..4 {
			if app.active_tab().visible()[app.active_tab().cursor].1.path == root.join("a/b/target.txt") {
				break;
			}
			let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
			Dispatcher::dispatch_event(&mut app, event);
		}
		assert_eq!(app.active_tab().visible()[app.active_tab().cursor].1.path, root.join("a/b/target.txt"));

		app.apply_fzf_output(0, &root, false, b"a\n");
		assert_eq!(app.active_tab().tree.root.path, root.join("a"));
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn zoxide_output_changes_the_requesting_tabs_root() {
		let root = std::env::temp_dir().join("tuzi-app-test-zoxide");
		let target = root.join("elsewhere");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&target).unwrap();
		let root = root.canonicalize().unwrap();
		let target = target.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		let mut stdout = target.to_string_lossy().into_owned().into_bytes();
		stdout.push(b'\n');
		app.apply_zoxide_output(0, &stdout);

		assert_eq!(app.active_tab().tree.root.path, target);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn empty_zoxide_output_leaves_the_root_alone() {
		let root = std::env::temp_dir().join("tuzi-app-test-zoxide-cancel");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.apply_zoxide_output(0, b""); // an empty picker result means the user canceled

		assert_eq!(app.active_tab().tree.root.path, root);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn page_and_absolute_movement_use_visible_rows_and_clamp() {
		let root = std::env::temp_dir().join("tuzi-app-test-page-movement");
		fs::create_dir_all(&root).unwrap();
		for index in 0..10 {
			fs::create_dir_all(root.join(format!("dir-{index}"))).unwrap();
		}
		let root = root.canonicalize().unwrap();

		let (mut app, _rx) = app(&root).await;
		app.tree_rows = 4;

		app.move_page(50);
		assert_eq!(app.active_tab().cursor, 2);
		app.move_page(100);
		assert_eq!(app.active_tab().cursor, 6);
		app.move_page(-50);
		assert_eq!(app.active_tab().cursor, 4);
		app.move_page(-100);
		assert_eq!(app.active_tab().cursor, 0, "movement clamps at the first row");

		app.move_page(100);
		app.move_page(100);
		app.move_page(100);
		assert_eq!(app.active_tab().cursor, 10, "movement clamps at the last row");

		app.active_tab_mut().move_to_top();
		assert_eq!(app.active_tab().cursor, 0);
		app.active_tab_mut().move_to_bottom();
		assert_eq!(app.active_tab().cursor, 10);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn background_events_route_to_the_tab_that_requested_them_even_when_inactive() {
		let root = std::env::temp_dir().join("tuzi-app-test-route");
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("b")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await; // tab 0, rooted at `root`
		app.new_tab(); // tab 1, also rooted at `root` (new_tab reuses the active dir)
		assert_eq!(app.active, 1);

		// tab 1's own initial listing is already in flight; drain it before
		// triggering the actual scenario below, so it can't race with (and
		// get received ahead of) the event this test cares about.
		loop {
			let event = rx.recv().await.unwrap();
			let done = matches!(event, Event::Loaded { tab: 1, done: true, .. });
			assert!(matches!(event, Event::Loaded { tab: 1, .. }), "tab 1's own startup load");
			Dispatcher::dispatch_event(&mut app, event);
			if done {
				break;
			}
		}

		// Expand a directory on the *inactive* tab 0 and route the
		// resulting Loaded event straight through Dispatcher, the way the
		// real event loop would — not by calling tab methods directly. Both
		// tabs share the same channel `app` was built with, so this is the
		// exact path a real background load takes.
		let path = root.join("a");
		app.tab_mut(0).unwrap().tree.mark_expanded(&path);
		app.tab_mut(0).unwrap().fs_scheduler.refresh(path);

		let event = loop {
			let event = rx.recv().await.unwrap();
			if matches!(event, Event::Loaded { done: true, .. }) {
				break event;
			}
			Dispatcher::dispatch_event(&mut app, event);
		};
		let tab = match &event {
			Event::Loaded { tab, .. } => *tab,
			_ => panic!("expected Loaded"),
		};
		assert_eq!(tab, 0, "tagged with the tab that asked for it, not whichever tab is active");

		Dispatcher::dispatch_event(&mut app, event);
		assert_eq!(app.active, 1, "dispatching a background event never changes which tab is active");
		assert!(
			app.tab_mut(0).unwrap().tree.is_loaded(&root.join("a")),
			"but it still lands on the tab it was meant for"
		);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_pubsub_subscriber_can_drive_app_state_through_a_yank() {
		let root = std::env::temp_dir().join("tuzi-app-test-pubsub-yank");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("src")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.pubsub.sub("test", "yank", Box::new(|_| vec![Command::ToggleTasks]));

		app.active_tab_mut().selection.insert(root.join("src"));
		assert!(!app.tasks.visible);
		app.yank_selected(false);
		let event = rx.recv().await.unwrap();
		assert!(matches!(event, Event::DdsDeliver(_)));
		Dispatcher::dispatch_event(&mut app, event);

		assert!(app.tasks.visible, "the subscriber's Command actually ran through App::execute");

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_changed_hover_path_publishes_one_hover_event() {
		let root = std::env::temp_dir().join("tuzi-app-test-hover");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("child")).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		app.pubsub.sub("test", "move", Box::new(|_| vec![Command::Cursor(crate::command::CursorTarget::Relative(1))]));
		let mut router = Router::default();
		app.handle_event(
			Event::DdsDeliver(Body::Custom { kind: "move".into(), data: serde_json::Value::Null }),
			&mut router,
		);

		let event = rx.recv().await.unwrap();
		assert!(matches!(event, Event::DdsDeliver(Body::Hover { path: Some(path) }) if path == root.join("child")));
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn the_emit_command_publishes_a_custom_kind_with_its_json_payload() {
		let root = std::env::temp_dir().join("tuzi-app-test-emit");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();

		let (mut app, mut rx) = app(&root).await;
		let seen: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> = Default::default();
		let recorded = seen.clone();
		app.pubsub.sub(
			"test",
			"my-kind",
			Box::new(move |body| {
				if let crate::dds::Body::Custom { data, .. } = body {
					*recorded.lock().unwrap() = Some(data.clone());
				}
				vec![Command::ToggleTasks]
			}),
		);

		app.execute(r#"emit my-kind '{"a":1}'"#.parse().unwrap());
		let event = rx.recv().await.unwrap();
		Dispatcher::dispatch_event(&mut app, event);

		assert_eq!(*seen.lock().unwrap(), Some(serde_json::json!({"a": 1})));
		assert!(app.tasks.visible);

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn controlled_launch_reports_the_app_peer_directly_to_its_parent() {
		let root = std::env::temp_dir().join("tuzi-app-test-dds-ready");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let socket_path = root.join("dds.sock");

		let (parent, mut parent_inbox) = dds::Client::connect(&socket_path, Vec::new()).await.unwrap();
		let (_observer, mut observer_inbox) = dds::Client::connect(&socket_path, vec![dds::WILDCARD_ABILITY.into()]).await.unwrap();
		let (mut app, _rx) = app(&root).await;
		app.dds_client = Some(App::connect_dds_at(app.tx.clone(), &socket_path, app.pubsub.abilities(), None).await.unwrap());
		let app_id = app.dds_client.as_ref().unwrap().id();

		app.announce_attach(&dds::DdsLaunch::new(parent.id(), "launch-token".into()).unwrap());
		let ready = loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), parent_inbox.recv()).await.unwrap().unwrap();
			if matches!(payload.body, Body::Attach { .. }) {
				break payload;
			}
		};
		assert_eq!(ready.sender, app_id);
		assert_eq!(ready.receiver, parent.id());
		assert_eq!(ready.body, Body::Attach { token: "launch-token".into() });

		loop {
			match tokio::time::timeout(std::time::Duration::from_millis(200), observer_inbox.recv()).await {
				Err(_) | Ok(None) => break,
				Ok(Some(payload)) if matches!(payload.body, Body::Sync { .. }) => continue,
				Ok(Some(payload)) => panic!("observer unexpectedly received direct message: {:?}", payload.body),
			}
		}

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn controlled_open_is_sent_only_to_the_parent() {
		let root = std::env::temp_dir().join("tuzi-app-test-parent-open");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("selected.txt"), b"").unwrap();
		let socket_path = root.join("dds.sock");

		let (parent, mut parent_inbox) = dds::Client::connect(&socket_path, Vec::new()).await.unwrap();
		let (_observer, mut observer_inbox) = dds::Client::connect(&socket_path, vec![dds::WILDCARD_ABILITY.into()]).await.unwrap();
		let (mut app, _rx) = app(&root).await;
		Arc::make_mut(&mut app.config).dds.open = DdsOpen::Parent;
		app.dds_client = Some(App::connect_dds_at(app.tx.clone(), &socket_path, app.pubsub.abilities(), None).await.unwrap());
		app.controller = Some(ControllerLink { launch: dds::DdsLaunch::new(parent.id(), "open-test".into()).unwrap(), online: true, abilities: HashSet::new() });

		app.open_selected(false);
		let opened = loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), parent_inbox.recv()).await.unwrap().unwrap();
			if matches!(payload.body, Body::Open { .. }) {
				break payload;
			}
		};
		assert_eq!(opened.receiver, parent.id());
		assert!(matches!(opened.body, Body::Open { paths } if !paths.is_empty()));
		loop {
			match tokio::time::timeout(std::time::Duration::from_millis(200), observer_inbox.recv()).await {
				Err(_) | Ok(None) => break,
				Ok(Some(payload)) if matches!(payload.body, Body::Sync { .. }) => continue,
				Ok(Some(payload)) => panic!("observer unexpectedly received parent open: {:?}", payload.body),
			}
		}

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn emit_parent_is_sent_only_to_the_parent() {
		let root = std::env::temp_dir().join("tuzi-app-test-emit-parent");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let socket_path = root.join("dds.sock");

		let (parent, mut parent_inbox) = dds::Client::connect(&socket_path, Vec::new()).await.unwrap();
		let (_observer, mut observer_inbox) = dds::Client::connect(&socket_path, vec![dds::WILDCARD_ABILITY.into()]).await.unwrap();
		let (mut app, _rx) = app(&root).await;
		app.dds_client = Some(App::connect_dds_at(app.tx.clone(), &socket_path, app.pubsub.abilities(), None).await.unwrap());
		app.controller = Some(ControllerLink { launch: dds::DdsLaunch::new(parent.id(), "emit-test".into()).unwrap(), online: true, abilities: HashSet::new() });

		app.execute(r#"emit --parent tuzi-hide '{"a":1}'"#.parse().unwrap());
		let received = loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), parent_inbox.recv()).await.unwrap().unwrap();
			if matches!(payload.body, Body::Custom { .. }) {
				break payload;
			}
		};
		assert_eq!(received.receiver, parent.id());
		assert_eq!(received.body, Body::Custom { kind: "tuzi-hide".into(), data: serde_json::json!({"a": 1}) });
		loop {
			match tokio::time::timeout(std::time::Duration::from_millis(200), observer_inbox.recv()).await {
				Err(_) | Ok(None) => break,
				Ok(Some(payload)) if matches!(payload.body, Body::Sync { .. }) => continue,
				Ok(Some(payload)) => panic!("observer unexpectedly received parent emit: {:?}", payload.body),
			}
		}

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn emit_parent_never_falls_back_to_broadcast_without_a_parent() {
		let root = std::env::temp_dir().join("tuzi-app-test-emit-parent-unavailable");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let socket_path = root.join("dds.sock");

		let (_observer, mut observer_inbox) = dds::Client::connect(&socket_path, vec![dds::WILDCARD_ABILITY.into()]).await.unwrap();
		let (mut app, _rx) = app(&root).await;
		app.dds_client = Some(App::connect_dds_at(app.tx.clone(), &socket_path, app.pubsub.abilities(), None).await.unwrap());

		app.execute("emit --parent tuzi-hide".parse().unwrap());
		assert_eq!(app.notices.last().map(|notice| notice.message.as_str()), Some("emit --parent requires a controlling parent"));

		app.controller = Some(ControllerLink { launch: dds::DdsLaunch::new(99, "offline".into()).unwrap(), online: false, abilities: HashSet::new() });
		app.execute("emit --parent tuzi-hide".parse().unwrap());
		assert_eq!(app.notices.last().map(|notice| notice.message.as_str()), Some("controller unavailable"));

		loop {
			match tokio::time::timeout(std::time::Duration::from_millis(200), observer_inbox.recv()).await {
				Err(_) | Ok(None) => break,
				Ok(Some(payload)) if matches!(payload.body, Body::Sync { .. }) => continue,
				Ok(Some(payload)) => panic!("observer unexpectedly received {:?}", payload.body),
			}
		}

		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn strict_parent_open_never_falls_back_when_controller_is_unavailable() {
		let root = std::env::temp_dir().join("tuzi-app-test-parent-open-unavailable");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let (mut app, _rx) = app(&root).await;
		Arc::make_mut(&mut app.config).dds.open = DdsOpen::Parent;

		app.open_selected(false);
		assert_eq!(app.active_tab().pending_notice.as_ref().map(|(_, message)| message.as_str()), Some("open requires a controlling parent"));

		app.controller = Some(ControllerLink { launch: dds::DdsLaunch::new(99, "offline".into()).unwrap(), online: false, abilities: HashSet::new() });
		app.open_selected(false);
		assert_eq!(app.active_tab().pending_notice.as_ref().map(|(_, message)| message.as_str()), Some("controller unavailable"));

		fs::remove_dir_all(&root).unwrap();
	}

	#[test]
	fn controlled_tuzi_accepts_tab_updates_only_from_its_parent() {
		let supported = HashSet::from(["update-tab".into()]);
		let state = |sender, data| dds::Payload {
			receiver: 7,
			sender,
			body: Body::Custom { kind: "update-tab".into(), data },
		};
		assert!(App::validate_dds_message(Some(41), 7, &supported, &state(41, serde_json::json!({}))).is_ok());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &state(42, serde_json::json!({}))).is_err());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &state(41, serde_json::json!({ "unknown": true }))).is_err());
		let mut broadcast_update = state(41, serde_json::json!({}));
		broadcast_update.receiver = 0;
		assert!(App::validate_dds_message(Some(41), 7, &supported, &broadcast_update).is_err());
		assert!(App::validate_dds_message(None, 7, &supported, &state(42, serde_json::Value::Null)).is_ok(), "an independent Tuzi keeps its normal subscription behavior");
		let broadcast = dds::Payload { receiver: 0, sender: 42, body: Body::Hover { path: None } };
		assert!(App::validate_dds_message(Some(41), 7, &supported, &broadcast).is_ok(), "ordinary subscribed broadcasts are not parent control messages");
		let switch = dds::Payload { receiver: 7, sender: 41, body: Body::Custom { kind: "switch-tab".into(), data: serde_json::json!({ "tab_id": 2 }) } };
		let supported_switch = HashSet::from(["switch-tab".into()]);
		assert!(App::validate_dds_message(Some(41), 7, &supported_switch, &switch).is_ok());
		assert!(App::validate_dds_message(Some(42), 7, &supported_switch, &switch).is_err());
	}

	#[test]
	fn controlled_tuzi_fully_validates_restore_state_from_its_parent() {
		let root = std::env::temp_dir().join("tuzi-app-test-restore-state-auth");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir(&root).unwrap();
		let supported = HashSet::from(["restore-state".into()]);
		let payload = |sender, data| dds::Payload {
			receiver: 7,
			sender,
			body: Body::Custom { kind: "restore-state".into(), data },
		};
		let valid = serde_json::json!({
			"version": 1, "active_tab": 0,
			"tabs": [{ "cwd": root.clone(), "cursor": null, "selection": [], "expanded": [] }]
		});
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, valid.clone())).is_ok());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(42, valid)).is_err());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, serde_json::json!({
			"version": 1, "active_tab": 0, "tabs": []
		}))).is_err());
		fs::remove_dir_all(root).unwrap();
	}

	#[test]
	fn controlled_tuzi_validates_reveal_sender_and_path() {
		let supported = HashSet::from(["reveal".into()]);
		let payload = |sender, data| dds::Payload {
			receiver: 7,
			sender,
			body: Body::Custom { kind: "reveal".into(), data },
		};
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, serde_json::json!({ "path": "/tmp/file" }))).is_ok());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(42, serde_json::json!({ "path": "/tmp/file" }))).is_err());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, serde_json::json!({ "path": "relative" }))).is_err());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, serde_json::json!({ "path": "/tmp/file", "extra": true }))).is_err());
	}

	#[test]
	fn controlled_tuzi_accepts_get_state_only_from_its_parent_as_a_direct_request() {
		let supported = HashSet::from(["get-state".into()]);
		let payload = |sender, receiver| dds::Payload { sender, receiver, body: Body::GetState { query_id: 42 } };
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, 7)).is_ok());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(42, 7)).is_err());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, 0)).is_err());
	}

	#[tokio::test]
	async fn current_session_snapshot_round_trips_through_restore_validation() {
		let root = std::env::temp_dir().join("tuzi-app-test-current-session-snapshot");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("nested")).unwrap();
		fs::write(root.join("nested/file.txt"), b"").unwrap();
		let root = root.canonicalize().unwrap();
		let target = root.join("nested/file.txt");
		let (mut app, mut rx) = app(&root).await;
		app.active_tab_mut().reveal(target.clone()).unwrap();
		while app.active_tab().visible_at(app.active_tab().cursor).is_none_or(|(_, node)| node.path != target) {
			let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
			Dispatcher::dispatch_event(&mut app, event);
		}
		app.tabs[0].selection.insert(target.clone());
		app.new_tab();
		let state = app.snapshot_state().unwrap();
		assert_eq!(state.active_tab, 1);
		assert_eq!(state.tabs.len(), 2);
		assert_eq!(state.tabs[0].cursor, Some(target.clone()));
		assert_eq!(state.tabs[0].selection, [target]);
		assert_eq!(state.tabs[0].expanded, [root.join("nested")]);
		assert_eq!(crate::session_state::validate_and_normalize(state.clone()).unwrap(), state);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn get_state_request_returns_a_direct_snapshot_to_the_parent() {
		let root = std::env::temp_dir().join("tuzi-app-test-dds-get-state");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let socket_path = root.join("dds.sock");
		let (parent, mut inbox) = dds::Client::connect(&socket_path, Vec::new()).await.unwrap();
		let (mut app, mut rx) = app(&root).await;
		app.controller = Some(ControllerLink { launch: dds::DdsLaunch::new(parent.id(), "query".into()).unwrap(), online: true, abilities: HashSet::new() });
		app.dds_client = Some(App::connect_dds_at(app.tx.clone(), &socket_path, app.pubsub.abilities(), Some(parent.id())).await.unwrap());
		let app_id = app.dds_client.as_ref().unwrap().id();
		loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), inbox.recv()).await.unwrap().unwrap();
			if matches!(payload.body, Body::Sync { ref peers } if peers.iter().any(|peer| peer.id == app_id)) { break }
		}
		parent.publish_to(app_id, Body::GetState { query_id: 42 });
		loop {
			let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
			let requested = matches!(&event, Event::DdsDeliver(Body::GetState { query_id: 42 }));
			Dispatcher::dispatch_event(&mut app, event);
			if requested { break }
		}
		let response = loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), inbox.recv()).await.unwrap().unwrap();
			if matches!(payload.body, Body::State { query_id: 42, .. }) { break payload }
		};
		assert_eq!(response.receiver, parent.id());
		assert_eq!(response.sender, app_id);
		let Body::State { state, .. } = response.body else { unreachable!() };
		assert_eq!(state.tabs[0].cwd, root.canonicalize().unwrap());
		app.new_tab();
		let second_id = app.active;
		parent.publish_to(app_id, Body::GetTabs { query_id: 43 });
		loop {
			let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
			let requested = matches!(&event, Event::DdsDeliver(Body::GetTabs { query_id: 43 }));
			Dispatcher::dispatch_event(&mut app, event);
			if requested { break }
		}
		let response = loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), inbox.recv()).await.unwrap().unwrap();
			if matches!(payload.body, Body::Tabs { query_id: 43, .. }) { break payload }
		};
		assert_eq!(response.receiver, parent.id());
		let Body::Tabs { active_tab_id, tabs, .. } = response.body else { unreachable!() };
		assert_eq!(active_tab_id, second_id);
		assert_eq!(tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(), vec![0, second_id]);
		assert!(tabs.iter().all(|tab| tab.cwd == root.canonicalize().unwrap()));
		let switch = Body::Custom { kind: "switch-tab".into(), data: serde_json::json!({ "tab_id": 0 }) };
		for command in app.pubsub.deliver(&switch) { app.execute(command); }
		assert_eq!(app.active, 0);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn reveal_message_expands_the_active_tab_and_reports_local_failure() {
		let root = std::env::temp_dir().join("tuzi-app-test-dds-reveal");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("nested")).unwrap();
		fs::write(root.join("nested/file.txt"), b"").unwrap();
		let root = root.canonicalize().unwrap();
		let target = root.join("nested/file.txt");
		let (mut app, mut rx) = app(&root).await;
		let body = Body::Custom { kind: "reveal".into(), data: serde_json::json!({ "path": target }) };
		for command in app.pubsub.deliver(&body) { app.execute(command); }
		while app.active_tab().visible_at(app.active_tab().cursor).is_none_or(|(_, node)| node.path != target) {
			let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
			Dispatcher::dispatch_event(&mut app, event);
		}
		assert_eq!(app.active_tab().tree.root.path, root);
		app.execute(Command::Reveal(root.join("missing.txt")));
		assert!(app.notices.iter().any(|notice| notice.message.contains("Cannot reveal path")));
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn restore_state_registry_message_starts_the_atomic_executor() {
		let old = std::env::temp_dir().join("tuzi-app-test-dds-restore-old");
		let replacement = std::env::temp_dir().join("tuzi-app-test-dds-restore-new");
		for path in [&old, &replacement] { let _ = fs::remove_dir_all(path); fs::create_dir(path).unwrap(); }
		let (mut app, mut rx) = app(&old).await;
		let body = Body::Custom {
			kind: "restore-state".into(),
			data: serde_json::json!({
				"version": 1, "active_tab": 0,
				"tabs": [{ "cwd": replacement.clone(), "cursor": null, "selection": [], "expanded": [] }]
			}),
		};
		for command in app.pubsub.deliver(&body) { app.execute(command); }
		assert!(app.staged_session.is_some());
		pump_restore(&mut app, &mut rx).await;
		assert_eq!(app.active_tab().tree.root.path, replacement.canonicalize().unwrap());
		for path in [old, replacement] { fs::remove_dir_all(path).unwrap(); }
	}

	fn tmp_dirs(names: &[&str]) -> Vec<PathBuf> {
		names.iter().map(|name| {
			let path = std::env::temp_dir().join(format!("tuzi-home-{name}-{}", std::process::id()));
			let _ = fs::remove_dir_all(&path);
			fs::create_dir_all(&path).unwrap();
			path.canonicalize().unwrap()
		}).collect()
	}

	fn tab_at(cwd: &Path) -> TabState {
		TabState { cwd: cwd.to_path_buf(), cursor: None, selection: Vec::new(), expanded: Vec::new() }
	}

	#[tokio::test]
	async fn restore_state_home_replaces_the_shared_session_home() {
		let dirs = tmp_dirs(&["restore-old", "restore-new", "restore-home"]);
		let (old, new, home) = (&dirs[0], &dirs[1], &dirs[2]);
		let (mut app, mut rx) = app(old).await;
		assert_eq!(app.home, *old);

		let mut state = session(0, vec![tab_at(new)]);
		state.home = Some(home.clone());
		app.begin_restore(state).unwrap();
		assert_eq!(app.home, *old, "home does not change while the session is only staged");
		pump_restore(&mut app, &mut rx).await;
		assert_eq!(app.home, *home);
		assert_eq!(app.active_tab().tree.root.path, *new, "tabs restore independently of home");

		app.begin_restore(session(0, vec![tab_at(old)])).unwrap();
		pump_restore(&mut app, &mut rx).await;
		assert_eq!(app.home, *home, "a snapshot without home keeps the current one");
		for path in dirs { fs::remove_dir_all(path).unwrap(); }
	}

	#[tokio::test]
	async fn failed_restore_keeps_the_old_home_and_tabs() {
		let dirs = tmp_dirs(&["failed-old", "failed-new", "failed-home"]);
		let (old, new, home) = (&dirs[0], &dirs[1], &dirs[2]);
		let (mut app, mut rx) = app(old).await;

		// A home that is not a directory is refused before anything is staged.
		let mut invalid = session(0, vec![tab_at(new)]);
		invalid.home = Some(new.join("missing"));
		assert!(app.begin_restore(invalid).is_err());
		assert!(app.staged_session.is_none());

		// A listing that fails while staging must not leak the snapshot's home.
		let staged_id = app.next_tab_id;
		let mut state = session(0, vec![tab_at(new)]);
		state.home = Some(home.clone());
		app.begin_restore(state).unwrap();
		Dispatcher::dispatch_event(&mut app, Event::Loaded {
			tab: staged_id, path: new.clone(), ticket: 0, result: Err(io::Error::other("listing failed")), done: true,
		});
		assert!(app.staged_session.is_none());
		assert_eq!(app.home, *old);
		assert_eq!(app.active_tab().tree.root.path, *old);
		let _ = &mut rx;
		for path in dirs { fs::remove_dir_all(path).unwrap(); }
	}

	#[test]
	fn startup_home_prefers_explicit_then_snapshot_then_path() {
		let dirs = tmp_dirs(&["startup-path", "startup-explicit", "startup-snapshot"]);
		let (path, explicit, snapshot) = (&dirs[0], &dirs[1], &dirs[2]);
		let with_home = || {
			let mut state = session(0, vec![tab_at(path)]);
			state.home = Some(snapshot.clone());
			Some(state)
		};

		let mut state = with_home();
		assert_eq!(App::resolve_startup_home(Some(explicit.clone()), &mut state, path).unwrap(), *explicit);
		assert_eq!(state.unwrap().home, None, "the snapshot home is consumed so the restore cannot override --home");

		let mut state = with_home();
		assert_eq!(App::resolve_startup_home(None, &mut state, path).unwrap(), *snapshot);

		let mut state = Some(session(0, vec![tab_at(path)]));
		assert_eq!(App::resolve_startup_home(None, &mut state, path).unwrap(), *path);
		assert_eq!(App::resolve_startup_home(None, &mut None, path).unwrap(), *path);

		assert!(App::resolve_startup_home(Some(path.join("missing")), &mut None, path).is_err());
		fs::write(path.join("file"), "").unwrap();
		assert!(App::resolve_startup_home(Some(path.join("file")), &mut None, path).is_err());
		for path in dirs { fs::remove_dir_all(path).unwrap(); }
	}

	#[tokio::test]
	async fn snapshots_carry_the_home_unless_it_has_vanished() {
		let dirs = tmp_dirs(&["snap-root", "snap-home"]);
		let (root, home) = (&dirs[0], &dirs[1]);
		let (mut app, _rx) = app(root).await;
		app.home = home.clone();
		assert_eq!(app.snapshot_state().unwrap().home.as_deref(), Some(home.as_path()));

		fs::remove_dir(home).unwrap();
		let state = app.snapshot_state().expect("a vanished home must not fail the snapshot");
		assert_eq!(state.home, None);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn set_home_changes_the_shared_home_and_rejects_bad_targets() {
		let dirs = tmp_dirs(&["set-root", "set-target"]);
		let (root, target) = (&dirs[0], &dirs[1]);
		fs::write(root.join("file"), "").unwrap();
		let (mut app, _rx) = app(root).await;
		let tab_count = app.tabs.len();

		app.execute(Command::SetHome(target.clone()));
		assert_eq!(app.home, *target);
		assert_eq!(app.tabs.len(), tab_count);
		assert_eq!(app.active_tab().tree.root.path, *root, "set-home never moves the current tab");

		for bad in [root.join("missing"), root.join("file")] {
			app.execute(Command::SetHome(bad));
			assert_eq!(app.home, *target, "an invalid target keeps the previous home");
			assert!(app.notices.last().is_some_and(|notice| notice.message.starts_with("Cannot set home")));
		}

		app.execute("cd @home".parse().unwrap());
		assert_eq!(app.active_tab().tree.root.path, *target, "g= now goes to the new home");
		for path in dirs { fs::remove_dir_all(path).unwrap(); }
	}

	#[test]
	fn controlled_tuzi_validates_set_home_from_its_parent() {
		let supported = HashSet::from(["set-home".into()]);
		let payload = |sender, receiver, data| dds::Payload {
			receiver,
			sender,
			body: Body::Custom { kind: "set-home".into(), data },
		};
		let valid = serde_json::json!({ "path": "/project" });
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, 7, valid.clone())).is_ok());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(42, 7, valid.clone())).is_err(), "only the parent may set home");
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, 0, valid.clone())).is_err(), "broadcasts are rejected");
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, 7, serde_json::json!({ "path": "relative" }))).is_err());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, 7, serde_json::json!({ "path": "/x", "extra": 1 }))).is_err());
		assert!(App::validate_dds_message(Some(41), 7, &supported, &payload(41, 7, serde_json::json!({}))).is_err());
		assert!(App::validate_dds_message(Some(41), 7, &HashSet::new(), &payload(41, 7, valid)).is_err(), "an unsupported ability is refused");
	}

	async fn loaded_within(rx: &mut mpsc::UnboundedReceiver<Event>, ms: u64) -> bool {
		while let Ok(Some(event)) = tokio::time::timeout(std::time::Duration::from_millis(ms), rx.recv()).await {
			if matches!(event, Event::Loaded { .. }) {
				return true;
			}
		}
		false
	}

	#[tokio::test]
	async fn the_refresh_command_reads_the_open_directories_and_says_so() {
		let root = std::env::temp_dir().join(format!("tuzi-app-test-refresh-cmd-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let (mut app, mut rx) = app(&root).await;
		while loaded_within(&mut rx, 300).await {} // the initial listing

		app.execute("refresh".parse().unwrap());
		assert!(app.notices.last().is_some_and(|notice| notice.message == "Refreshing 1 directory"), "{:?}", app.notices.last().map(|n| n.message.clone()));
		assert!(loaded_within(&mut rx, 1000).await, "the directory must actually be listed again");
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn dropped_events_refresh_the_tab_and_tell_the_user() {
		let root = std::env::temp_dir().join(format!("tuzi-app-test-issue-dropped-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let (mut app, mut rx) = app(&root).await;
		while loaded_within(&mut rx, 300).await {}

		Dispatcher::dispatch_event(&mut app, Event::WatchIssue { tab: 0, issue: crate::watcher::WatchIssue::EventsDropped });
		let notice = app.notices.last().unwrap();
		assert!(notice.message.contains("dropped") && notice.level == NoticeLevel::Info, "{}", notice.message);
		assert!(loaded_within(&mut rx, 1000).await, "the open directories must be read again");
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_watch_that_could_not_be_registered_is_reported_without_refreshing() {
		let root = std::env::temp_dir().join(format!("tuzi-app-test-issue-register-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let (mut app, mut rx) = app(&root).await;
		while loaded_within(&mut rx, 300).await {}

		let issue = crate::watcher::WatchIssue::RegisterFailed { path: root.join("sub"), error: "too many open files".into() };
		Dispatcher::dispatch_event(&mut app, Event::WatchIssue { tab: 0, issue });
		let notice = app.notices.last().unwrap();
		assert!(notice.message.contains("Cannot watch") && notice.message.contains("too many open files") && notice.level == NoticeLevel::Warn, "{}", notice.message);
		assert!(!loaded_within(&mut rx, 400).await, "nothing was lost, and refreshing would only try to register the watch again");
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_repeating_watch_error_is_shown_once_at_a_time() {
		let root = std::env::temp_dir().join(format!("tuzi-app-test-issue-repeat-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let (mut app, _rx) = app(&root).await;
		for _ in 0..5 {
			Dispatcher::dispatch_event(&mut app, Event::WatchIssue { tab: 0, issue: crate::watcher::WatchIssue::BackendError("bad fd".into()) });
		}
		let shown = app.notices.iter().filter(|notice| notice.message == "File watching error: bad fd").count();
		assert_eq!(shown, 1);
		fs::remove_dir_all(&root).unwrap();
	}

	#[test]
	fn app_advertises_its_control_operations() {
		assert_eq!(App::new_registry().abilities(), ["get-state", "get-tabs", "restore-state", "reveal", "set-home", "switch-tab", "update-tab"]);
	}

	#[tokio::test]
	async fn the_app_peer_sends_and_receives_dds_messages_through_the_event_loop() {
		let root = std::env::temp_dir().join("tuzi-app-test-dds-peer");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("selected.txt"), b"").unwrap();
		let root = root.canonicalize().unwrap();
		let socket_path = root.join("dds.sock");

		let (mut app, mut rx) = app(&root).await;
		app.dds_client = Some(App::connect_dds_at(app.tx.clone(), &socket_path, app.pubsub.abilities(), None).await.unwrap());
		let app_peer = app.dds_client.as_ref().unwrap().id();
		let (remote, mut remote_inbox) = dds::Client::connect(&socket_path, vec!["from-app".into(), "yank".into()]).await.unwrap();

		loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), remote_inbox.recv()).await.unwrap().unwrap();
			if let Body::Sync { peers } = payload.body
				&& peers.len() >= 2
			{
				let abilities = &peers.iter().find(|peer| peer.id == app_peer).unwrap().abilities;
				assert_eq!(abilities.iter().map(String::as_str).collect::<HashSet<_>>(), HashSet::from(["get-state", "get-tabs", "restore-state", "reveal", "set-home", "switch-tab", "update-tab"]), "the App advertises its Registry snapshot instead of '*'");
				break;
			}
		}

		app.emit(Body::Custom { kind: "from-app".into(), data: serde_json::Value::Null }, false);
		loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), remote_inbox.recv()).await.unwrap().unwrap();
			if payload.body.kind() == "from-app" {
				break;
			}
		}

		let yank = Body::Yank { paths: vec![root.join("selected.txt")], cut: false };
		app.publish(yank.clone());
		assert!(
			tokio::time::timeout(std::time::Duration::from_millis(200), remote_inbox.recv()).await.is_err(),
			"implicit built-in events stay private by default"
		);
		Arc::make_mut(&mut app.config).dds.broadcast.push("yank".into());
		app.publish(yank);
		loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), remote_inbox.recv()).await.unwrap().unwrap();
			if payload.body.kind() == "yank" {
				break;
			}
		}

		remote.publish(Body::Custom {
			kind: "update-tab".into(),
			data: serde_json::json!({ "selection": ["selected.txt", "missing.txt"] }),
		});
		while !app.active_tab().selection.contains(&root.join("selected.txt")) {
			let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
			Dispatcher::dispatch_event(&mut app, event);
		}

		assert!(app.active_tab().selection.contains(&root.join("selected.txt")), "the remote message entered Event::DdsDeliver and produced UpdateTab");
		assert!(!app.active_tab().selection.contains(&root.join("missing.txt")), "missing paths are ignored");
		drop(remote);
		drop(app);
		let _ = fs::remove_dir_all(&root);
	}

	#[tokio::test]
	async fn controlled_implicit_events_use_parent_or_explicit_broadcast_once() {
		let root = std::env::temp_dir().join("tuzi-app-test-dds-parent-events");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let socket_path = root.join("dds.sock");
		let (parent, mut parent_inbox) = dds::Client::connect(&socket_path, vec!["hover".into()]).await.unwrap();
		let (_observer, mut observer_inbox) = dds::Client::connect(&socket_path, vec![dds::WILDCARD_ABILITY.into()]).await.unwrap();
		let (mut app, mut rx) = app(&root).await;
		app.controller = Some(ControllerLink { launch: dds::DdsLaunch::new(parent.id(), "events".into()).unwrap(), online: true, abilities: HashSet::new() });
		app.dds_client = Some(App::connect_dds_at(app.tx.clone(), &socket_path, app.pubsub.abilities(), Some(parent.id())).await.unwrap());
		while !app.controller.as_ref().unwrap().abilities.contains("hover") {
			let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
			Dispatcher::dispatch_event(&mut app, event);
		}
		app.publish(Body::Cd { path: root.clone() });
		assert!(tokio::time::timeout(std::time::Duration::from_millis(100), async {
			while let Some(payload) = parent_inbox.recv().await {
				if payload.body.kind() == "cd" { return }
			}
		}).await.is_err(), "the parent did not subscribe to cd");

		let hover = Body::Hover { path: Some(root.join("file.txt")) };
		app.publish(hover.clone());
		let directed = loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), parent_inbox.recv()).await.unwrap().unwrap();
			if payload.body.kind() == "hover" { break payload }
		};
		assert_eq!(directed.receiver, parent.id());
		assert_eq!(directed.body, hover);
		assert!(tokio::time::timeout(std::time::Duration::from_millis(100), async {
			while let Some(payload) = observer_inbox.recv().await {
				if payload.body.kind() == "hover" { return }
			}
		}).await.is_err(), "an observer must not see a parent-only event");

		Arc::make_mut(&mut app.config).dds.broadcast.push("hover".into());
		app.publish(hover.clone());
		let public_for_parent = loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), parent_inbox.recv()).await.unwrap().unwrap();
			if payload.body.kind() == "hover" { break payload }
		};
		let public_for_observer = loop {
			let payload = tokio::time::timeout(std::time::Duration::from_secs(2), observer_inbox.recv()).await.unwrap().unwrap();
			if payload.body.kind() == "hover" { break payload }
		};
		assert_eq!(public_for_parent.receiver, 0);
		assert_eq!(public_for_observer.receiver, 0);
		assert_eq!(public_for_parent.body, hover);
		assert!(tokio::time::timeout(std::time::Duration::from_millis(100), parent_inbox.recv()).await.is_err(), "broadcast must not also send a direct copy");

		app.update_controller_peers(&[]);
		assert!(!app.controller.as_ref().unwrap().online);
		assert!(app.controller.as_ref().unwrap().abilities.is_empty());
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn set_state_can_change_directory_and_resolve_relative_selections() {
		let root = std::env::temp_dir().join("tuzi-app-test-update-tab-path");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("next")).unwrap();
		fs::write(root.join("next/keep.txt"), b"").unwrap();
		let root = root.canonicalize().unwrap();
		let next = root.join("next");

		let (mut app, _rx) = app(&root).await;
		Dispatcher::dispatch_event(
			&mut app,
			Event::DdsDeliver(Body::Custom {
				kind: "update-tab".into(),
				data: serde_json::json!({
					"path": next,
					"selection": ["keep.txt", "gone.txt"]
				}),
			}),
		);

		assert_eq!(app.active_tab().tree.root.path, next);
		assert!(app.active_tab().selection.contains(&next.join("keep.txt")));
		assert!(!app.active_tab().selection.contains(&next.join("gone.txt")));
		fs::remove_dir_all(&root).unwrap();
	}
}
