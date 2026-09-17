use std::{
	collections::HashMap,
	path::{Path, PathBuf},
	sync::Arc,
};

use tokio::sync::mpsc::UnboundedSender;

use crate::{
	event::Event,
	fs::{Engine, create_symlink, symlink_target, unique_dest_avoiding},
};

const FIRST_LISTING_BATCH_SIZE: usize = 64;
const LISTING_BATCH_SIZE: usize = 512;

#[derive(Default)]
struct Entry {
	busy: Option<u64>,
	dirty: bool,
}

pub struct FsScheduler {
	tab: usize,
	tx: UnboundedSender<Event>,
	engine: Arc<dyn Engine>,
	entries: HashMap<PathBuf, Entry>,
	next: u64,
}

impl FsScheduler {
	pub fn new(tab: usize, tx: UnboundedSender<Event>, engine: Arc<dyn Engine>) -> Self {
		Self {
			tab,
			tx,
			engine,
			entries: HashMap::new(),
			next: 0,
		}
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
			let chunk_tx = tx.clone();
			let chunk_path = path.clone();
			let result = tokio::task::spawn_blocking(move || {
				engine.read_dir_batches(&target, FIRST_LISTING_BATCH_SIZE, LISTING_BATCH_SIZE, &mut |entries| {
					chunk_tx
						.send(Event::Loaded {
							tab,
							path: chunk_path.clone(),
							ticket,
							result: Ok(entries),
							done: false,
						})
						.is_ok()
				})
			})
			.await
			.unwrap_or_else(|error| Err(std::io::Error::other(error)))
			.map(|()| Vec::new());
			let _ = tx.send(Event::Loaded { tab, path, ticket, result, done: true });
		});
	}

	pub fn accept(&mut self, path: &Path, ticket: u64, done: bool) -> bool {
		let Some(entry) = self.entries.get_mut(path) else {
			return false;
		};
		if entry.busy != Some(ticket) {
			return false;
		}
		if !done {
			return true;
		}
		entry.busy = None;
		if std::mem::take(&mut entry.dirty) {
			self.spawn(path.to_path_buf());
		}
		true
	}

	pub fn forget(&mut self, path: &Path) {
		self.entries.remove(path);
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

	/// Creates one symlink per source, pointing at it directly (not at
	/// whatever it itself might resolve through) — instant enough that it
	/// doesn't need the queued/cancelable machinery `TaskManager` gives
	/// copy/move.
	pub fn link(&self, sources: Vec<PathBuf>, target_dir: PathBuf, absolute: bool) {
		let tab = self.tab;
		let tx = self.tx.clone();
		let task_target_dir = target_dir.clone();
		tokio::spawn(async move {
			let result = tokio::task::spawn_blocking(move || {
				for source in &sources {
					let Some(name) = source.file_name() else {
						continue;
					};
					let dest = unique_dest_avoiding(&task_target_dir, name, |_| false);
					let content = symlink_target(&task_target_dir, source, absolute);
					let is_dir = std::fs::metadata(source).is_ok_and(|meta| meta.is_dir());
					create_symlink(&content, &dest, is_dir)?;
				}
				Ok(())
			})
			.await
			.unwrap_or_else(|error| Err(std::io::Error::other(error)));
			let _ = tx.send(Event::Linked { tab, target: target_dir, result });
		});
	}
}

#[cfg(test)]
mod tests {
	use std::{
		sync::atomic::{AtomicUsize, Ordering},
		time::Duration,
	};

	use super::*;
	use crate::fs::Cha;

	struct CountingEngine(AtomicUsize);

	impl Engine for CountingEngine {
		fn read_dir(&self, _path: &Path) -> std::io::Result<Vec<(PathBuf, Cha)>> {
			self.0.fetch_add(1, Ordering::Relaxed);
			Ok(Vec::new())
		}
	}

	struct LargeEngine;

	impl Engine for LargeEngine {
		fn read_dir(&self, path: &Path) -> std::io::Result<Vec<(PathBuf, Cha)>> {
			Ok((0..1_200)
				.map(|index| {
					(
						path.join(format!("file-{index}")),
						Cha {
							len: 0,
							is_dir: false,
							is_link: false,
							link_target: None,
							link_broken: false,
							modified: None,
							mode: 0,
						},
					)
				})
				.collect())
		}
	}

	#[tokio::test]
	async fn large_listings_arrive_in_bounded_batches_before_done() {
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
		let mut scheduler = FsScheduler::new(2, tx, Arc::new(LargeEngine));
		let path = PathBuf::from("large-directory");
		scheduler.refresh(path.clone());

		let mut sizes = Vec::new();
		loop {
			let Event::Loaded { ticket, result, done, .. } = rx.recv().await.unwrap() else { panic!("expected listing") };
			assert!(scheduler.accept(&path, ticket, done));
			if done {
				assert!(result.is_ok());
				break;
			}
			sizes.push(result.unwrap().len());
		}

		assert_eq!(sizes, [64, 512, 512, 112]);
	}

	#[tokio::test]
	async fn changes_while_busy_collapse_into_one_follow_up_refresh() {
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
		let engine = Arc::new(CountingEngine(AtomicUsize::new(0)));
		let mut scheduler = FsScheduler::new(4, tx, engine.clone());
		let path = PathBuf::from("busy-directory");

		scheduler.refresh(path.clone());
		for _ in 0..5 {
			scheduler.refresh(path.clone());
		}

		let first = rx.recv().await.unwrap();
		let Event::Loaded { ticket, .. } = first else { panic!("expected listing") };
		assert!(scheduler.accept(&path, ticket, true));

		let second = rx.recv().await.unwrap();
		let Event::Loaded { ticket, .. } = second else { panic!("expected follow-up listing") };
		assert!(scheduler.accept(&path, ticket, true));
		assert_eq!(engine.0.load(Ordering::Relaxed), 2);
		assert!(tokio::time::timeout(Duration::from_millis(30), rx.recv()).await.is_err());
	}
}
