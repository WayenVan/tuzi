use std::{collections::{HashMap, HashSet}, ffi::OsString, io, path::PathBuf, process::ExitCode, time::{Duration, Instant}};

use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tuzi::dds::{self, Body, Payload, PeerInfo, WILDCARD_ABILITY};
use tuzi::session_state::SessionState;

#[derive(Parser)]
#[command(name = "tu", version, about = "Companion command-line tools for Tuzi")]
struct Cli {
	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand)]
enum Command {
	/// Inspect and exchange messages on Tuzi's local event bus.
	Dds(DdsArgs),
}

#[derive(Args)]
struct DdsArgs {
	/// Connect to a specific DDS socket instead of the default runtime socket.
	#[arg(long, global = true, value_name = "PATH")]
	socket: Option<PathBuf>,

	#[command(subcommand)]
	command: DdsCommand,
}

#[derive(Subcommand)]
enum DdsCommand {
	/// Broadcast a custom event to interested peers.
	#[command(
		name = "pub",
		after_long_help = "Examples:\n  tu dds pub greeting '{\"text\":\"hello\"}'\n  tu dds pub refresh\n  tu dds --socket /tmp/test.sock pub greeting '{\"text\":\"hello\"}'"
	)]
	Publish(MessageArgs),

	/// Send a custom event directly to one peer.
	#[command(
		name = "pub-to",
		after_long_help = "Examples:\n  tu dds pub-to 12345 greeting '{\"text\":\"hello\"}'\n  tu dds pub-to 12345 refresh"
	)]
	PublishTo {
		/// Destination peer ID, as shown by `tu dds peers`.
		peer_id: u64,
		#[command(flatten)]
		message: MessageArgs,
	},

	/// Subscribe to event kinds and print messages until interrupted.
	#[command(
		after_long_help = "Examples:\n  tu dds sub\n  tu dds sub hover yank\n  tu dds sub greeting --json"
	)]
	Sub {
		/// Event kinds to receive. With none supplied, receives every kind.
		#[arg(value_name = "KIND")]
		kinds: Vec<String>,
		/// Print the complete wire envelope as newline-delimited JSON.
		#[arg(long)]
		json: bool,
	},

	/// Show currently connected peers and their declared abilities.
	#[command(after_long_help = "Examples:\n  tu dds peers\n  tu dds peers --json")]
	Peers {
		/// Print the peer list as JSON.
		#[arg(long)]
		json: bool,
	},

	/// Launch Tuzi and print the DDS peer ID established by its Attach handshake.
	#[command(
		after_long_help = "Examples:\n  tu dds spawn\n  tu dds spawn -- /project\n  tu dds spawn --timeout 10 -- --config-dir /tmp/tuzi-test /project\n  tu dds spawn --json -- /project"
	)]
	Spawn {
		/// Seconds to wait for Tuzi's Attach handshake.
		#[arg(long, default_value_t = 5, value_name = "SECONDS")]
		timeout: u64,
		/// Print the resulting peer and process IDs as JSON.
		#[arg(long)]
		json: bool,
		/// Tuzi arguments. Precede them with `--`.
		#[arg(last = true, value_name = "TUZI_ARGS", allow_hyphen_values = true)]
		args: Vec<OsString>,
	},

	/// Bridge Neovim or another host to the Tuzi clients it controls.
	#[command(
		after_long_help = "Examples:\n  tu dds controller\n  tu dds controller --abilities hover,cd,renamed\n\nInput (JSON Lines):\n  {\"request_id\":1,\"op\":\"register\",\"token\":\"launch-token\"}\n  {\"request_id\":2,\"op\":\"list\"}\n  {\"request_id\":3,\"op\":\"get-tabs\",\"peer_id\":902}\n  {\"request_id\":4,\"op\":\"switch-tab\",\"peer_id\":902,\"tab_id\":3}\n  {\"request_id\":5,\"op\":\"update-tab\",\"peer_id\":902,\"update\":{\"path\":\"/project\"}}"
	)]
	Controller {
		/// Event kinds to receive from controlled peers, including their implicit state events.
		#[arg(long, value_delimiter = ',', default_value = "cd,yank,renamed,task-done", value_name = "KINDS")]
		abilities: Vec<String>,
	},
}

#[derive(Args)]
struct MessageArgs {
	/// Custom event kind. Built-in protocol kinds are reserved.
	#[arg(value_parser = parse_custom_kind)]
	kind: String,
	/// JSON value carried by the event.
	#[arg(default_value = "null", value_parser = parse_json)]
	data: serde_json::Value,
}

fn parse_custom_kind(value: &str) -> Result<String, String> {
	if value.is_empty() || dds::BUILTIN_KINDS.contains(&value) {
		Err(format!("'{value}' is empty or a reserved built-in kind"))
	} else {
		Ok(value.to_owned())
	}
}

fn parse_json(value: &str) -> Result<serde_json::Value, String> {
	serde_json::from_str(value).map_err(|error| format!("invalid JSON: {error}"))
}

#[tokio::main]
async fn main() -> ExitCode {
	let cli = Cli::parse();
	match run(cli).await {
		Ok(()) => ExitCode::SUCCESS,
		Err(error) => {
			eprintln!("tu: {error}");
			ExitCode::FAILURE
		}
	}
}

async fn run(cli: Cli) -> io::Result<()> {
	let Command::Dds(args) = cli.command;
	let socket = args.socket.unwrap_or_else(dds::socket_path);
	match args.command {
		DdsCommand::Publish(message) => publish(&socket, None, message).await,
		DdsCommand::PublishTo { peer_id, message } => publish(&socket, Some(peer_id), message).await,
		DdsCommand::Sub { kinds, json } => subscribe(&socket, kinds, json).await,
		DdsCommand::Peers { json } => peers(&socket, json).await,
		DdsCommand::Spawn { timeout, json, args } => spawn_tuzi(&socket, Duration::from_secs(timeout), json, args).await,
		DdsCommand::Controller { abilities } => controller(&socket, abilities).await,
	}
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
enum ControllerRequest {
	Register { request_id: u64, token: String },
	CancelRegister { request_id: u64, token: String },
	List { request_id: u64 },
	Detach { request_id: u64, peer_id: u64 },
	UpdateTab { request_id: u64, peer_id: u64, update: TabUpdate },
	GetState { request_id: u64, peer_id: u64 },
	GetTabs { request_id: u64, peer_id: u64 },
	SwitchTab { request_id: u64, peer_id: u64, tab_id: usize },
	Reveal { request_id: u64, peer_id: u64, path: PathBuf },
	RestoreState { request_id: u64, peer_id: u64, state: SessionState },
	Publish {
		request_id: u64,
		peer_id: u64,
		kind:  String,
		#[serde(default)]
		data:  serde_json::Value,
	},
	Ping { request_id: u64 },
}

impl ControllerRequest {
	fn request_id(&self) -> u64 {
		match self {
			Self::Register { request_id, .. }
			| Self::CancelRegister { request_id, .. }
			| Self::List { request_id }
			| Self::Detach { request_id, .. }
			| Self::UpdateTab { request_id, .. }
			| Self::GetState { request_id, .. }
			| Self::GetTabs { request_id, .. }
			| Self::SwitchTab { request_id, .. }
			| Self::Reveal { request_id, .. }
			| Self::RestoreState { request_id, .. }
			| Self::Publish { request_id, .. }
			| Self::Ping { request_id } => *request_id,
		}
	}
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TabUpdate {
	#[serde(default)]
	path:      Option<PathBuf>,
	#[serde(default)]
	selection: Vec<PathBuf>,
}

#[derive(Default)]
struct ControllerState {
	pending:    HashSet<String>,
	controlled: HashMap<String, u64>,
	missing:    HashMap<u64, Instant>,
	peers:      HashMap<u64, HashSet<String>>,
	queries: HashMap<u64, PendingQuery>,
	next_query_id: u64,
}

struct PendingQuery {
	request_id: u64,
	peer_id: u64,
	deadline: Instant,
	kind: QueryKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueryKind { State, Tabs }

impl QueryKind {
	fn name(self) -> &'static str {
		match self { Self::State => "get-state", Self::Tabs => "get-tabs" }
	}
}

const PEER_LEFT_GRACE: Duration = Duration::from_millis(500);
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PENDING_QUERIES: usize = 128;
const CONTROLLER_PROTOCOL_VERSION: u64 = 2;

impl ControllerState {
	fn register(&mut self, token: String) -> Result<(), &'static str> {
		if token.is_empty() || token.len() > dds::MAX_LAUNCH_TOKEN_BYTES {
			return Err("token must contain 1 to 256 bytes");
		}
		if self.pending.contains(&token) || self.controlled.contains_key(&token) {
			return Err("token is already registered");
		}
		self.pending.insert(token);
		Ok(())
	}

	fn accept_attach(&mut self, token: &str, peer_id: u64) -> bool {
		if !self.pending.remove(token) {
			return false;
		}
		self.controlled.insert(token.to_owned(), peer_id);
		self.missing.remove(&peer_id);
		true
	}

	#[cfg(test)]
	fn peer(&self, token: &str) -> Option<u64> {
		self.controlled.get(token).copied()
	}

	fn controls(&self, peer_id: u64) -> bool {
		self.controlled.values().any(|&id| id == peer_id)
	}

	fn cancel_register(&mut self, token: &str) -> bool { self.pending.remove(token) }

	fn detach(&mut self, peer_id: u64) -> Option<String> {
		let token = self.controlled.iter().find_map(|(token, &id)| (id == peer_id).then(|| token.clone()))?;
		self.controlled.remove(&token);
		self.missing.remove(&peer_id);
		Some(token)
	}

	fn clients(&self) -> Vec<(&str, u64, bool)> {
		let mut clients: Vec<_> = self.controlled.iter()
			.map(|(token, &peer_id)| (token.as_str(), peer_id, self.peers.contains_key(&peer_id)))
			.collect();
		clients.sort_unstable_by_key(|client| client.1);
		clients
	}

	fn observe_peers(&mut self, peers: &[dds::PeerInfo], now: Instant) {
		self.peers = peers.iter().map(|peer| (peer.id, peer.abilities.iter().cloned().collect())).collect();
		for &peer_id in self.controlled.values() {
			if self.peers.contains_key(&peer_id) {
				self.missing.remove(&peer_id);
			} else {
				self.missing.entry(peer_id).or_insert(now);
			}
		}
	}

	fn validate_target(&self, peer_id: u64, kind: &str) -> Result<(), &'static str> {
		if !self.controls(peer_id) {
			return Err("peer_id is not controlled");
		}
		let Some(abilities) = self.peers.get(&peer_id) else {
			return Err("peer is offline");
		};
		if !abilities.contains(kind) && !abilities.contains(dds::WILDCARD_ABILITY) {
			return Err("peer does not support this operation");
		}
		Ok(())
	}

	fn begin_query(&mut self, request_id: u64, peer_id: u64, kind: QueryKind, now: Instant) -> Result<u64, &'static str> {
		if self.queries.values().any(|query| query.request_id == request_id) {
			return Err("request_id is already pending");
		}
		if self.queries.len() >= MAX_PENDING_QUERIES {
			return Err("too many pending queries");
		}
		let query_id = self.next_query_id.checked_add(1).ok_or("query ID space exhausted")?;
		self.next_query_id = query_id;
		self.queries.insert(query_id, PendingQuery { request_id, peer_id, deadline: now + QUERY_TIMEOUT, kind });
		Ok(query_id)
	}

	fn complete_query(&mut self, query_id: u64, sender: u64, kind: QueryKind) -> Option<u64> {
		let query = self.queries.get(&query_id)?;
		if query.peer_id != sender || query.kind != kind { return None }
		self.queries.remove(&query_id).map(|query| query.request_id)
	}

	fn take_expired_queries(&mut self, now: Instant) -> Vec<(u64, QueryKind)> {
		let mut expired = Vec::new();
		self.queries.retain(|_, query| {
			if now >= query.deadline {
				expired.push((query.request_id, query.kind));
				false
			} else { true }
		});
		expired.sort_unstable_by_key(|query| query.0);
		expired
	}

	fn take_queries_for_peer(&mut self, peer_id: u64) -> Vec<(u64, QueryKind)> {
		let mut canceled = Vec::new();
		self.queries.retain(|_, query| {
			if query.peer_id == peer_id {
				canceled.push((query.request_id, query.kind));
				false
			} else { true }
		});
		canceled.sort_unstable_by_key(|query| query.0);
		canceled
	}

	fn take_departed(&mut self, now: Instant) -> Vec<(String, u64)> {
		let departed_ids: HashSet<_> = self.missing.iter()
			.filter_map(|(&peer_id, &since)| (now.duration_since(since) >= PEER_LEFT_GRACE).then_some(peer_id))
			.collect();
		if departed_ids.is_empty() {
			return Vec::new();
		}
		self.missing.retain(|peer_id, _| !departed_ids.contains(peer_id));
		let departed: Vec<_> = self.controlled.iter()
			.filter_map(|(token, &peer_id)| departed_ids.contains(&peer_id).then_some((token.clone(), peer_id)))
			.collect();
		self.controlled.retain(|_, peer_id| !departed_ids.contains(peer_id));
		departed
	}
}

async fn controller(socket: &std::path::Path, abilities: Vec<String>) -> io::Result<()> {
	let (client, mut inbox) = dds::Client::connect(socket, abilities).await?;
	let mut input = BufReader::new(tokio::io::stdin()).lines();
	let mut output = tokio::io::stdout();
	let mut state = ControllerState::default();
	let mut lifecycle = tokio::time::interval(Duration::from_millis(50));
	lifecycle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
	write_json_line(&mut output, &serde_json::json!({
		"event": "controller-ready",
		"protocol_version": CONTROLLER_PROTOCOL_VERSION,
		"peer_id": client.id(),
	})).await?;

	loop {
		tokio::select! {
			line = input.next_line() => match line? {
				Some(line) => handle_controller_request(&client, &mut state, &mut output, &line).await?,
				None => return Ok(()),
			},
			payload = inbox.recv() => match payload {
				Some(payload) => handle_controller_payload(&mut state, &mut output, client.id(), payload).await?,
				None => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "DDS controller inbox closed")),
			},
			_ = lifecycle.tick() => {
				for (token, peer_id) in state.take_departed(Instant::now()) {
					for (request_id, kind) in state.take_queries_for_peer(peer_id) {
						write_json_line(&mut output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": format!("peer left before {} completed", kind.name()) })).await?;
					}
					write_json_line(&mut output, &serde_json::json!({ "event": "tuzi-left", "token": token, "peer_id": peer_id })).await?;
				}
				for (request_id, kind) in state.take_expired_queries(Instant::now()) {
					write_json_line(&mut output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": format!("{} timed out", kind.name()) })).await?;
				}
			},
		}
	}
}

async fn handle_controller_request(
	client: &dds::Client,
	state: &mut ControllerState,
	output: &mut tokio::io::Stdout,
	line: &str,
) -> io::Result<()> {
	let request = match serde_json::from_str::<ControllerRequest>(line) {
		Ok(request) => request,
		Err(error) => return write_json_line(output, &serde_json::json!({ "ok": false, "error": format!("invalid request: {error}") })).await,
	};
	if state.queries.values().any(|query| query.request_id == request.request_id()) {
		return write_json_line(output, &serde_json::json!({ "ok": false, "error": "request_id is already pending" })).await;
	}
	match request {
		ControllerRequest::Register { request_id, token } => match state.register(token) {
			Ok(()) => write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true })).await,
			Err(error) => write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await,
		},
		ControllerRequest::CancelRegister { request_id, token } => {
			let removed = state.cancel_register(&token);
			if removed {
				write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true })).await
			} else {
				write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": "token is not pending" })).await
			}
		}
		ControllerRequest::List { request_id } => {
			let clients: Vec<_> = state.clients().into_iter().map(|(token, peer_id, online)| serde_json::json!({ "token": token, "peer_id": peer_id, "online": online })).collect();
			write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true, "clients": clients })).await
		}
		ControllerRequest::Detach { request_id, peer_id } => {
			let removed = state.detach(peer_id).is_some();
			if removed {
				write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true })).await?;
				for (pending_id, kind) in state.take_queries_for_peer(peer_id) {
					write_json_line(output, &serde_json::json!({ "request_id": pending_id, "ok": false, "error": format!("peer detached before {} completed", kind.name()) })).await?;
				}
				Ok(())
			} else {
				write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": "peer_id is not controlled" })).await
			}
		}
		ControllerRequest::UpdateTab { request_id, peer_id, update } => {
			if let Err(error) = state.validate_target(peer_id, "update-tab") {
				return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await;
			}
			client.publish_to(peer_id, Body::Custom {
				kind: "update-tab".into(),
				data: serde_json::json!({ "path": update.path, "selection": update.selection }),
			});
			write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true, "status": "queued" })).await
		}
		ControllerRequest::GetState { request_id, peer_id } => {
			if let Err(error) = state.validate_target(peer_id, "get-state") {
				return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await;
			}
			let query_id = match state.begin_query(request_id, peer_id, QueryKind::State, Instant::now()) {
				Ok(query_id) => query_id,
				Err(error) => return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await,
			};
			client.publish_to(peer_id, Body::GetState { query_id });
			Ok(())
		}
		ControllerRequest::GetTabs { request_id, peer_id } => {
			if let Err(error) = state.validate_target(peer_id, "get-tabs") {
				return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await;
			}
			let query_id = match state.begin_query(request_id, peer_id, QueryKind::Tabs, Instant::now()) {
				Ok(query_id) => query_id,
				Err(error) => return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await,
			};
			client.publish_to(peer_id, Body::GetTabs { query_id });
			Ok(())
		}
		ControllerRequest::SwitchTab { request_id, peer_id, tab_id } => {
			if let Err(error) = state.validate_target(peer_id, "switch-tab") {
				return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await;
			}
			client.publish_to(peer_id, Body::Custom { kind: "switch-tab".into(), data: serde_json::json!({ "tab_id": tab_id }) });
			write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true, "status": "queued" })).await
		}
		ControllerRequest::Reveal { request_id, peer_id, path } => {
			if let Err(error) = state.validate_target(peer_id, "reveal") {
				return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await;
			}
			if !path.is_absolute() {
				return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": "path must be absolute" })).await;
			}
			client.publish_to(peer_id, Body::Custom { kind: "reveal".into(), data: serde_json::json!({ "path": path }) });
			write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true, "status": "queued" })).await
		}
		ControllerRequest::RestoreState { request_id, peer_id, state: snapshot } => {
			if let Err(error) = state.validate_target(peer_id, "restore-state") {
				return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await;
			}
			client.publish_to(peer_id, Body::Custom {
				kind: "restore-state".into(),
				data: serde_json::to_value(snapshot).expect("SessionState must serialize"),
			});
			write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true, "status": "queued" })).await
		}
		ControllerRequest::Publish { request_id, peer_id, kind, data } => {
			if kind.is_empty() || matches!(kind.as_str(), "update-tab" | "switch-tab" | "restore-state" | "reveal") || dds::BUILTIN_KINDS.contains(&kind.as_str()) {
				return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": "kind is empty or reserved" })).await;
			}
			if let Err(error) = state.validate_target(peer_id, &kind) {
				return write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await;
			}
			client.publish_to(peer_id, Body::Custom { kind, data });
			write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true, "status": "queued" })).await
		}
		ControllerRequest::Ping { request_id } => write_json_line(output, &serde_json::json!({
			"request_id": request_id,
			"ok": true,
			"protocol_version": CONTROLLER_PROTOCOL_VERSION,
			"peer_id": client.id(),
		})).await,
	}
}

async fn handle_controller_payload(
	state: &mut ControllerState,
	output: &mut tokio::io::Stdout,
	controller_id: u64,
	payload: Payload,
) -> io::Result<()> {
	if let Body::Sync { peers } = &payload.body {
		state.observe_peers(peers, Instant::now());
		return Ok(());
	}
	if payload.receiver == controller_id
		&& let Body::Attach { token } = &payload.body
	{
		if state.accept_attach(token, payload.sender) {
			return write_json_line(output, &serde_json::json!({ "event": "tuzi-ready", "token": token, "peer_id": payload.sender })).await;
		}
		return Ok(());
	}
	match &payload.body {
		Body::SessionEnd { state: snapshot } => {
			if payload.receiver == controller_id && state.controls(payload.sender) {
				write_json_line(output, &serde_json::json!({ "event": "tuzi-exit", "peer_id": payload.sender, "state": snapshot })).await?;
			}
			return Ok(());
		}
		Body::State { query_id, state: snapshot } => {
			if payload.receiver == controller_id
				&& let Some(request_id) = state.complete_query(*query_id, payload.sender, QueryKind::State)
			{
					write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true, "state": snapshot })).await?;
			}
			return Ok(());
		}
		Body::StateError { query_id, error } => {
			if payload.receiver == controller_id
				&& let Some(request_id) = state.complete_query(*query_id, payload.sender, QueryKind::State)
			{
					write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": false, "error": error })).await?;
			}
			return Ok(());
		}
		Body::Tabs { query_id, active_tab_id, tabs } => {
			if payload.receiver == controller_id
				&& let Some(request_id) = state.complete_query(*query_id, payload.sender, QueryKind::Tabs)
			{
				write_json_line(output, &serde_json::json!({ "request_id": request_id, "ok": true, "active_tab_id": active_tab_id, "tabs": tabs })).await?;
			}
			return Ok(());
		}
		_ => {}
	}
	if state.controls(payload.sender) && !matches!(payload.body, Body::Sync { .. } | Body::Join { .. }) {
		write_json_line(output, &serde_json::json!({
			"event": "message",
			"peer_id": payload.sender,
			"kind": payload.body.kind(),
			"body": payload.body,
		})).await?;
	}
	Ok(())
}

async fn write_json_line(output: &mut tokio::io::Stdout, value: &serde_json::Value) -> io::Result<()> {
	let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
	line.push(b'\n');
	output.write_all(&line).await?;
	output.flush().await
}

async fn spawn_tuzi(socket: &std::path::Path, timeout: Duration, json: bool, args: Vec<OsString>) -> io::Result<()> {
	if socket != dds::socket_path() {
		return Err(io::Error::new(io::ErrorKind::InvalidInput, "`tu dds spawn` currently requires the default DDS socket"));
	}
	if timeout.is_zero() {
		return Err(io::Error::new(io::ErrorKind::InvalidInput, "spawn timeout must be greater than zero"));
	}

	let (controller, mut inbox) = dds::Client::connect(socket, Vec::new()).await?;
	let token = launch_token()?;
	let executable = tuzi_executable()?;
	let mut child = tokio::process::Command::new(executable)
		.args(args)
		.env("TUZI_DDS_PARENT", controller.id().to_string())
		.env("TUZI_DDS_TOKEN", &token)
		.spawn()?;
	let pid = child.id();

	let wait_ready = async {
		loop {
			let payload = inbox.recv().await.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "DDS connection closed before Tuzi attached"))?;
			if payload.receiver == controller.id()
				&& let Body::Attach { token: received } = payload.body
				&& received == token
			{
				return Ok::<u64, io::Error>(payload.sender);
			}
		}
	};

	let peer_id = tokio::select! {
		result = tokio::time::timeout(timeout, wait_ready) => match result {
			Ok(result) => result?,
			Err(_) => {
				let _ = child.start_kill();
				let _ = child.wait().await;
				return Err(io::Error::new(io::ErrorKind::TimedOut, format!("Tuzi did not complete its DDS handshake within {} seconds", timeout.as_secs())));
			}
		},
		status = child.wait() => {
			let status = status?;
			return Err(io::Error::other(format!("Tuzi exited before its DDS handshake completed ({status})")));
		}
	};

	// Keep the wrapper (and therefore its foreground process group) alive
	// while the interactive child owns the terminal. If `tu` returned here,
	// the shell would reclaim the terminal from the still-running Tuzi and
	// raw/alternate-screen state could be left corrupted. Printing is also
	// deferred until Tuzi restores the terminal on exit.
	let status = child.wait().await?;
	if json {
		println!("{}", serde_json::json!({ "peer_id": peer_id, "pid": pid }));
	} else {
		println!("{peer_id}");
	}
	if status.success() {
		Ok(())
	} else {
		Err(io::Error::other(format!("Tuzi exited with {status}")))
	}
}

fn launch_token() -> io::Result<String> {
	let mut bytes = [0u8; 16];
	getrandom::fill(&mut bytes).map_err(|error| io::Error::other(format!("failed to generate launch token: {error}")))?;
	Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn tuzi_executable() -> io::Result<OsString> {
	let current = std::env::current_exe()?;
	let sibling = current.with_file_name(if cfg!(windows) { "tuzi.exe" } else { "tuzi" });
	Ok(if sibling.is_file() { sibling.into_os_string() } else { OsString::from("tuzi") })
}

async fn publish(socket: &std::path::Path, receiver: Option<u64>, message: MessageArgs) -> io::Result<()> {
	let (client, _inbox) = dds::Client::connect(socket, Vec::new()).await?;
	let body = Body::Custom { kind: message.kind, data: message.data };
	if let Some(receiver) = receiver {
		client.publish_to(receiver, body);
	} else {
		client.publish(body);
	}
	client.flush().await;
	Ok(())
}

async fn subscribe(socket: &std::path::Path, mut kinds: Vec<String>, json: bool) -> io::Result<()> {
	if kinds.is_empty() {
		kinds.push(WILDCARD_ABILITY.to_owned());
	}
	let (_client, mut inbox) = dds::Client::connect(socket, kinds).await?;
	while let Some(payload) = inbox.recv().await {
		print_payload(&payload, json)?;
	}
	Ok(())
}

async fn peers(socket: &std::path::Path, json: bool) -> io::Result<()> {
	let (_client, mut inbox) = dds::Client::connect(socket, Vec::new()).await?;
	while let Some(payload) = inbox.recv().await {
		if let Body::Sync { peers } = payload.body {
			print_peers(&peers, json)?;
			return Ok(());
		}
	}
	Err(io::Error::new(io::ErrorKind::UnexpectedEof, "DDS connection closed before peer discovery"))
}

fn print_payload(payload: &Payload, json: bool) -> io::Result<()> {
	if json {
		println!("{}", serde_json::to_string(payload).map_err(io::Error::other)?);
	} else {
		println!("{} -> {}  {}  {}", payload.sender, payload.receiver, payload.body.kind(), serde_json::to_string(&payload.body).map_err(io::Error::other)?);
	}
	Ok(())
}

fn print_peers(peers: &[PeerInfo], json: bool) -> io::Result<()> {
	if json {
		println!("{}", serde_json::to_string(peers).map_err(io::Error::other)?);
	} else {
		println!("PEER ID\tABILITIES");
		for peer in peers {
			let abilities = if peer.abilities.is_empty() { "-".to_owned() } else { peer.abilities.join(",") };
			println!("{}\t{}", peer.id, abilities);
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn rejects_reserved_custom_kinds() {
		assert!(parse_custom_kind("hover").is_err());
		assert_eq!(parse_custom_kind("greeting").unwrap(), "greeting");
	}

	#[test]
	fn command_tree_parses_all_dds_operations() {
		assert!(Cli::try_parse_from(["tu", "dds", "pub", "hello"]).is_ok());
		assert!(Cli::try_parse_from(["tu", "dds", "pub-to", "42", "hello", "{}"]).is_ok());
		assert!(Cli::try_parse_from(["tu", "dds", "sub", "hover", "yank", "--json"]).is_ok());
		assert!(Cli::try_parse_from(["tu", "dds", "peers"]).is_ok());
		assert!(Cli::try_parse_from(["tu", "dds", "spawn", "--", "--no-config", "/tmp"]).is_ok());
		assert!(Cli::try_parse_from(["tu", "dds", "controller"]).is_ok());
	}

	#[test]
	fn launch_tokens_are_random_128_bit_hex_values() {
		let a = launch_token().unwrap();
		let b = launch_token().unwrap();
		assert_eq!(a.len(), 32);
		assert!(a.bytes().all(|byte| byte.is_ascii_hexdigit()));
		assert_ne!(a, b);
	}

	#[test]
	fn controller_only_accepts_registered_ready_tokens() {
		let mut state = ControllerState::default();
		assert!(!state.accept_attach("unknown", 10));
		state.register("known".into()).unwrap();
		assert!(state.accept_attach("known", 11));
		assert_eq!(state.peer("known"), Some(11));
		assert!(state.controls(11));
		assert!(!state.controls(10));
		assert!(!state.accept_attach("known", 12));
	}

	#[test]
	fn controller_removes_a_peer_only_after_the_grace_period() {
		let start = Instant::now();
		let mut state = ControllerState::default();
		state.register("known".into()).unwrap();
		assert!(state.accept_attach("known", 11));

		state.observe_peers(&[], start);
		assert!(state.take_departed(start + PEER_LEFT_GRACE - Duration::from_millis(1)).is_empty());
		state.observe_peers(&[dds::PeerInfo { id: 11, abilities: vec!["update-tab".into()] }], start + Duration::from_millis(250));
		assert!(state.take_departed(start + PEER_LEFT_GRACE).is_empty(), "reappearing during failover cancels departure");

		state.observe_peers(&[], start + Duration::from_secs(1));
		assert_eq!(state.take_departed(start + Duration::from_secs(1) + PEER_LEFT_GRACE), vec![("known".into(), 11)]);
		assert_eq!(state.peer("known"), None);
		assert!(!state.controls(11));
		assert!(state.register("known".into()).is_ok(), "a departed token can be registered again");
	}

	#[test]
	fn controller_protocol_uses_request_id_and_op() {
		assert_eq!(CONTROLLER_PROTOCOL_VERSION, 2);
		assert!(matches!(
			serde_json::from_str::<ControllerRequest>(r#"{"request_id":7,"op":"register","token":"known"}"#).unwrap(),
			ControllerRequest::Register { request_id: 7, token } if token == "known"
		));
		assert!(serde_json::from_str::<ControllerRequest>(r#"{"id":7,"command":"register","token":"known"}"#).is_err());
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":8,"op":"list"}"#).unwrap(), ControllerRequest::List { request_id: 8 }));
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":9,"op":"ping"}"#).unwrap(), ControllerRequest::Ping { request_id: 9 }));
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":10,"op":"cancel-register","token":"known"}"#).unwrap(), ControllerRequest::CancelRegister { request_id: 10, .. }));
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":11,"op":"detach","peer_id":42}"#).unwrap(), ControllerRequest::Detach { request_id: 11, peer_id: 42 }));
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":12,"op":"update-tab","peer_id":42,"update":{"path":"/tmp","selection":[]}}"#).unwrap(), ControllerRequest::UpdateTab { request_id: 12, peer_id: 42, .. }));
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":16,"op":"get-state","peer_id":42}"#).unwrap(), ControllerRequest::GetState { request_id: 16, peer_id: 42 }));
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":17,"op":"get-tabs","peer_id":42}"#).unwrap(), ControllerRequest::GetTabs { request_id: 17, peer_id: 42 }));
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":18,"op":"switch-tab","peer_id":42,"tab_id":3}"#).unwrap(), ControllerRequest::SwitchTab { request_id: 18, peer_id: 42, tab_id: 3 }));
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":15,"op":"reveal","peer_id":42,"path":"/tmp/file"}"#).unwrap(), ControllerRequest::Reveal { request_id: 15, peer_id: 42, path } if path == PathBuf::from("/tmp/file")));
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":14,"op":"restore-state","peer_id":42,"state":{"version":1,"active_tab":0,"tabs":[{"cwd":"/tmp","cursor":null,"selection":[],"expanded":[]}]}}"#).unwrap(), ControllerRequest::RestoreState { request_id: 14, peer_id: 42, .. }));
		assert!(matches!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":13,"op":"publish","peer_id":42,"kind":"event","data":null}"#).unwrap(), ControllerRequest::Publish { request_id: 13, peer_id: 42, .. }));
		assert!(serde_json::from_str::<ControllerRequest>(r#"{"request_id":10,"op":"publish","token":"known","kind":"event"}"#).is_err());
	}

	#[test]
	fn controller_lists_and_detaches_clients_by_peer_id() {
		let mut state = ControllerState::default();
		state.register("second".into()).unwrap();
		state.register("first".into()).unwrap();
		assert!(state.accept_attach("second", 22));
		assert!(state.accept_attach("first", 11));
		state.observe_peers(&[
			dds::PeerInfo { id: 22, abilities: vec!["update-tab".into()] },
			dds::PeerInfo { id: 11, abilities: vec!["update-tab".into()] },
		], Instant::now());
		assert_eq!(state.clients(), vec![("first", 11, true), ("second", 22, true)]);
		assert_eq!(state.detach(11).as_deref(), Some("first"));
		assert!(!state.controls(11));
		assert!(state.controls(22));
	}

	#[test]
	fn controller_cancels_only_pending_tokens() {
		let mut state = ControllerState::default();
		state.register("pending".into()).unwrap();
		assert!(state.cancel_register("pending"));
		assert!(!state.cancel_register("pending"));
	}

	#[test]
	fn controller_validates_control_ownership_online_state_and_ability() {
		let mut state = ControllerState::default();
		state.register("known".into()).unwrap();
		assert!(state.accept_attach("known", 11));
		assert_eq!(state.validate_target(12, "update-tab"), Err("peer_id is not controlled"));
		assert_eq!(state.validate_target(11, "update-tab"), Err("peer is offline"));
		state.observe_peers(&[dds::PeerInfo { id: 11, abilities: vec!["hover".into()] }], Instant::now());
		assert_eq!(state.validate_target(11, "update-tab"), Err("peer does not support this operation"));
		state.observe_peers(&[dds::PeerInfo { id: 11, abilities: vec!["update-tab".into()] }], Instant::now());
		assert_eq!(state.validate_target(11, "update-tab"), Ok(()));
		assert_eq!(state.validate_target(11, "restore-state"), Err("peer does not support this operation"));
		state.observe_peers(&[dds::PeerInfo { id: 11, abilities: vec!["restore-state".into()] }], Instant::now());
		assert_eq!(state.validate_target(11, "restore-state"), Ok(()));
		assert_eq!(state.validate_target(11, "reveal"), Err("peer does not support this operation"));
		state.observe_peers(&[dds::PeerInfo { id: 11, abilities: vec!["reveal".into()] }], Instant::now());
		assert_eq!(state.validate_target(11, "reveal"), Ok(()));
		assert_eq!(state.validate_target(11, "get-state"), Err("peer does not support this operation"));
		state.observe_peers(&[dds::PeerInfo { id: 11, abilities: vec!["get-state".into()] }], Instant::now());
		assert_eq!(state.validate_target(11, "get-state"), Ok(()));
	}

	#[test]
	fn queries_match_the_right_peer_and_expire_without_replaying() {
		let now = Instant::now();
		let mut state = ControllerState::default();
		let first = state.begin_query(7, 11, QueryKind::State, now).unwrap();
		assert_eq!(state.begin_query(7, 12, QueryKind::Tabs, now), Err("request_id is already pending"));
		assert_eq!(state.complete_query(first, 12, QueryKind::State), None);
		assert_eq!(state.complete_query(first, 11, QueryKind::Tabs), None);
		assert_eq!(state.complete_query(first, 11, QueryKind::State), Some(7));
		assert_eq!(state.complete_query(first, 11, QueryKind::State), None);
		let second = state.begin_query(7, 11, QueryKind::Tabs, now).unwrap();
		assert_ne!(first, second, "a late reply must not complete a reused request_id");
		assert!(state.take_expired_queries(now + QUERY_TIMEOUT - Duration::from_millis(1)).is_empty());
		assert_eq!(state.take_expired_queries(now + QUERY_TIMEOUT), vec![(7, QueryKind::Tabs)]);
		assert_eq!(state.complete_query(second, 11, QueryKind::Tabs), None);
		state.begin_query(8, 11, QueryKind::State, now).unwrap();
		state.begin_query(9, 12, QueryKind::Tabs, now).unwrap();
		assert_eq!(state.take_queries_for_peer(11), vec![(8, QueryKind::State)]);
		assert_eq!(state.take_queries_for_peer(12), vec![(9, QueryKind::Tabs)]);
	}

	#[tokio::test]
	async fn controller_identifies_a_registered_child_and_receives_its_messages() {
		let socket = std::env::temp_dir().join(format!("tuzi-controller-test-{}/dds.sock", std::process::id()));
		let _ = std::fs::remove_file(&socket);
		let (controller, mut inbox) = dds::Client::connect(&socket, vec!["hover".into()]).await.unwrap();
		let (child, mut child_inbox) = dds::Client::connect(&socket, Vec::new()).await.unwrap();
		loop {
			let payload = tokio::time::timeout(Duration::from_secs(2), child_inbox.recv()).await.unwrap().unwrap();
			if matches!(payload.body, Body::Sync { ref peers } if peers.len() >= 2) {
				break;
			}
		}

		let mut state = ControllerState::default();
		state.register("launch".into()).unwrap();
		child.publish_to(controller.id(), Body::Attach { token: "launch".into() });
		let ready = loop {
			let payload = tokio::time::timeout(Duration::from_secs(2), inbox.recv()).await.unwrap().unwrap();
			if matches!(payload.body, Body::Attach { .. }) {
				break payload;
			}
		};
		let Body::Attach { token } = ready.body else { unreachable!() };
		assert!(state.accept_attach(&token, ready.sender));
		assert!(state.controls(child.id()));

		child.publish(Body::Hover { path: Some("/tmp/file".into()) });
		let hover = loop {
			let payload = tokio::time::timeout(Duration::from_secs(2), inbox.recv()).await.unwrap().unwrap();
			if matches!(payload.body, Body::Hover { .. }) {
				break payload;
			}
		};
		assert_eq!(hover.sender, child.id());
		assert!(state.controls(hover.sender));

		let _ = std::fs::remove_file(&socket);
	}
}
