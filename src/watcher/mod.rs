use std::{collections::HashSet, io, path::{Path, PathBuf}, sync::{Arc, Mutex}};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use tokio::sync::mpsc::UnboundedSender;

use crate::event::Event;

pub struct Watcher {
	inner:   Box<dyn NotifyWatcher + Send>,
	watched: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Watcher {
	pub fn new(tab: usize, tx: UnboundedSender<Event>) -> io::Result<Self> {
		let watched = Arc::new(Mutex::new(HashSet::new()));
		let inner = RecommendedWatcher::new(event_handler(tab, tx, watched.clone()), Config::default()).map_err(io::Error::other)?;
		Ok(Self { inner: Box::new(inner), watched })
	}

	#[cfg(test)]
	fn new_polling(tab: usize, tx: UnboundedSender<Event>, interval: std::time::Duration) -> io::Result<Self> {
		let watched = Arc::new(Mutex::new(HashSet::new()));
		let config = Config::default().with_poll_interval(interval).with_compare_contents(true);
		let inner = notify::PollWatcher::new(event_handler(tab, tx, watched.clone()), config).map_err(io::Error::other)?;
		Ok(Self { inner: Box::new(inner), watched })
	}

	/// Watches one directory level, non-recursively — nodes watch themselves
	/// only while expanded, mirroring what's actually visible on screen.
	pub fn watch(&mut self, path: &Path) -> io::Result<()> {
		let path = path.canonicalize()?;
		self.inner.watch(&path, RecursiveMode::NonRecursive).map_err(io::Error::other)?;
		self.watched.lock().unwrap().insert(path);
		Ok(())
	}

	pub fn unwatch(&mut self, path: &Path) {
		let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
		let _ = self.inner.unwatch(&path);
		self.watched.lock().unwrap().remove(&path);
	}
}

// The backend (FSEvents/inotify/...) doesn't consistently report a changed
// child's path vs. its watched parent, and may resolve symlinks along the way.
// Walk upward until reaching a directory registered by this watcher.
fn event_handler(
	tab: usize,
	tx: UnboundedSender<Event>,
	matched: Arc<Mutex<HashSet<PathBuf>>>,
) -> impl FnMut(notify::Result<notify::Event>) + Send + 'static {
	move |res| {
		let Ok(event) = res else { return };
		let watched = matched.lock().unwrap();
		for path in event.paths {
			let path = path.canonicalize().unwrap_or(path);
			if let Some(dir) = nearest_watched(&watched, &path) {
				let _ = tx.send(Event::Changed { tab, path: dir });
			}
		}
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
