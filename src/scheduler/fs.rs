use std::{collections::HashMap, path::{Path, PathBuf}, sync::Arc};

use tokio::sync::mpsc::UnboundedSender;

use crate::{event::Event, fs::{self, Engine}};

#[derive(Default)]
struct Entry {
	busy:  Option<u64>,
	dirty: bool,
}

pub struct FsScheduler {
	tab:     usize,
	tx:      UnboundedSender<Event>,
	engine:  Arc<dyn Engine>,
	entries: HashMap<PathBuf, Entry>,
	next:    u64,
}

impl FsScheduler {
	pub fn new(tab: usize, tx: UnboundedSender<Event>, engine: Arc<dyn Engine>) -> Self {
		Self { tab, tx, engine, entries: HashMap::new(), next: 0 }
	}

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

	pub fn move_paths(&self, sources: Vec<PathBuf>, target_dir: PathBuf) {
		let tab = self.tab;
		let tx = self.tx.clone();
		let dir = target_dir.clone();
		tokio::spawn(async move {
			tokio::task::spawn_blocking(move || {
				for src in &sources {
					let Some(name) = src.file_name() else { continue };
					let dest = fs::unique_dest(&dir, name);
					if std::fs::rename(src, &dest).is_err() && fs::copy_recursive(src, &dest).is_ok() {
						let _ = fs::remove(src);
					}
				}
			})
			.await
			.ok();
			let _ = tx.send(Event::Pasted { tab, target: target_dir });
		});
	}

	pub fn create(&self, base: PathBuf, value: String) {
		let tab = self.tab;
		let tx = self.tx.clone();
		let directory = value.ends_with('/') || value.ends_with('\\');
		let target = base.join(&value);
		let task_target = target.clone();
		let task_value = value.clone();
		let task_base = base.clone();
		tokio::spawn(async move {
			let result = tokio::task::spawn_blocking(move || {
				if task_value.is_empty() {
					return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "name cannot be empty"));
				}
				if directory {
					std::fs::create_dir_all(&task_target)
				} else {
					let Some(parent) = task_target.parent() else {
						return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "file has no parent directory"));
					};
					std::fs::create_dir_all(parent)?;
					std::fs::OpenOptions::new().write(true).create_new(true).open(&task_target).map(drop)
				}
			})
			.await
			.unwrap_or_else(|error| Err(std::io::Error::other(error)));
			let _ = tx.send(Event::Created { tab, base: task_base, value, target, result });
		});
	}
}
