use std::{
	collections::{HashMap, HashSet},
	io,
	path::{Path, PathBuf},
	sync::{
		Arc, Mutex,
		mpsc::{self, SyncSender},
	},
	thread,
	time::Duration,
};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use tokio::{runtime::Handle, sync::mpsc::UnboundedSender, task::AbortHandle, time::Instant};

use crate::{
	event::Event,
	fs::{Cha, FsChange},
};

/// Something went wrong with file watching that the view cannot recover from
/// by itself. The watcher only reports it; the app decides what to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WatchIssue {
	/// The OS dropped events (for inotify, its queue overflowed), so any
	/// directory may have changed without us hearing about it.
	EventsDropped,
	/// The backend failed to read events.
	BackendError(String),
	/// A watch could not be registered, so that directory will not update on
	/// its own. Directories that no longer exist are not reported.
	RegisterFailed { path: PathBuf, error: String },
}

type IssueReporter = Arc<dyn Fn(WatchIssue) + Send + Sync>;

pub struct Watcher {
	commands: mpsc::Sender<WatchCommand>,
	worker: Option<thread::JoinHandle<()>>,
	pending: PendingChanges,
}

enum WatchCommand {
	Watch { path: PathBuf, reply: Option<SyncSender<io::Result<()>>> },
	Unwatch(PathBuf),
	Shutdown,
}

/// `notify` needs the canonical (symlink-resolved) path to actually register
/// an OS-level watch, and its backends sometimes report that resolved form
/// back in events too — but the tree indexes nodes by their own, possibly
/// symlinked, path. This keeps both directions so raw events can be matched
/// in canonical space while everything handed back to the app (and `unwatch`)
/// stays in the tree's own path space.
#[derive(Default)]
struct WatchIndex {
	by_canonical: HashMap<PathBuf, PathBuf>,
	by_original: HashMap<PathBuf, PathBuf>,
}

const MAX_CHANGE_BATCH: usize = 1000;
/// Caps how long a directory under nonstop churn (e.g. a log file being
/// appended to faster than the configured debounce apart) can be starved of an
/// update — the sliding debounce below would otherwise keep resetting
/// forever.

type PendingChanges = Arc<Mutex<HashMap<PathBuf, PendingChange>>>;

struct PendingChange {
	first_seen: Instant,
	deadline: Instant,
	paths: HashSet<PathBuf>,
	refresh: bool,
	abort: Option<AbortHandle>,
}

struct ChangeSet {
	parent: PathBuf,
	paths: HashSet<PathBuf>,
	refresh: bool,
}

impl Watcher {
	pub fn new(tab: usize, tx: UnboundedSender<Event>, debounce: Duration, max_wait: Duration, poll_interval: Duration) -> io::Result<Self> {
		let watched = Arc::new(Mutex::new(WatchIndex::default()));
		let pending = Arc::new(Mutex::new(HashMap::new()));
		let runtime = Handle::try_current().map_err(io::Error::other)?;
		let notify_config = Config::default().with_poll_interval(poll_interval).with_compare_contents(true);
		let report = issue_reporter(tab, tx.clone());
		let inner = RecommendedWatcher::new(event_handler(tab, tx, watched.clone(), pending.clone(), runtime, debounce, max_wait), notify_config).map_err(io::Error::other)?;
		Ok(Self::with_inner(Box::new(inner), watched, pending, report))
	}

	#[cfg(test)]
	fn new_polling(tab: usize, tx: UnboundedSender<Event>, interval: std::time::Duration) -> io::Result<Self> {
		let watched = Arc::new(Mutex::new(WatchIndex::default()));
		let pending = Arc::new(Mutex::new(HashMap::new()));
		let runtime = Handle::try_current().map_err(io::Error::other)?;
		let config = Config::default().with_poll_interval(interval).with_compare_contents(true);
		let report = issue_reporter(tab, tx.clone());
		let inner = notify::PollWatcher::new(event_handler(tab, tx, watched.clone(), pending.clone(), runtime, Duration::from_millis(250), Duration::from_secs(1)), config).map_err(io::Error::other)?;
		Ok(Self::with_inner(Box::new(inner), watched, pending, report))
	}

	fn with_inner(mut inner: Box<dyn NotifyWatcher + Send>, watched: Arc<Mutex<WatchIndex>>, pending: PendingChanges, report: IssueReporter) -> Self {
		let (commands, rx) = mpsc::channel();
		let worker_watched = watched.clone();
		let worker = thread::Builder::new()
			.name("tuzi-watcher".into())
			.spawn(move || {
				while let Ok(command) = rx.recv() {
					match command {
						WatchCommand::Watch { path, reply } => {
							let result = register_watch(inner.as_mut(), &worker_watched, &path);
							match reply {
								Some(reply) => {
									let _ = reply.send(result);
								}
								// Nobody is waiting for this result, so a failure
								// would otherwise leave the directory silently
								// stale. But a directory that is gone, or that we
								// may not read, is that directory's own problem and
								// its listing already shows it; only a failure that
								// says something about watching itself is worth
								// interrupting the user for (e.g. the OS limit on
								// watches).
								None => {
									if let Err(error) = result
										&& !matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied)
									{
										report(WatchIssue::RegisterFailed { path, error: error.to_string() });
									}
								}
							}
						}
						WatchCommand::Unwatch(path) => unregister_watch(inner.as_mut(), &worker_watched, &path),
						WatchCommand::Shutdown => break,
					}
				}
			})
			.expect("watcher worker thread");
		Self { commands, worker: Some(worker), pending }
	}

	/// Watches one directory level, non-recursively — nodes watch themselves
	/// only while expanded, mirroring what's actually visible on screen.
	pub fn watch(&self, path: &Path) -> io::Result<()> {
		let (tx, rx) = mpsc::sync_channel(1);
		self.commands.send(WatchCommand::Watch { path: path.to_path_buf(), reply: Some(tx) }).map_err(io::Error::other)?;
		rx.recv().map_err(io::Error::other)?
	}

	/// Queues the complete canonicalize-and-register operation. Expanding a
	/// node therefore never waits on a slow symlink target or network mount.
	pub fn watch_async(&self, path: PathBuf) {
		let _ = self.commands.send(WatchCommand::Watch { path, reply: None });
	}

	/// Takes the same (tree-space) path `watch` was given — not re-derived
	/// from the filesystem, so this still works even if whatever the path
	/// pointed at has already vanished.
	pub fn unwatch(&self, path: &Path) {
		let _ = self.commands.send(WatchCommand::Unwatch(path.to_path_buf()));
		if let Some(change) = self.pending.lock().unwrap().remove(path)
			&& let Some(abort) = change.abort
		{
			abort.abort();
		}
	}
}

impl Drop for Watcher {
	fn drop(&mut self) {
		let _ = self.commands.send(WatchCommand::Shutdown);
		if let Some(worker) = self.worker.take() {
			let _ = worker.join();
		}
		for (_, change) in self.pending.lock().unwrap().drain() {
			if let Some(abort) = change.abort {
				abort.abort();
			}
		}
	}
}

/// Registers, or re-registers, the watch for one directory.
///
/// A path we already know is *not* skipped: the kernel drops a watch when its
/// directory is deleted or moved away, without telling us. If the path then
/// comes back as a new directory, skipping it here would leave that directory
/// unwatched for good, so a known path is unwatched and armed again.
fn register_watch(inner: &mut dyn NotifyWatcher, watched: &Mutex<WatchIndex>, path: &Path) -> io::Result<()> {
	let canonical = path.canonicalize()?;
	let (known, moved_from) = {
		let watched = watched.lock().unwrap();
		let known = watched.by_canonical.contains_key(&canonical);
		// The same tree path may now resolve elsewhere (a retargeted symlink).
		let moved_from = watched.by_original.get(path).filter(|old| **old != canonical).cloned();
		(known, moved_from)
	};
	if known {
		let _ = inner.unwatch(&canonical);
	}
	if let Some(old) = moved_from {
		let _ = inner.unwatch(&old);
		watched.lock().unwrap().by_canonical.remove(&old);
	}
	inner.watch(&canonical, RecursiveMode::NonRecursive).map_err(notify_to_io)?;
	let mut watched = watched.lock().unwrap();
	watched.by_canonical.insert(canonical.clone(), path.to_path_buf());
	watched.by_original.insert(path.to_path_buf(), canonical);
	Ok(())
}

/// Converts a notify error without losing what an I/O error was (so that
/// "permission denied" can still be told from "too many watches").
fn notify_to_io(error: notify::Error) -> io::Error {
	match &error.kind {
		notify::ErrorKind::Io(source) => io::Error::new(source.kind(), error.to_string()),
		notify::ErrorKind::PathNotFound => io::Error::new(io::ErrorKind::NotFound, error.to_string()),
		_ => io::Error::other(error),
	}
}

fn issue_reporter(tab: usize, tx: UnboundedSender<Event>) -> IssueReporter {
	Arc::new(move |issue| {
		let _ = tx.send(Event::WatchIssue { tab, issue });
	})
}

/// How long after reporting one issue the next may be reported. Shortened
/// under test so the tests need not sleep for seconds.
const ISSUE_COOLDOWN: Duration = Duration::from_millis(if cfg!(test) { 150 } else { 2000 });

/// Rate-limits watch issues. A storm of dropped events produces one
/// notification per read, and one rescan covers all of them, so an issue
/// arriving while one was just reported is held back and reported once the
/// cooldown ends, never lost and never more than one per cooldown. Everything
/// is decided under one lock so an issue cannot slip between "nothing left to
/// send" and "gate reopened".
#[derive(Default)]
struct IssueGate {
	state: Mutex<GateState>,
}

#[derive(Default)]
struct GateState {
	running: bool,
	queued: Option<WatchIssue>,
}

fn raise_issue(tab: usize, tx: &UnboundedSender<Event>, runtime: &Handle, gate: &Arc<IssueGate>, delay: Duration, issue: WatchIssue) {
	{
		let mut state = gate.state.lock().unwrap();
		if state.running {
			state.queued = Some(issue);
			return;
		}
		state.running = true;
	}
	let (tx, gate) = (tx.clone(), gate.clone());
	runtime.spawn(async move {
		tokio::time::sleep(delay).await;
		let _ = tx.send(Event::WatchIssue { tab, issue });
		loop {
			tokio::time::sleep(ISSUE_COOLDOWN).await;
			let next = {
				let mut state = gate.state.lock().unwrap();
				let next = state.queued.take();
				state.running = next.is_some();
				next
			};
			match next {
				Some(issue) => {
					let _ = tx.send(Event::WatchIssue { tab, issue });
				}
				None => break,
			}
		}
	});
}

fn unregister_watch(inner: &mut dyn NotifyWatcher, watched: &Mutex<WatchIndex>, path: &Path) {
	let mut watched = watched.lock().unwrap();
	let Some(canonical) = watched.by_original.remove(path) else {
		return;
	};
	watched.by_canonical.remove(&canonical);
	drop(watched);
	let _ = inner.unwatch(&canonical);
}

// The backend (FSEvents/inotify/...) doesn't consistently report a changed
// child's path vs. its watched parent, and may resolve symlinks along the
// way — so the ancestor walk below matches in canonical space. Everything
// downstream of that match (the `changed` map, debounce bookkeeping, the
// event finally sent out) is then translated back to the tree's own path,
// which is what `Tab` actually indexes its nodes by.
fn event_handler(tab: usize, tx: UnboundedSender<Event>, matched: Arc<Mutex<WatchIndex>>, pending: PendingChanges, runtime: Handle, debounce: Duration, max_wait: Duration) -> impl FnMut(notify::Result<notify::Event>) + Send + 'static {
	let issue_gate = Arc::new(IssueGate::default());
	let issue_delay = debounce.max(Duration::from_millis(200));
	move |res| {
		let event = match res {
			Ok(event) => event,
			// A path that vanished mid-flight is the parent directory's news
			// to report; anything else means we may have missed changes.
			Err(error) if matches!(error.kind, notify::ErrorKind::PathNotFound | notify::ErrorKind::WatchNotFound) => return,
			Err(error) => {
				raise_issue(tab, &tx, &runtime, &issue_gate, issue_delay, WatchIssue::BackendError(error.to_string()));
				return;
			}
		};
		if event.need_rescan() {
			raise_issue(tab, &tx, &runtime, &issue_gate, issue_delay, WatchIssue::EventsDropped);
		}
		if event.kind.is_access() {
			return;
		}
		let watched = matched.lock().unwrap();
		let mut changed: HashMap<PathBuf, (HashSet<PathBuf>, bool)> = HashMap::new();
		for path in event.paths {
			if let Some(canonical_dir) = nearest_watched(&watched.by_canonical, &path) {
				let Some(original_dir) = watched.by_canonical.get(&canonical_dir) else {
					continue;
				};
				let entry = changed.entry(original_dir.clone()).or_default();
				if path == canonical_dir {
					entry.1 = true;
				} else if let Ok(relative) = path.strip_prefix(&canonical_dir)
					&& let Some(component) = relative.components().next()
				{
					entry.0.insert(original_dir.join(component.as_os_str()));
				}
			}
		}
		drop(watched);
		for (parent, (paths, refresh)) in changed {
			debounce_changes(tab, tx.clone(), pending.clone(), &runtime, ChangeSet { parent, paths, refresh }, debounce, max_wait);
		}
	}
}

fn debounce_changes(tab: usize, tx: UnboundedSender<Event>, pending: PendingChanges, runtime: &Handle, change_set: ChangeSet, delay: Duration, max_wait: Duration) {
	let ChangeSet { parent, paths, refresh } = change_set;
	let now = Instant::now();
	let deadline = now + delay;
	if let Some(change) = pending.lock().unwrap().get_mut(&parent) {
		change.paths.extend(paths);
		change.refresh |= refresh;
		let capped = change.first_seen + max_wait;
		change.deadline = if change.paths.len() >= MAX_CHANGE_BATCH { now } else { deadline.min(capped) };
		return;
	}
	pending.lock().unwrap().insert(
		parent.clone(),
		PendingChange {
			first_seen: now,
			deadline,
			paths,
			refresh,
			abort: None,
		},
	);
	let task_parent = parent.clone();
	let task_pending = pending.clone();
	let handle = runtime.spawn(async move {
		loop {
			let Some(deadline) = task_pending.lock().unwrap().get(&task_parent).map(|change| change.deadline) else {
				return;
			};
			tokio::time::sleep_until(deadline).await;
			let change = {
				let mut pending = task_pending.lock().unwrap();
				if pending.get(&task_parent).is_some_and(|change| change.deadline <= Instant::now()) {
					pending.remove(&task_parent)
				} else {
					None
				}
			};
			if let Some(change) = change {
				if change.refresh {
					let _ = tx.send(Event::Changed { tab, path: task_parent });
					return;
				}
				let changes = tokio::task::spawn_blocking(move || inspect_paths(change.paths)).await.unwrap_or_default();
				if !changes.is_empty() {
					let _ = tx.send(Event::FilesChanged { tab, parent: task_parent, changes });
				}
				return;
			}
		}
	});
	if let Some(change) = pending.lock().unwrap().get_mut(&parent) {
		change.abort = Some(handle.abort_handle());
	}
}

fn inspect_paths(paths: HashSet<PathBuf>) -> Vec<FsChange> {
	paths
		.into_iter()
		.filter_map(|path| match std::fs::symlink_metadata(&path) {
			Ok(metadata) => {
				let mut cha = Cha::from(metadata);
				cha.resolve_symlink(&path);
				Some(FsChange::Upsert { path, cha })
			}
			Err(error) if error.kind() == io::ErrorKind::NotFound => Some(FsChange::Delete { path }),
			Err(_) => None,
		})
		.collect()
}

fn nearest_watched(by_canonical: &HashMap<PathBuf, PathBuf>, path: &Path) -> Option<PathBuf> {
	let mut cur = Some(path);
	while let Some(p) = cur {
		if by_canonical.contains_key(p) {
			return Some(p.to_path_buf());
		}
		cur = p.parent();
	}
	None
}

#[cfg(test)]
mod tests {
	use std::{
		fs,
		time::{Duration, SystemTime, UNIX_EPOCH},
	};

	use tokio::sync::mpsc;

	use super::*;

	#[tokio::test]
	async fn repeated_changes_for_one_directory_are_debounced() {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let pending = Arc::new(Mutex::new(HashMap::new()));
		let runtime = Handle::current();
		let parent = PathBuf::from("busy-directory");
		let path = parent.join("changed-file");

		for _ in 0..5 {
			debounce_changes(
				3,
				tx.clone(),
				pending.clone(),
				&runtime,
				ChangeSet {
					parent: parent.clone(),
					paths: HashSet::from([path.clone()]),
					refresh: false,
				},
				Duration::from_millis(20),
				Duration::from_secs(1),
			);
		}

		let event = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await.unwrap().unwrap();
		assert!(matches!(event, Event::FilesChanged { tab: 3, parent: ref changed, changes } if changed == &parent && matches!(changes.as_slice(), [FsChange::Delete { path: deleted }] if deleted == &path)));
		assert!(tokio::time::timeout(Duration::from_millis(60), rx.recv()).await.is_err());
	}

	#[tokio::test]
	async fn nonstop_churn_still_flushes_once_max_wait_elapses() {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let pending = Arc::new(Mutex::new(HashMap::new()));
		let parent = PathBuf::from("hot-directory");
		let path = parent.join("hot-file");

		// Keeps resetting the idle deadline every 35ms, well inside the
		// 60ms idle window, so the debounce alone would never fire.
		let (churn_tx, churn_pending, churn_parent, churn_path) = (tx.clone(), pending.clone(), parent.clone(), path.clone());
		tokio::spawn(async move {
			let runtime = Handle::current();
			loop {
				debounce_changes(
					9,
					churn_tx.clone(),
					churn_pending.clone(),
					&runtime,
					ChangeSet {
						parent: churn_parent.clone(),
						paths: HashSet::from([churn_path.clone()]),
						refresh: false,
					},
					Duration::from_millis(60),
					Duration::from_millis(120),
				);
				tokio::time::sleep(Duration::from_millis(35)).await;
			}
		});

		// `max_wait` (120ms) must force a flush well inside this timeout,
		// even though the churner above never lets the idle debounce go
		// quiet for the whole run.
		let event = tokio::time::timeout(Duration::from_millis(250), rx.recv()).await.expect("max_wait did not force a flush").unwrap();
		assert!(matches!(event, Event::FilesChanged { tab: 9, .. }));
	}

	#[tokio::test]
	async fn external_write_produces_a_changed_event() {
		let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
		let dir = std::env::temp_dir().join(format!("tuzi-watcher-test-{}-{nonce}", std::process::id()));
		fs::create_dir(&dir).unwrap();
		let dir = dir.canonicalize().unwrap();

		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = Watcher::new_polling(7, tx, Duration::from_millis(25)).unwrap();
		watcher.watch(&dir).unwrap();
		fs::write(dir.join("new.txt"), b"hi").unwrap();

		let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("timed out").expect("channel closed");
		match event {
			Event::FilesChanged { tab, parent, changes } => {
				assert_eq!(tab, 7);
				assert_eq!(parent, dir);
				assert!(matches!(changes.as_slice(), [FsChange::Upsert { path, .. }] if path == &dir.join("new.txt")));
			}
			_ => panic!("unexpected event"),
		}

		fs::remove_dir_all(&dir).unwrap();
	}

	#[cfg(unix)]
	#[tokio::test]
	async fn changes_through_a_symlinked_directory_are_reported_under_the_link_path() {
		let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
		let base = std::env::temp_dir().join(format!("tuzi-watcher-symlink-test-{}-{nonce}", std::process::id()));
		let real = base.join("real");
		let link = base.join("link");
		fs::create_dir_all(&real).unwrap();
		std::os::unix::fs::symlink(&real, &link).unwrap();

		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = Watcher::new_polling(11, tx, Duration::from_millis(25)).unwrap();
		// The tree only ever knows about `link` — it never sees `real`, the
		// canonicalized form `watch` resolves internally to register the OS
		// watch.
		watcher.watch(&link).unwrap();
		fs::write(real.join("new.txt"), b"hi").unwrap();

		let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("timed out").expect("channel closed");
		match event {
			Event::FilesChanged { tab, parent, changes } => {
				assert_eq!(tab, 11);
				assert_eq!(parent, link, "must be tagged with the tree's own path, not the symlink-resolved one");
				assert!(matches!(changes.as_slice(), [FsChange::Upsert { path, .. }] if path == &link.join("new.txt")));
			}
			_ => panic!("unexpected event"),
		}

		fs::remove_dir_all(&base).unwrap();
	}

	#[cfg(unix)]
	#[test]
	fn inspect_paths_lstats_a_changed_symlink_instead_of_following_it() {
		let root = std::env::temp_dir().join(format!("tuzi-watcher-inspect-test-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("real")).unwrap();
		std::os::unix::fs::symlink("real", root.join("link")).unwrap();

		let changes = inspect_paths(HashSet::from([root.join("link")]));
		let [FsChange::Upsert { cha, .. }] = changes.as_slice() else { panic!("expected one upsert") };
		assert!(cha.is_link, "a live refresh must still see this as a symlink, not silently follow through to the target");
		assert!(cha.is_dir, "still expandable, since it points at a directory");
		assert_eq!(cha.link_target, Some(PathBuf::from("real")));

		fs::remove_dir_all(&root).unwrap();
	}

	fn probe_handler(tx: mpsc::UnboundedSender<Event>) -> impl FnMut(notify::Result<notify::Event>) + Send + 'static {
		let watched = Arc::new(Mutex::new(WatchIndex::default()));
		let pending = Arc::new(Mutex::new(HashMap::new()));
		event_handler(1, tx, watched, pending, Handle::current(), Duration::from_millis(20), Duration::from_millis(200))
	}

	async fn next_issue(rx: &mut mpsc::UnboundedReceiver<Event>, within: Duration) -> Option<WatchIssue> {
		match tokio::time::timeout(within, rx.recv()).await {
			Ok(Some(Event::WatchIssue { tab: 1, issue })) => Some(issue),
			Ok(Some(_)) => panic!("unexpected event"),
			_ => None,
		}
	}

	fn rescan() -> notify::Result<notify::Event> {
		Ok(notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan))
	}

	#[tokio::test]
	async fn dropped_events_are_reported_so_the_view_can_be_rescanned() {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let mut handler = probe_handler(tx);
		handler(rescan());
		assert_eq!(next_issue(&mut rx, Duration::from_secs(2)).await, Some(WatchIssue::EventsDropped));
	}

	#[tokio::test]
	async fn backend_errors_are_reported_but_a_vanished_path_is_not() {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let mut handler = probe_handler(tx);
		handler(Err(notify::Error::generic("cannot read events")));
		assert!(matches!(next_issue(&mut rx, Duration::from_secs(2)).await, Some(WatchIssue::BackendError(text)) if text.contains("cannot read events")));

		// Those two are races with a directory disappearing, and the parent
		// directory reports that itself.
		tokio::time::sleep(ISSUE_COOLDOWN * 3).await;
		handler(Err(notify::Error::path_not_found()));
		handler(Err(notify::Error::watch_not_found()));
		assert_eq!(next_issue(&mut rx, Duration::from_millis(600)).await, None);
	}

	#[tokio::test]
	async fn a_storm_of_drops_is_one_report_plus_one_trailing_report() {
		let (tx, mut rx) = mpsc::unbounded_channel();
		let mut handler = probe_handler(tx);
		for _ in 0..200 {
			handler(rescan());
		}
		assert_eq!(next_issue(&mut rx, Duration::from_secs(2)).await, Some(WatchIssue::EventsDropped));
		// Events kept being dropped while that report was in flight, so one
		// more follows once the cooldown ends: nothing is lost...
		assert_eq!(next_issue(&mut rx, Duration::from_secs(2)).await, Some(WatchIssue::EventsDropped));
		// ...and 200 drops did not become 200 reports.
		assert_eq!(next_issue(&mut rx, Duration::from_millis(600)).await, None);

		// Once quiet, the gate is open again.
		handler(rescan());
		assert_eq!(next_issue(&mut rx, Duration::from_secs(2)).await, Some(WatchIssue::EventsDropped));
	}

	#[tokio::test]
	async fn a_directory_that_was_deleted_and_recreated_is_watched_again() {
		let root = std::env::temp_dir().join(format!("tuzi-watch-rearm-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("sub")).unwrap();
		let root = root.canonicalize().unwrap();
		let sub = root.join("sub");

		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = Watcher::new(1, tx, Duration::from_millis(20), Duration::from_millis(200), Duration::from_millis(50)).unwrap();
		watcher.watch(&sub).unwrap();

		// The kernel drops the watch with the directory, and nothing tells us.
		fs::remove_dir_all(&sub).unwrap();
		fs::create_dir(&sub).unwrap();
		while tokio::time::timeout(Duration::from_millis(400), rx.recv()).await.is_ok() {}

		watcher.watch(&sub).unwrap();
		fs::write(sub.join("after"), b"").unwrap();
		let mut heard = false;
		while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await {
			if matches!(&event, Event::FilesChanged { changes, .. } if changes.iter().any(|change| matches!(change, FsChange::Upsert { path, .. } if path == &sub.join("after")))) {
				heard = true;
				break;
			}
		}
		assert!(heard, "the recreated directory must deliver events again");
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn arming_a_watch_again_does_not_raise_events_of_its_own() {
		let root = std::env::temp_dir().join(format!("tuzi-watch-quiet-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = Watcher::new(1, tx, Duration::from_millis(20), Duration::from_millis(200), Duration::from_millis(50)).unwrap();
		for _ in 0..5 {
			watcher.watch(&root).unwrap();
		}
		assert!(tokio::time::timeout(Duration::from_millis(500), rx.recv()).await.is_err(), "re-arming alone must not look like a change");
		fs::write(root.join("real"), b"").unwrap();
		assert!(matches!(tokio::time::timeout(Duration::from_secs(3), rx.recv()).await, Ok(Some(Event::FilesChanged { .. }))), "and the watch still works afterwards");
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	/// A backend whose every `watch` fails the way hitting the OS limit does.
	struct RefusingWatcher;

	impl NotifyWatcher for RefusingWatcher {
		fn new<F: notify::EventHandler>(_: F, _: Config) -> notify::Result<Self> {
			Ok(Self)
		}
		fn watch(&mut self, _: &Path, _: RecursiveMode) -> notify::Result<()> {
			Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch))
		}
		fn unwatch(&mut self, _: &Path) -> notify::Result<()> {
			Ok(())
		}
		fn kind() -> notify::WatcherKind {
			notify::WatcherKind::NullWatcher
		}
	}

	#[tokio::test]
	async fn a_failure_of_watching_itself_is_reported() {
		let root = std::env::temp_dir().join(format!("tuzi-watch-limit-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = Watcher::with_inner(Box::new(RefusingWatcher), Arc::default(), Arc::default(), issue_reporter(1, tx));

		watcher.watch_async(root.clone());
		match next_issue(&mut rx, Duration::from_secs(2)).await {
			Some(WatchIssue::RegisterFailed { path, error }) => {
				assert_eq!(path, root);
				assert!(error.to_lowercase().contains("limit"), "the reason must survive: {error}");
			}
			other => panic!("expected RegisterFailed, got {other:?}"),
		}
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_directorys_own_access_problems_are_left_to_its_listing() {
		use std::os::unix::fs::PermissionsExt;

		let root = std::env::temp_dir().join(format!("tuzi-watch-own-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		let root = root.canonicalize().unwrap();
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = Watcher::new(1, tx, Duration::from_millis(20), Duration::from_millis(200), Duration::from_millis(50)).unwrap();

		watcher.watch_async(root.join("does-not-exist"));
		assert_eq!(next_issue(&mut rx, Duration::from_millis(500)).await, None, "a directory that is gone is not a watching failure");

		let locked = root.join("locked");
		fs::create_dir(&locked).unwrap();
		fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
		if fs::read_dir(&locked).is_err() {
			// Not root, so the permission really applies: the listing shows the
			// error on the node, and a second message here would be noise.
			watcher.watch_async(locked.clone());
			assert_eq!(next_issue(&mut rx, Duration::from_millis(500)).await, None, "unreadable is not a watching failure either");
		}
		fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}
}
