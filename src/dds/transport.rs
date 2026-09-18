//! The Unix-socket half of DDS (`.ai/dds-plan.md` P3/P4a): whichever process
//! tries to connect first and finds nobody listening becomes the `Server`
//! for every later `Client`, including itself. Clients reconnect and repeat
//! that election if the server owner exits. Kept local-only (no
//! `Send`-across-network transport) — every peer lives on the same
//! machine.

use std::{
	collections::{HashMap, HashSet},
	io,
	os::unix::fs::{MetadataExt, PermissionsExt},
	path::Path,
	sync::Arc,
	time::Duration,
};

use tokio::{
	io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
	net::{UnixListener, UnixStream},
	select,
	sync::{Mutex, mpsc},
	task::{JoinHandle, JoinSet},
};

use super::{Body, Payload, PeerId, PeerInfo, new_peer_id};

/// A long-lived DDS client: send `publish`/`publish_to` to its supervisor,
/// which maintains the socket connection, and drain the paired receiver for
/// payloads other peers sent us.
pub struct Client {
	id:         PeerId,
	outbox:     mpsc::UnboundedSender<Payload>,
	supervisor: JoinHandle<()>,
}

/// A peer that wants every kind forwarded to it, regardless of ability —
/// what `tuzi sub` announces to act as a debug tap on the whole bus.
pub const WILDCARD_ABILITY: &str = "*";

impl Client {
	/// Connects to `socket_path`, bootstrapping a `Server` there first if
	/// nothing answers. `abilities` are the kinds this client wants
	/// forwarded from other peers — announced immediately via `Hi`.
	pub async fn connect(socket_path: &Path, abilities: Vec<String>) -> io::Result<(Self, mpsc::UnboundedReceiver<Payload>)> {
		let id = new_peer_id();
		let connection = connect_or_bootstrap(socket_path).await?;
		let (outbox_tx, outbox_rx) = mpsc::unbounded_channel::<Payload>();
		let (inbox_tx, inbox_rx) = mpsc::unbounded_channel::<Payload>();
		let path = socket_path.to_path_buf();
		let supervisor = tokio::spawn(supervise(id, abilities, path, connection, outbox_rx, inbox_tx));
		let client = Self { id, outbox: outbox_tx, supervisor };
		Ok((client, inbox_rx))
	}

	/// This connection's routing identity. Controllers use their own ID as
	/// the parent address; a Ready payload's sender identifies the child.
	pub fn id(&self) -> PeerId {
		self.id
	}

	/// Broadcasts to every peer that declared interest in `body`'s kind.
	pub fn publish(&self, body: Body) {
		let _ = self.outbox.send(Payload::broadcast(self.id, body));
	}

	pub fn publish_to(&self, receiver: PeerId, body: Body) {
		let _ = self.outbox.send(Payload { receiver, sender: self.id, body });
	}

	/// Closes the outbound channel and waits for every already-queued
	/// payload to actually hit the socket. A short-lived CLI invocation
	/// (`tuzi emit`) needs this — otherwise the process can exit before
	/// its background write task gets a chance to run.
	pub async fn flush(self) {
		drop(self.outbox);
		let _ = self.supervisor.await;
	}
}

struct Connection {
	stream: UnixStream,
	/// Present only when this client won the election and hosts the server.
	/// Keeping the handle here ties that server's lifetime to its owner.
	server: Option<JoinHandle<()>>,
}

async fn supervise(
	id: PeerId,
	abilities: Vec<String>,
	socket_path: std::path::PathBuf,
	mut connection: Connection,
	mut outbox: mpsc::UnboundedReceiver<Payload>,
	inbox: mpsc::UnboundedSender<Payload>,
) {
	let mut pending_retry = None;
	loop {
		let (read_half, mut write_half) = connection.stream.into_split();
		let mut lines = BufReader::new(read_half).lines();
		if write_payload(&mut write_half, &Payload::broadcast(id, Body::Hi { abilities: abilities.clone() })).await.is_err() {
			connection = reconnect(&socket_path, connection.server.take()).await;
			continue;
		}
		if let Some(payload) = pending_retry.take()
			&& write_payload(&mut write_half, &payload).await.is_err()
		{
			// This was the one permitted retry. Drop the payload and repair the
			// connection without replaying it again.
			connection = reconnect(&socket_path, connection.server.take()).await;
			continue;
		}

		pending_retry = loop {
			select! {
				message = outbox.recv() => {
					let Some(payload) = message else {
						if let Some(server) = connection.server.take() { server.abort(); }
						return;
					};
					if write_payload(&mut write_half, &payload).await.is_err() {
						break Some(payload);
					}
				}
				line = lines.next_line() => match line {
					Ok(Some(line)) => {
						if let Ok(payload) = serde_json::from_str::<Payload>(&line)
							&& payload.sender != id
						{
							let _ = inbox.send(payload);
						}
					}
					Ok(None) | Err(_) => break None,
				}
			}
		};

		connection = reconnect(&socket_path, connection.server.take()).await;
	}
}

async fn reconnect(socket_path: &Path, mut owned_server: Option<JoinHandle<()>>) -> Connection {
	// A client that owns a healthy server should reconnect to it without
	// throwing away ownership. If that fails, stop the suspect server and
	// join the same election as every other peer.
	if let Some(server) = owned_server.take() {
		if !server.is_finished()
			&& let Ok(stream) = UnixStream::connect(socket_path).await
		{
			return Connection { stream, server: Some(server) };
		}
		server.abort();
	}
	let mut failures = 0;
	loop {
		match connect_or_bootstrap(socket_path).await {
			Ok(connection) => return connection,
			Err(_) => {
				tokio::time::sleep(reconnect_backoff(failures)).await;
				failures += 1;
			}
		}
	}
}

/// The first reconnect attempt happens immediately. These delays are only
/// paid after consecutive failures, then cap at 500ms to avoid a busy loop
/// during a longer outage.
const RECONNECT_BACKOFF_MS: &[u64] = &[20, 50, 100, 250, 500];

fn reconnect_backoff(failures: usize) -> Duration {
	Duration::from_millis(RECONNECT_BACKOFF_MS[failures.min(RECONNECT_BACKOFF_MS.len() - 1)])
}

async fn write_payload(write_half: &mut tokio::net::unix::OwnedWriteHalf, payload: &Payload) -> io::Result<()> {
	let mut line = serde_json::to_string(payload).map_err(io::Error::other)?;
	line.push('\n');
	write_half.write_all(line.as_bytes()).await
}

struct Peer {
	abilities: HashSet<String>,
	outbox:    mpsc::UnboundedSender<String>,
}

type PeerTable = Arc<Mutex<HashMap<PeerId, Peer>>>;

/// Connects to `socket_path`, self-electing as the server there if nobody
/// answers. Concurrent bootstrappers are expected: several processes can
/// all find nobody listening at once and race to bind. Only one `bind`
/// call can win that race — everyone else must fall back to connecting to
/// the winner instead of erroring out, and must never unconditionally
/// `remove_file` a path first, since that can unlink a winner's socket
/// out from under it mid-race and orphan a perfectly live server.
///
/// This is a best-effort election, not a real distributed lock — under a
/// large number of simultaneous cold-start racers (many processes
/// starting at the exact same instant with no server yet at all) a few
/// can still occasionally fail; a real fix would need an flock'd lock
/// file. Not worth that complexity for what this bus is actually used
/// for: one long-running `tuzi` plus occasional `tuzi emit`/`tuzi sub`
/// calls, not a cold-start stampede. Jittered backoff below is enough to
/// make that realistic case reliable.
async fn connect_or_bootstrap(socket_path: &Path) -> io::Result<Connection> {
	if let Ok(stream) = UnixStream::connect(socket_path).await {
		return Ok(Connection { stream, server: None });
	}

	// Desyncs racers so they don't all reach "nobody answered, I'll clean
	// up" on the same tick and stampede `remove_file` together.
	let jitter_ms = new_peer_id() % 15;
	let mut consecutive_dead = 0u32;

	const ATTEMPTS: u32 = 12;
	for attempt in 0..ATTEMPTS {
		match Server::try_bind(socket_path) {
			Ok(listener) => {
				let server = Server::serve(listener);
				return UnixStream::connect(socket_path).await.map(|stream| Connection { stream, server: Some(server) });
			}
			Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
				// Lost the race — someone else's `bind` just won. Give
				// them a moment to start accepting and connect to them
				// instead of fighting over the path again.
				if let Ok(stream) = UnixStream::connect(socket_path).await {
					return Ok(Connection { stream, server: None });
				}
				consecutive_dead += 1;
				// Only clear the path once several attempts in a row
				// found neither a bindable nor a connectable socket —
				// one miss can just be scheduling noise, not proof the
				// path is truly abandoned.
				if consecutive_dead >= 3 {
					let _ = std::fs::remove_file(socket_path);
					consecutive_dead = 0;
				}
			}
			Err(error) => return Err(error),
		}
		let backoff = Duration::from_millis((attempt as u64 + 1) * 10 + jitter_ms);
		tokio::time::sleep(backoff).await;
	}
	UnixStream::connect(socket_path).await.map(|stream| Connection { stream, server: None })
}

struct Server;

impl Server {
	fn try_bind(socket_path: &Path) -> io::Result<UnixListener> {
		if let Some(parent) = socket_path.parent() {
			std::fs::create_dir_all(parent)?;
			let metadata = std::fs::metadata(parent)?;
			// SAFETY: `geteuid` has no preconditions and only reads process state.
			if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
				return Err(io::Error::new(io::ErrorKind::PermissionDenied, "DDS runtime directory is not owned by the current user"));
			}
			std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
		}
		let listener = UnixListener::bind(socket_path)?;
		if let Err(error) = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600)) {
			drop(listener);
			let _ = std::fs::remove_file(socket_path);
			return Err(error);
		}
		Ok(listener)
	}

	fn serve(listener: UnixListener) -> JoinHandle<()> {
		tokio::spawn(async move {
			let peers: PeerTable = Arc::new(Mutex::new(HashMap::new()));
			let mut connections = JoinSet::new();
			while let Ok((stream, _)) = listener.accept().await {
				connections.spawn(handle_connection(stream, peers.clone()));
			}
		})
	}
}

async fn handle_connection(stream: UnixStream, peers: PeerTable) {
	let (read_half, mut write_half) = stream.into_split();
	let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
	let mut line_rx = line_rx;

	let mut lines = BufReader::new(read_half).lines();
	let mut connected: Option<PeerId> = None;
	loop {
		select! {
			line = lines.next_line() => {
				let Ok(Some(line)) = line else { break };
				let Ok(payload) = serde_json::from_str::<Payload>(&line) else { continue };
				match &payload.body {
					Body::Hi { abilities } if connected.is_none() => {
						if !route_hi(&peers, payload.sender, abilities, line_tx.clone()).await {
							// Never let a new connection replace an existing peer's
							// routing identity. Closing it forces the claimant to retry.
							break;
						}
						connected = Some(payload.sender);
					}
					Body::Hi { .. } => break,
					_ if connected == Some(payload.sender) => route(&peers, &payload, &line).await,
					_ => break,
				}
			}
			message = line_rx.recv() => {
				let Some(mut line) = message else { break };
				line.push('\n');
				if write_half.write_all(line.as_bytes()).await.is_err() { break; }
			}
		}
	}
	if let Some(id) = connected {
		let mut table = peers.lock().await;
		if table.remove(&id).is_some() {
			broadcast_hey(&table);
		}
	}
}

async fn route_hi(peers: &PeerTable, sender: PeerId, abilities: &[String], outbox: mpsc::UnboundedSender<String>) -> bool {
	let mut table = peers.lock().await;
	if table.contains_key(&sender) {
		return false;
	}
	table.insert(sender, Peer { abilities: abilities.iter().cloned().collect(), outbox });
	broadcast_hey(&table);
	true
}

fn broadcast_hey(table: &HashMap<PeerId, Peer>) {
	let hey = Payload::broadcast(0, Body::Hey {
		peers: table.iter().map(|(&id, peer)| PeerInfo { id, abilities: peer.abilities.iter().cloned().collect() }).collect(),
	});
	let Ok(line) = serde_json::to_string(&hey) else { return };
	for peer in table.values() {
		let _ = peer.outbox.send(line.clone());
	}
}

async fn route(peers: &PeerTable, payload: &Payload, line: &str) {
	let table = peers.lock().await;
	if payload.receiver == 0 {
		for (&id, peer) in table.iter() {
			let interested = peer.abilities.contains(WILDCARD_ABILITY) || peer.abilities.contains(payload.body.kind());
			if id != payload.sender && interested {
				let _ = peer.outbox.send(line.to_string());
			}
		}
	} else if let Some(peer) = table.get(&payload.receiver) {
		let _ = peer.outbox.send(line.to_string());
	}
}

#[cfg(test)]
mod tests {
	use std::{
		sync::atomic::{AtomicU64, Ordering},
		time::Duration,
	};

	use tokio::time::timeout;

	use super::*;

	fn unique_socket_path() -> std::path::PathBuf {
		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let n = COUNTER.fetch_add(1, Ordering::Relaxed);
		std::env::temp_dir().join(format!("tuzi-dds-test-{}-{n}", std::process::id())).join("dds.sock")
	}

	/// Consumes `Hey` broadcasts on `inbox` until one lists at least `want`
	/// peers — the deterministic way to know the server has finished
	/// registering everyone this test connected before publishing.
	async fn wait_for_peer_count(inbox: &mut mpsc::UnboundedReceiver<Payload>, want: usize) {
		loop {
			let payload = timeout(Duration::from_secs(2), inbox.recv()).await.expect("timed out waiting for Hey").expect("channel closed");
			if let Body::Hey { peers } = payload.body
				&& peers.len() >= want
			{
				return;
			}
		}
	}

	async fn wait_for_exact_peer_count(inbox: &mut mpsc::UnboundedReceiver<Payload>, want: usize) {
		loop {
			let payload = timeout(Duration::from_secs(2), inbox.recv()).await.expect("timed out waiting for Hey").expect("channel closed");
			if let Body::Hey { peers } = payload.body
				&& peers.len() == want
			{
				return;
			}
		}
	}

	/// Every connected peer gets its own copy of every handshake `Hey`, not
	/// just the one waited on in `wait_for_peer_count` — skip those to get
	/// at the payload a test actually cares about.
	async fn recv(inbox: &mut mpsc::UnboundedReceiver<Payload>) -> Payload {
		loop {
			let payload = timeout(Duration::from_secs(2), inbox.recv()).await.expect("timed out waiting for a payload").expect("channel closed");
			if !matches!(payload.body, Body::Hey { .. } | Body::Hi { .. }) {
				return payload;
			}
		}
	}

	/// Same handshake-noise caveat as `recv`: a stale, never-drained `Hey`
	/// sitting ahead in the channel isn't the "no payload" this is meant
	/// to assert.
	async fn recv_nothing(inbox: &mut mpsc::UnboundedReceiver<Payload>) {
		loop {
			match timeout(Duration::from_millis(200), inbox.recv()).await {
				Err(_) => return,
				Ok(None) => return,
				Ok(Some(payload)) if matches!(payload.body, Body::Hey { .. } | Body::Hi { .. }) => continue,
				Ok(Some(_)) => panic!("expected no payload, but one arrived"),
			}
		}
	}

	#[tokio::test]
	async fn broadcast_only_reaches_peers_that_declared_the_kind() {
		let socket_path = unique_socket_path();

		let (_yank_sub, mut yank_inbox) = Client::connect(&socket_path, vec!["yank".into()]).await.unwrap();
		let (_cd_sub, mut cd_inbox) = Client::connect(&socket_path, vec!["cd".into()]).await.unwrap();
		let (publisher, mut pub_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		wait_for_peer_count(&mut pub_inbox, 3).await;

		publisher.publish(Body::Yank { paths: vec!["/tmp/a".into()], cut: false });

		assert!(matches!(recv(&mut yank_inbox).await.body, Body::Yank { .. }));
		recv_nothing(&mut cd_inbox).await;

		let _ = std::fs::remove_file(&socket_path);
	}

	#[tokio::test]
	async fn direct_addressing_reaches_only_that_peer() {
		let socket_path = unique_socket_path();

		let (a, mut a_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		let (_b, mut b_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		let (sender, mut sender_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		wait_for_peer_count(&mut sender_inbox, 3).await;

		sender.publish_to(a.id(), Body::Custom { kind: "ping".into(), data: serde_json::Value::Null });

		let received = recv(&mut a_inbox).await;
		assert_eq!(received.receiver, a.id());
		recv_nothing(&mut b_inbox).await;

		let _ = std::fs::remove_file(&socket_path);
	}

	#[tokio::test]
	async fn duplicate_peer_ids_do_not_replace_the_original_route() {
		let peers: PeerTable = Default::default();
		let (original_tx, _original_rx) = mpsc::unbounded_channel();
		let (claimant_tx, _claimant_rx) = mpsc::unbounded_channel();

		assert!(route_hi(&peers, 42, &["first".into()], original_tx).await);
		assert!(!route_hi(&peers, 42, &["second".into()], claimant_tx).await);

		let table = peers.lock().await;
		let peer = table.get(&42).unwrap();
		assert!(peer.abilities.contains("first"));
		assert!(!peer.abilities.contains("second"));
	}

	#[test]
	fn reconnects_immediately_then_uses_bounded_backoff() {
		let delays: Vec<_> = (0..7).map(|failure| reconnect_backoff(failure).as_millis()).collect();
		assert_eq!(delays, [20, 50, 100, 250, 500, 500, 500]);
	}

	#[tokio::test]
	async fn a_wildcard_subscriber_sees_every_kind() {
		let socket_path = unique_socket_path();

		let (_sub, mut sub_inbox) = Client::connect(&socket_path, vec![WILDCARD_ABILITY.to_string()]).await.unwrap();
		let (publisher, mut pub_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		wait_for_peer_count(&mut pub_inbox, 2).await;

		publisher.publish(Body::Custom { kind: "anything".into(), data: serde_json::Value::Null });
		assert_eq!(recv(&mut sub_inbox).await.body.kind(), "anything");

		let _ = std::fs::remove_file(&socket_path);
	}

	#[tokio::test]
	async fn flush_waits_for_the_write_to_land_before_returning() {
		let socket_path = unique_socket_path();

		let (_sub, mut sub_inbox) = Client::connect(&socket_path, vec![WILDCARD_ABILITY.to_string()]).await.unwrap();
		let (publisher, mut pub_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		wait_for_peer_count(&mut pub_inbox, 2).await;

		publisher.publish(Body::Custom { kind: "bye".into(), data: serde_json::Value::Null });
		publisher.flush().await;

		assert_eq!(recv(&mut sub_inbox).await.body.kind(), "bye");

		let _ = std::fs::remove_file(&socket_path);
	}

	#[tokio::test]
	async fn a_second_client_reuses_the_first_clients_self_elected_server() {
		let socket_path = unique_socket_path();

		let (_first, mut first_inbox) = Client::connect(&socket_path, vec!["cd".into()]).await.unwrap();
		let (second, mut second_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		wait_for_peer_count(&mut second_inbox, 2).await;

		second.publish(Body::Cd { path: "/tmp".into() });
		assert!(matches!(recv(&mut first_inbox).await.body, Body::Cd { .. }), "the second client's message reached the first over the socket the first one bootstrapped");

		let _ = std::fs::remove_file(&socket_path);
	}

	#[tokio::test]
	async fn disconnect_broadcasts_an_updated_peer_table() {
		let socket_path = unique_socket_path();

		let (_owner, mut owner_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		let (departing, mut departing_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		let (_observer, mut observer_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		wait_for_peer_count(&mut owner_inbox, 3).await;
		wait_for_peer_count(&mut departing_inbox, 3).await;
		wait_for_peer_count(&mut observer_inbox, 3).await;

		departing.flush().await;
		wait_for_exact_peer_count(&mut observer_inbox, 2).await;

		let _ = std::fs::remove_file(&socket_path);
	}

	#[tokio::test]
	async fn socket_and_runtime_directory_are_private_to_the_owner() {
		let socket_path = unique_socket_path();
		let (_client, _inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();

		let directory_mode = std::fs::metadata(socket_path.parent().unwrap()).unwrap().permissions().mode() & 0o777;
		let socket_mode = std::fs::metadata(&socket_path).unwrap().permissions().mode() & 0o777;
		assert_eq!(directory_mode, 0o700);
		assert_eq!(socket_mode, 0o600);

		let _ = std::fs::remove_file(&socket_path);
	}

	#[tokio::test]
	async fn surviving_peers_re_elect_a_server_after_its_owner_exits() {
		let socket_path = unique_socket_path();

		let (owner, mut owner_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		let (_subscriber, mut subscriber_inbox) = Client::connect(&socket_path, vec!["ping".into()]).await.unwrap();
		let (publisher, mut publisher_inbox) = Client::connect(&socket_path, Vec::new()).await.unwrap();
		wait_for_peer_count(&mut owner_inbox, 3).await;
		wait_for_peer_count(&mut subscriber_inbox, 3).await;
		wait_for_peer_count(&mut publisher_inbox, 3).await;

		// `owner` won the initial bootstrap. Ending its supervisor also ends
		// the server runtime and every accepted connection, just as exiting
		// the owner process would in production.
		owner.flush().await;

		wait_for_peer_count(&mut subscriber_inbox, 2).await;
		wait_for_peer_count(&mut publisher_inbox, 2).await;
		publisher.publish(Body::Custom { kind: "ping".into(), data: serde_json::Value::Null });
		assert_eq!(recv(&mut subscriber_inbox).await.body.kind(), "ping");

		let _ = std::fs::remove_file(&socket_path);
	}
}
