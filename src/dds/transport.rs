//! The Unix-socket half of DDS (`.ai/dds-plan.md` P3): whichever process
//! tries to connect first and finds nobody listening becomes the `Server`
//! for every later `Client`, including itself. Kept local-only (no
//! `Send`-across-network transport) — every peer lives on the same
//! machine.

use std::{
	collections::{HashMap, HashSet},
	io,
	path::Path,
	sync::Arc,
	time::Duration,
};

use tokio::{
	io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
	net::{UnixListener, UnixStream},
	sync::{Mutex, mpsc},
};

use super::{Body, Payload, PeerId, PeerInfo, new_peer_id};

/// A connected DDS client: send `publish`/`publish_to` to write onto the
/// socket, and drain the paired receiver for payloads other peers sent us.
pub struct Client {
	id:         PeerId,
	outbox:     mpsc::UnboundedSender<Payload>,
	write_task: tokio::task::JoinHandle<()>,
}

/// A peer that wants every kind forwarded to it, regardless of ability —
/// what `tuzi sub` announces to act as a debug tap on the whole bus.
pub const WILDCARD_ABILITY: &str = "*";

impl Client {
	/// Connects to `socket_path`, bootstrapping a `Server` there first if
	/// nothing answers. `abilities` are the kinds this client wants
	/// forwarded from other peers — announced immediately via `Hi`.
	pub async fn connect(socket_path: &Path, abilities: Vec<String>) -> io::Result<(Self, mpsc::UnboundedReceiver<Payload>)> {
		let stream = connect_or_bootstrap(socket_path).await?;

		let id = new_peer_id();
		let (read_half, write_half) = stream.into_split();
		let (outbox_tx, outbox_rx) = mpsc::unbounded_channel::<Payload>();
		let (inbox_tx, inbox_rx) = mpsc::unbounded_channel::<Payload>();

		let write_task = tokio::spawn(write_loop(write_half, outbox_rx));
		tokio::spawn(async move {
			let mut lines = BufReader::new(read_half).lines();
			while let Ok(Some(line)) = lines.next_line().await {
				if let Ok(payload) = serde_json::from_str::<Payload>(&line)
					&& payload.sender != id
				{
					let _ = inbox_tx.send(payload);
				}
			}
		});

		let client = Self { id, outbox: outbox_tx, write_task };
		let _ = client.outbox.send(Payload::broadcast(id, Body::Hi { abilities }));
		Ok((client, inbox_rx))
	}

	// No production caller needs its own id or point-to-point addressing
	// yet (`tuzi emit`/`tuzi sub` only broadcast/listen); exercised by the
	// tests below until a real point-to-point use case lands.
	#[allow(dead_code)]
	pub fn id(&self) -> PeerId {
		self.id
	}

	/// Broadcasts to every peer that declared interest in `body`'s kind.
	pub fn publish(&self, body: Body) {
		let _ = self.outbox.send(Payload::broadcast(self.id, body));
	}

	#[allow(dead_code)]
	pub fn publish_to(&self, receiver: PeerId, body: Body) {
		let _ = self.outbox.send(Payload { receiver, sender: self.id, body });
	}

	/// Closes the outbound channel and waits for every already-queued
	/// payload to actually hit the socket. A short-lived CLI invocation
	/// (`tuzi emit`) needs this — otherwise the process can exit before
	/// its background write task gets a chance to run.
	pub async fn flush(self) {
		drop(self.outbox);
		let _ = self.write_task.await;
	}
}

async fn write_loop(mut write_half: tokio::net::unix::OwnedWriteHalf, mut outbox: mpsc::UnboundedReceiver<Payload>) {
	while let Some(payload) = outbox.recv().await {
		let Ok(mut line) = serde_json::to_string(&payload) else { continue };
		line.push('\n');
		if write_half.write_all(line.as_bytes()).await.is_err() {
			break;
		}
	}
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
async fn connect_or_bootstrap(socket_path: &Path) -> io::Result<UnixStream> {
	if let Ok(stream) = UnixStream::connect(socket_path).await {
		return Ok(stream);
	}

	// Desyncs racers so they don't all reach "nobody answered, I'll clean
	// up" on the same tick and stampede `remove_file` together.
	let jitter_ms = new_peer_id() % 15;
	let mut consecutive_dead = 0u32;

	const ATTEMPTS: u32 = 12;
	for attempt in 0..ATTEMPTS {
		match Server::try_bind(socket_path) {
			Ok(listener) => {
				Server::serve(listener);
				return UnixStream::connect(socket_path).await;
			}
			Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
				// Lost the race — someone else's `bind` just won. Give
				// them a moment to start accepting and connect to them
				// instead of fighting over the path again.
				if let Ok(stream) = UnixStream::connect(socket_path).await {
					return Ok(stream);
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
	UnixStream::connect(socket_path).await
}

struct Server;

impl Server {
	fn try_bind(socket_path: &Path) -> io::Result<UnixListener> {
		if let Some(parent) = socket_path.parent() {
			std::fs::create_dir_all(parent)?;
		}
		UnixListener::bind(socket_path)
	}

	fn serve(listener: UnixListener) {
		tokio::spawn(async move {
			let peers: PeerTable = Arc::new(Mutex::new(HashMap::new()));
			while let Ok((stream, _)) = listener.accept().await {
				tokio::spawn(handle_connection(stream, peers.clone()));
			}
		});
	}
}

async fn handle_connection(stream: UnixStream, peers: PeerTable) {
	let (read_half, write_half) = stream.into_split();
	let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
	tokio::spawn(write_lines(write_half, line_rx));

	let mut lines = BufReader::new(read_half).lines();
	let mut connected: Option<PeerId> = None;
	while let Ok(Some(line)) = lines.next_line().await {
		let Ok(payload) = serde_json::from_str::<Payload>(&line) else { continue };
		match &payload.body {
			Body::Hi { abilities } => {
				connected = Some(payload.sender);
				route_hi(&peers, payload.sender, abilities, line_tx.clone()).await;
			}
			_ => route(&peers, &payload, &line).await,
		}
	}
	if let Some(id) = connected {
		peers.lock().await.remove(&id);
	}
}

async fn write_lines(mut write_half: tokio::net::unix::OwnedWriteHalf, mut lines: mpsc::UnboundedReceiver<String>) {
	while let Some(mut line) = lines.recv().await {
		line.push('\n');
		if write_half.write_all(line.as_bytes()).await.is_err() {
			break;
		}
	}
}

async fn route_hi(peers: &PeerTable, sender: PeerId, abilities: &[String], outbox: mpsc::UnboundedSender<String>) {
	let mut table = peers.lock().await;
	table.insert(sender, Peer { abilities: abilities.iter().cloned().collect(), outbox });

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
		std::env::temp_dir().join(format!("tuzi-dds-test-{}-{n}.sock", std::process::id()))
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
}
