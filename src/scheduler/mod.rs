use std::{collections::HashMap, path::{Path, PathBuf}, sync::Arc};

use tokio::sync::mpsc::UnboundedSender;

use crate::{event::Event, fs::{self, Engine}};

#[derive(Default)]
struct Entry {
	busy:  Option<u64>,
	dirty: bool,
}

pub struct Scheduler {
	tab:     usize,
	tx:      UnboundedSender<Event>,
	engine:  Arc<dyn Engine>,
	entries: HashMap<PathBuf, Entry>,
	next:    u64,
}

impl Scheduler {
	pub fn new(tab: usize, tx: UnboundedSender<Event>, engine: Arc<dyn Engine>) -> Self {
		Self { tab, tx, engine, entries: HashMap::new(), next: 0 }
	}

	/// Requests a fresh listing for `path`. If one's already in flight, just
	/// marks it dirty so a follow-up fetch fires the moment this one lands —
	/// callers never need to worry about piling up duplicate reads for the
	/// same directory during a burst of changes.
	pub fn refresh(&mut self, path: PathBuf) {
		let entry = self.entries.entry(path.clone()).or_default();
		if entry.busy.is_some() {
			entry.dirty = true;
			return;
		}
		self.spawn(path);
	}

	fn spawn(&mut self, path: PathBuf) {
		let ticket = self.next;
		self.next += 1;
		self.entries.entry(path.clone()).or_default().busy = Some(ticket);

		let tab = self.tab;
		let engine = self.engine.clone();
		let tx = self.tx.clone();
		let target = path.clone();
		tokio::spawn(async move {
			let result = tokio::task::spawn_blocking(move || engine.read_dir(&target)).await.expect("read_dir task panicked");
			let _ = tx.send(Event::Loaded { tab, path, ticket, result });
		});
	}

	/// Call when a `Loaded` event arrives. Returns whether it's still
	/// current — `false` means a newer request superseded it, so the
	/// listing it carries should be discarded rather than applied.
	pub fn accept(&mut self, path: &Path, ticket: u64) -> bool {
		let Some(entry) = self.entries.get_mut(path) else { return false };
		if entry.busy != Some(ticket) {
			return false;
		}
		entry.busy = None;
		if std::mem::take(&mut entry.dirty) {
			self.spawn(path.to_path_buf());
		}
		true
	}

	/// Stops tracking a path — call on collapse/delete so a directory
	/// nobody's watching anymore doesn't keep a stale entry around.
	pub fn forget(&mut self, path: &Path) { self.entries.remove(path); }

	pub fn delete(&self, paths: Vec<PathBuf>) {
		let tab = self.tab;
		let tx = self.tx.clone();
		let targets = paths.clone();
		tokio::spawn(async move {
			tokio::task::spawn_blocking(move || {
				for path in &targets {
					let _ = fs::remove(path);
				}
			})
			.await
			.ok();
			let _ = tx.send(Event::Deleted { tab, paths });
		});
	}

	pub fn copy(&self, sources: Vec<PathBuf>, target_dir: PathBuf) {
		let tab = self.tab;
		let tx = self.tx.clone();
		let dir = target_dir.clone();
		tokio::spawn(async move {
			tokio::task::spawn_blocking(move || {
				for src in &sources {
					let Some(name) = src.file_name() else { continue };
					let dest = fs::unique_dest(&dir, name);
					let _ = fs::copy_recursive(src, &dest);
				}
			})
			.await
			.ok();
			let _ = tx.send(Event::Pasted { tab, target: target_dir });
		});
	}
}
