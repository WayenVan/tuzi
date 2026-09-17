use std::{collections::{HashMap, HashSet}, io, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::Duration};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use tokio::{runtime::Handle, sync::mpsc::UnboundedSender, task::AbortHandle, time::Instant};

use crate::{event::Event, fs::{Cha, FsChange}};

pub struct Watcher {
	inner:   Box<dyn NotifyWatcher + Send>,
	watched: Arc<Mutex<HashSet<PathBuf>>>,
	pending: PendingChanges,
}

const CHANGE_DEBOUNCE: Duration = Duration::from_millis(250);
const MAX_CHANGE_BATCH: usize = 1000;
/// Caps how long a directory under nonstop churn (e.g. a log file being
/// appended to faster than `CHANGE_DEBOUNCE` apart) can be starved of an
/// update — the sliding debounce below would otherwise keep resetting
/// forever.
const MAX_CHANGE_WAIT: Duration = Duration::from_secs(1);

type PendingChanges = Arc<Mutex<HashMap<PathBuf, PendingChange>>>;

struct PendingChange {
	first_seen: Instant,
	deadline:   Instant,
	paths:      HashSet<PathBuf>,
	refresh:    bool,
	abort:      Option<AbortHandle>,
}

struct ChangeSet {
	parent:  PathBuf,
	paths:   HashSet<PathBuf>,
	refresh: bool,
}

impl Watcher {
	pub fn new(tab: usize, tx: UnboundedSender<Event>) -> io::Result<Self> {
		let watched = Arc::new(Mutex::new(HashSet::new()));
		let pending = Arc::new(Mutex::new(HashMap::new()));
		let runtime = Handle::try_current().map_err(io::Error::other)?;
		let inner = RecommendedWatcher::new(event_handler(tab, tx, watched.clone(), pending.clone(), runtime), Config::default()).map_err(io::Error::other)?;
		Ok(Self { inner: Box::new(inner), watched, pending })
	}

	#[cfg(test)]
	fn new_polling(tab: usize, tx: UnboundedSender<Event>, interval: std::time::Duration) -> io::Result<Self> {
		let watched = Arc::new(Mutex::new(HashSet::new()));
		let pending = Arc::new(Mutex::new(HashMap::new()));
		let runtime = Handle::try_current().map_err(io::Error::other)?;
		let config = Config::default().with_poll_interval(interval).with_compare_contents(true);
		let inner = notify::PollWatcher::new(event_handler(tab, tx, watched.clone(), pending.clone(), runtime), config).map_err(io::Error::other)?;
		Ok(Self { inner: Box::new(inner), watched, pending })
	}

	/// Watches one directory level, non-recursively — nodes watch themselves
	/// only while expanded, mirroring what's actually visible on screen.
	pub fn watch(&mut self, path: &Path) -> io::Result<()> {
		let path = path.canonicalize()?;
		if self.watched.lock().unwrap().contains(&path) {
			return Ok(());
		}
		self.inner.watch(&path, RecursiveMode::NonRecursive).map_err(io::Error::other)?;
		self.watched.lock().unwrap().insert(path);
		Ok(())
	}

	pub fn unwatch(&mut self, path: &Path) {
		let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
		let _ = self.inner.unwatch(&path);
		self.watched.lock().unwrap().remove(&path);
		if let Some(change) = self.pending.lock().unwrap().remove(&path)
			&& let Some(abort) = change.abort
		{
			abort.abort();
		}
	}
}

impl Drop for Watcher {
	fn drop(&mut self) {
		for (_, change) in self.pending.lock().unwrap().drain() {
			if let Some(abort) = change.abort {
				abort.abort();
			}
		}
	}
}

// The backend (FSEvents/inotify/...) doesn't consistently report a changed
// child's path vs. its watched parent, and may resolve symlinks along the way.
// Walk upward until reaching a directory registered by this watcher.
fn event_handler(
	tab: usize,
	tx: UnboundedSender<Event>,
	matched: Arc<Mutex<HashSet<PathBuf>>>,
	pending: PendingChanges,
	runtime: Handle,
) -> impl FnMut(notify::Result<notify::Event>) + Send + 'static {
	move |res| {
		let Ok(event) = res else { return };
		if event.kind.is_access() {
			return;
		}
		let watched = matched.lock().unwrap();
		let mut changed: HashMap<PathBuf, (HashSet<PathBuf>, bool)> = HashMap::new();
		for path in event.paths {
			if let Some(dir) = nearest_watched(&watched, &path) {
				let entry = changed.entry(dir.clone()).or_default();
				if path == dir {
					entry.1 = true;
				} else if let Ok(relative) = path.strip_prefix(&dir)
					&& let Some(component) = relative.components().next()
				{
					entry.0.insert(dir.join(component.as_os_str()));
				}
			}
		}
		drop(watched);
		for (parent, (paths, refresh)) in changed {
			debounce_changes(tab, tx.clone(), pending.clone(), &runtime, ChangeSet { parent, paths, refresh }, CHANGE_DEBOUNCE, MAX_CHANGE_WAIT);
		}
	}
}

fn debounce_changes(
	tab: usize,
	tx: UnboundedSender<Event>,
	pending: PendingChanges,
	runtime: &Handle,
	change_set: ChangeSet,
	delay: Duration,
	max_wait: Duration,
) {
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
	pending.lock().unwrap().insert(parent.clone(), PendingChange { first_seen: now, deadline, paths, refresh, abort: None });
	let task_parent = parent.clone();
	let task_pending = pending.clone();
	let handle = runtime.spawn(async move {
		loop {
			let Some(deadline) = task_pending.lock().unwrap().get(&task_parent).map(|change| change.deadline) else { return };
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
		.filter_map(|path| match std::fs::metadata(&path) {
			Ok(metadata) => Some(FsChange::Upsert { path, cha: Cha::from(metadata) }),
			Err(error) if error.kind() == io::ErrorKind::NotFound => Some(FsChange::Delete { path }),
			Err(_) => None,
		})
		.collect()
}

fn nearest_watched(watched: &HashSet<PathBuf>, path: &Path) -> Option<PathBuf> {
	let mut cur = Some(path);
	while let Some(p) = cur {
		if watched.contains(p) {
			return Some(p.to_path_buf());
		}
		cur = p.parent();
	}
	None
}

#[cfg(test)]
mod tests {
	use std::{fs, time::{Duration, SystemTime, UNIX_EPOCH}};

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
				ChangeSet { parent: parent.clone(), paths: HashSet::from([path.clone()]), refresh: false },
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
					ChangeSet { parent: churn_parent.clone(), paths: HashSet::from([churn_path.clone()]), refresh: false },
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
		let mut watcher = Watcher::new_polling(7, tx, Duration::from_millis(25)).unwrap();
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
}
