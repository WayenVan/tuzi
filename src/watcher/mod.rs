use std::{collections::{HashMap, HashSet}, io, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::Duration};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use tokio::{runtime::Handle, sync::mpsc::UnboundedSender, task::AbortHandle, time::Instant};

use crate::event::Event;

pub struct Watcher {
	inner:   Box<dyn NotifyWatcher + Send>,
	watched: Arc<Mutex<HashSet<PathBuf>>>,
	pending: PendingChanges,
}

const CHANGE_DEBOUNCE: Duration = Duration::from_millis(75);

type PendingChanges = Arc<Mutex<HashMap<PathBuf, PendingChange>>>;

struct PendingChange {
	deadline: Instant,
	abort:    Option<AbortHandle>,
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
		let watched = matched.lock().unwrap();
		let mut changed = HashSet::new();
		for path in event.paths {
			let path = path.canonicalize().unwrap_or(path);
			if let Some(dir) = nearest_watched(&watched, &path) {
				changed.insert(dir);
			}
		}
		drop(watched);
		for path in changed {
			debounce_changed(tab, tx.clone(), pending.clone(), &runtime, path, CHANGE_DEBOUNCE);
		}
	}
}

fn debounce_changed(
	tab: usize,
	tx: UnboundedSender<Event>,
	pending: PendingChanges,
	runtime: &Handle,
	path: PathBuf,
	delay: Duration,
) {
	let deadline = Instant::now() + delay;
	if let Some(change) = pending.lock().unwrap().get_mut(&path) {
		change.deadline = deadline;
		return;
	}
	pending.lock().unwrap().insert(path.clone(), PendingChange { deadline, abort: None });
	let task_path = path.clone();
	let task_pending = pending.clone();
	let handle = runtime.spawn(async move {
		loop {
			let Some(deadline) = task_pending.lock().unwrap().get(&task_path).map(|change| change.deadline) else { return };
			tokio::time::sleep_until(deadline).await;
			let send = {
				let mut pending = task_pending.lock().unwrap();
				if pending.get(&task_path).is_some_and(|change| change.deadline <= Instant::now()) {
					pending.remove(&task_path);
					true
				} else {
					false
				}
			};
			if send {
				let _ = tx.send(Event::Changed { tab, path: task_path });
				return;
			}
		}
	});
	if let Some(change) = pending.lock().unwrap().get_mut(&path) {
		change.abort = Some(handle.abort_handle());
	}
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
		let path = PathBuf::from("busy-directory");

		for _ in 0..5 {
			debounce_changed(3, tx.clone(), pending.clone(), &runtime, path.clone(), Duration::from_millis(20));
		}

		let event = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await.unwrap().unwrap();
		assert!(matches!(event, Event::Changed { tab: 3, path: ref changed } if changed == &path));
		assert!(tokio::time::timeout(Duration::from_millis(60), rx.recv()).await.is_err());
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
			Event::Changed { tab, path } => {
				assert_eq!(tab, 7);
				assert_eq!(path, dir);
			}
			_ => panic!("unexpected event"),
		}

		fs::remove_dir_all(&dir).unwrap();
	}
}
