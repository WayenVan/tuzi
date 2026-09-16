use std::{collections::HashSet, io, path::{Path, PathBuf}, sync::{Arc, Mutex}};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::mpsc::UnboundedSender;

use crate::event::Event;

pub struct Watcher {
	inner:   RecommendedWatcher,
	watched: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Watcher {
	pub fn new(tab: usize, tx: UnboundedSender<Event>) -> io::Result<Self> {
		let watched = Arc::new(Mutex::new(HashSet::new()));
		let matched = watched.clone();

		// The backend (FSEvents/inotify/...) doesn't consistently report a
		// changed child's path vs. its watched parent's, and may resolve
		// symlinks along the way — so instead of trusting `path.parent()`,
		// walk up from whatever it reports until we hit a directory we
		// actually watch.
		let inner = RecommendedWatcher::new(
			move |res: notify::Result<notify::Event>| {
				let Ok(event) = res else { return };
				let watched = matched.lock().unwrap();
				for path in event.paths {
					let path = path.canonicalize().unwrap_or(path);
					if let Some(dir) = nearest_watched(&watched, &path) {
						let _ = tx.send(Event::Changed { tab, path: dir });
					}
				}
			},
			Config::default(),
		)
		.map_err(io::Error::other)?;

		Ok(Self { inner, watched })
	}

	/// Watches one directory level, non-recursively — nodes watch themselves
	/// only while expanded, mirroring what's actually visible on screen.
	pub fn watch(&mut self, path: &Path) {
		let Ok(path) = path.canonicalize() else { return };
		if self.inner.watch(&path, RecursiveMode::NonRecursive).is_ok() {
			self.watched.lock().unwrap().insert(path);
		}
	}

	pub fn unwatch(&mut self, path: &Path) {
		let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
		let _ = self.inner.unwatch(&path);
		self.watched.lock().unwrap().remove(&path);
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
	use std::{fs, thread, time::Duration};

	use tokio::sync::mpsc;

	use super::*;

	#[tokio::test]
	async fn external_write_produces_a_changed_event() {
		let dir = std::env::temp_dir().join("tuzi-watcher-test");
		fs::create_dir_all(&dir).unwrap();
		let dir = dir.canonicalize().unwrap();

		let (tx, mut rx) = mpsc::unbounded_channel();
		let mut watcher = Watcher::new(7, tx).unwrap();
		watcher.watch(&dir);

		// give the OS watch a moment to actually arm before writing
		thread::sleep(Duration::from_millis(200));
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
