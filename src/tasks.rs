use std::{collections::HashSet, fs, io::{self, Read, Write}, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};

use tokio::{sync::{Semaphore, mpsc::UnboundedSender, watch}, task::JoinHandle};

use crate::{config::{ConflictPolicy, Tasks as TaskConfig}, event::Event, fs::{format_size, remove, unique_dest_avoiding}};

#[path = "tasks/trash.rs"]
mod trash_backend;
use trash_backend::{SystemTrash, TrashBackend};

pub type TaskId = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskKind { Copy, Move, Trash, Delete }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState { Queued, Blocked, Scanning, Running, Canceling, Failed }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DependencyState { Pending, Succeeded, Blocked }

pub struct Task {
	pub id: TaskId,
	pub kind: TaskKind,
	pub title: String,
	pub state: TaskState,
	pub files_total: u64,
	pub files_done: u64,
	pub bytes_total: u64,
	pub bytes_done: u64,
	pub error: Option<String>,
	cancel: Arc<AtomicBool>,
	handle: Option<JoinHandle<()>>,
	reserved_target: Option<PathBuf>,
	dependency: watch::Sender<DependencyState>,
}

impl Task {
	pub fn percent(&self) -> f64 {
		if self.bytes_total > 0 { self.bytes_done as f64 * 100.0 / self.bytes_total as f64 }
		else if self.files_total > 0 { self.files_done as f64 * 100.0 / self.files_total as f64 }
		else { 0.0 }
	}

	pub fn detail(&self) -> String {
		match self.state {
			TaskState::Queued => "Waiting…".into(),
			TaskState::Blocked => self.error.clone().unwrap_or_else(|| "Blocked by a child task".into()),
			TaskState::Scanning => "Scanning…".into(),
			TaskState::Canceling => "Canceling…".into(),
			TaskState::Failed => self.error.clone().unwrap_or_else(|| "Failed".into()),
			// Trash hands the whole item to the OS's own trash implementation
			// in one call, with no file/byte counts to report; Delete scans
			// and removes one file at a time, but "bytes deleted" isn't a
			// meaningful number the way "bytes copied" is, so it only shows
			// a file count.
			TaskState::Running => match self.kind {
				TaskKind::Trash => "Moving to Trash… (cannot cancel)".into(),
				TaskKind::Delete => format!("{}/{} files", self.files_done, self.files_total),
				TaskKind::Copy | TaskKind::Move => {
					format!("{}/{} files  {} / {}", self.files_done, self.files_total, format_size(self.bytes_done), format_size(self.bytes_total))
				}
			},
		}
	}
}

pub enum TaskEvent {
	Scanning(TaskId),
	Started { id: TaskId, files: u64, bytes: u64 },
	Progress { id: TaskId, files: u64, bytes: u64 },
	Blocked { id: TaskId, reason: String },
	/// `subject` is the path the tab should react to once this task is
	/// gone: the destination directory to refresh for a copy or move, or
	/// the original item for a trash or delete (so the caller can run the
	/// same watcher/cache/selection cleanup a directly-deleted path needs).
	Finished { id: TaskId, tab: usize, subject: PathBuf, outcome: TaskOutcome },
}

pub enum TaskOutcome { Succeeded, Canceled { changed: bool }, Failed(String) }

pub struct TaskManager {
	pub visible: bool,
	pub cursor: usize,
	pub tasks: Vec<Task>,
	next_id: TaskId,
	tx: UnboundedSender<Event>,
	permits: Arc<Semaphore>,
	reserved_targets: HashSet<PathBuf>,
	trash: Arc<dyn TrashBackend>,
	config: TaskConfig,
}


impl TaskManager {
	#[cfg(test)]
	pub fn new(tx: UnboundedSender<Event>) -> Self {
		Self::configured(tx, crate::config::Config::default().tasks)
	}

	pub fn configured(tx: UnboundedSender<Event>, config: TaskConfig) -> Self {
		Self::with_trash_and_config(tx, Arc::new(SystemTrash), config)
	}

	#[cfg(test)]
	fn with_trash(tx: UnboundedSender<Event>, trash: Arc<dyn TrashBackend>) -> Self {
		Self::with_trash_and_config(tx, trash, crate::config::Config::default().tasks)
	}

	fn with_trash_and_config(tx: UnboundedSender<Event>, trash: Arc<dyn TrashBackend>, config: TaskConfig) -> Self {
		Self { visible: false, cursor: 0, tasks: Vec::new(), next_id: 0, tx, permits: Arc::new(Semaphore::new(config.workers)), reserved_targets: HashSet::new(), trash, config }
	}

	#[cfg(test)]
	pub fn enqueue(&mut self, sources: Vec<PathBuf>, target_dir: PathBuf, cut: bool, tab: usize) {
		self.enqueue_with_policy(sources, target_dir, cut, tab, ConflictPolicy::Rename);
	}

	pub fn enqueue_with_policy(&mut self, sources: Vec<PathBuf>, target_dir: PathBuf, cut: bool, tab: usize, policy: ConflictPolicy) -> usize {
		if policy == ConflictPolicy::Error {
			let conflicts = sources.iter().filter_map(|source| source.file_name()).map(|name| target_dir.join(name)).filter(|target| target.exists() || self.reserved_targets.contains(target)).count();
			if conflicts > 0 { return conflicts; }
		}
		let dependencies = dependency_channels(&sources, cut);
		for (source, (dependency, waits)) in sources.into_iter().zip(dependencies) {
			let Some(name) = source.file_name() else { continue };
			let direct = target_dir.join(name);
			let target = match policy {
				ConflictPolicy::Rename => unique_dest_avoiding(&target_dir, name, |path| self.reserved_targets.contains(path)),
				ConflictPolicy::Error => direct,
			};
			self.reserved_targets.insert(target.clone());
			let id = self.next_id;
			self.next_id += 1;
			let kind = if cut { TaskKind::Move } else { TaskKind::Copy };
			let verb = if cut { "Move" } else { "Copy" };
			let title = format!("{verb} {} → {}", source.display(), target.display());
			let cancel = Arc::new(AtomicBool::new(false));
			let handle = self.spawn(id, tab, source, target.clone(), target_dir.clone(), cut, cancel.clone(), waits);
			self.tasks.push(Task {
				id, kind, title, state: TaskState::Queued, files_total: 0, files_done: 0,
				bytes_total: 0, bytes_done: 0, error: None, cancel, handle: Some(handle), reserved_target: Some(target), dependency,
			});
		}
		0
	}

	#[allow(clippy::too_many_arguments)]
	fn spawn(&self, id: TaskId, tab: usize, source: PathBuf, target: PathBuf, refresh_dir: PathBuf, cut: bool, cancel: Arc<AtomicBool>, waits: Vec<watch::Receiver<DependencyState>>) -> JoinHandle<()> {
		let tx = self.tx.clone();
		let permits = self.permits.clone();
		let config = self.config.clone();
		tokio::spawn(async move {
			if !wait_dependencies(id, waits, &tx).await { return }
			let Ok(_permit) = permits.acquire_owned().await else { return };
			if canceled(&cancel) {
				let _ = tx.send(Event::Task(TaskEvent::Finished { id, tab, subject: refresh_dir, outcome: TaskOutcome::Canceled { changed: false } }));
				return;
			}
			let _ = tx.send(Event::Task(TaskEvent::Scanning(id)));
			let worker_tx = tx.clone();
			let finish_target = refresh_dir;
			let outcome = outcome(tokio::task::spawn_blocking(move || run_task(id, &source, &target, cut, &cancel, &worker_tx, &config)).await, true);
			let _ = tx.send(Event::Task(TaskEvent::Finished { id, tab, subject: finish_target, outcome }));
		})
	}

	/// Moves each top-level source to the OS trash, one task per source —
	/// same fan-out as `enqueue`, but there's no destination to collide on
	/// and the OS trash implementation handles everything atomically, so
	/// there's nothing to scan or report progress on.
	pub fn enqueue_trash(&mut self, sources: Vec<PathBuf>, tab: usize) {
		let dependencies = dependency_channels(&sources, true);
		for (source, (dependency, waits)) in sources.into_iter().zip(dependencies) {
			let id = self.next_id;
			self.next_id += 1;
			let title = format!("Trash {}", source.display());
			let cancel = Arc::new(AtomicBool::new(false));
			let handle = self.spawn_trash(id, tab, source.clone(), cancel.clone(), waits);
			self.tasks.push(Task {
				id, kind: TaskKind::Trash, title, state: TaskState::Queued, files_total: 0, files_done: 0,
				bytes_total: 0, bytes_done: 0, error: None, cancel, handle: Some(handle), reserved_target: None, dependency,
			});
		}
	}

	fn spawn_trash(&self, id: TaskId, tab: usize, source: PathBuf, cancel: Arc<AtomicBool>, waits: Vec<watch::Receiver<DependencyState>>) -> JoinHandle<()> {
		let tx = self.tx.clone();
		let permits = self.permits.clone();
		let trash = self.trash.clone();
		tokio::spawn(async move {
			if !wait_dependencies(id, waits, &tx).await { return }
			let Ok(_permit) = permits.acquire_owned().await else { return };
			if canceled(&cancel) {
				let _ = tx.send(Event::Task(TaskEvent::Finished { id, tab, subject: source, outcome: TaskOutcome::Canceled { changed: false } }));
				return;
			}
			let _ = tx.send(Event::Task(TaskEvent::Started { id, files: 0, bytes: 0 }));
			let finish_subject = source.clone();
			let outcome = match tokio::task::spawn_blocking(move || trash.delete(&source)).await {
				Ok(Ok(())) => TaskOutcome::Succeeded,
				Ok(Err(error)) => TaskOutcome::Failed(error),
				Err(error) => TaskOutcome::Failed(error.to_string()),
			};
			let _ = tx.send(Event::Task(TaskEvent::Finished { id, tab, subject: finish_subject, outcome }));
		})
	}

	/// Permanently removes each top-level source, one task per source, with
	/// the same scan-then-walk shape `run_task`'s copy/move side uses.
	pub fn enqueue_delete(&mut self, sources: Vec<PathBuf>, tab: usize) {
		let dependencies = dependency_channels(&sources, true);
		for (source, (dependency, waits)) in sources.into_iter().zip(dependencies) {
			let id = self.next_id;
			self.next_id += 1;
			let title = format!("Delete {}", source.display());
			let cancel = Arc::new(AtomicBool::new(false));
			let handle = self.spawn_delete(id, tab, source.clone(), cancel.clone(), waits);
			self.tasks.push(Task {
				id, kind: TaskKind::Delete, title, state: TaskState::Queued, files_total: 0, files_done: 0,
				bytes_total: 0, bytes_done: 0, error: None, cancel, handle: Some(handle), reserved_target: None, dependency,
			});
		}
	}

	fn spawn_delete(&self, id: TaskId, tab: usize, source: PathBuf, cancel: Arc<AtomicBool>, waits: Vec<watch::Receiver<DependencyState>>) -> JoinHandle<()> {
		let tx = self.tx.clone();
		let permits = self.permits.clone();
		let progress_interval = Duration::from_millis(self.config.progress_interval_ms);
		tokio::spawn(async move {
			if !wait_dependencies(id, waits, &tx).await { return }
			let Ok(_permit) = permits.acquire_owned().await else { return };
			if canceled(&cancel) {
				let _ = tx.send(Event::Task(TaskEvent::Finished { id, tab, subject: source, outcome: TaskOutcome::Canceled { changed: false } }));
				return;
			}
			let _ = tx.send(Event::Task(TaskEvent::Scanning(id)));
			let worker_tx = tx.clone();
			let finish_subject = source.clone();
			let outcome = outcome(tokio::task::spawn_blocking(move || run_delete(id, &source, &cancel, &worker_tx, progress_interval)).await, true);
			let _ = tx.send(Event::Task(TaskEvent::Finished { id, tab, subject: finish_subject, outcome }));
		})
	}

	pub fn accept(&mut self, event: TaskEvent) -> Option<(usize, TaskKind, PathBuf)> {
		match event {
			TaskEvent::Scanning(id) => self.find_mut(id)?.state = TaskState::Scanning,
			TaskEvent::Started { id, files, bytes } => {
				let task = self.find_mut(id)?;
				task.state = TaskState::Running;
				task.files_total = files;
				task.bytes_total = bytes;
			}
			TaskEvent::Progress { id, files, bytes } => {
				let task = self.find_mut(id)?;
				task.files_done = files;
				task.bytes_done = bytes;
			}
			TaskEvent::Blocked { id, reason } => {
				let task = self.find_mut(id)?;
				task.state = TaskState::Blocked;
				task.error = Some(reason);
				task.handle = None;
			}
			TaskEvent::Finished { id, tab, subject, outcome } => {
				let kind = self.find_mut(id)?.kind;
				match outcome {
					TaskOutcome::Succeeded => {
						self.find_mut(id)?.dependency.send_replace(DependencyState::Succeeded);
						self.remove(id);
						return Some((tab, kind, subject));
					}
					TaskOutcome::Canceled { changed } => {
						self.find_mut(id)?.dependency.send_replace(DependencyState::Blocked);
						self.remove(id);
						if changed { return Some((tab, kind, subject)); }
					}
					TaskOutcome::Failed(error) => {
						let task = self.find_mut(id)?;
						task.dependency.send_replace(DependencyState::Blocked);
						task.state = TaskState::Failed;
						task.error = Some(error);
						task.handle = None;
					}
				}
			}
		}
		self.clamp_cursor();
		None
	}

	pub fn move_cursor(&mut self, delta: isize) {
		if self.tasks.is_empty() { self.cursor = 0; return }
		self.cursor = (self.cursor as isize + delta).clamp(0, self.tasks.len() as isize - 1) as usize;
	}

	pub fn cancel_selected(&mut self) {
		let Some(task) = self.tasks.get_mut(self.cursor) else { return };
		if matches!(task.state, TaskState::Failed | TaskState::Blocked) {
			let id = task.id;
			task.dependency.send_replace(DependencyState::Blocked);
			self.remove(id);
			return;
		}
		if task.kind == TaskKind::Trash && task.state != TaskState::Queued { return }
		task.cancel.store(true, Ordering::Relaxed);
		if task.state == TaskState::Queued {
			task.dependency.send_replace(DependencyState::Blocked);
			if let Some(handle) = task.handle.take() { handle.abort(); }
			let id = task.id;
			self.remove(id);
		} else {
			task.state = TaskState::Canceling;
		}
	}

	pub fn summary(&self) -> Option<(usize, f64)> {
		let active: Vec<_> = self.tasks.iter().filter(|t| t.state != TaskState::Failed).collect();
		(!active.is_empty()).then(|| (active.len(), active.iter().map(|t| t.percent()).sum::<f64>() / active.len() as f64))
	}

	fn find_mut(&mut self, id: TaskId) -> Option<&mut Task> { self.tasks.iter_mut().find(|task| task.id == id) }
	fn remove(&mut self, id: TaskId) {
		if let Some(index) = self.tasks.iter().position(|task| task.id == id)
			&& let Some(target) = self.tasks.remove(index).reserved_target {
			self.reserved_targets.remove(&target);
		}
		self.clamp_cursor();
	}
	fn clamp_cursor(&mut self) { self.cursor = self.cursor.min(self.tasks.len().saturating_sub(1)); }
}

fn dependency_channels(sources: &[PathBuf], ordered: bool) -> Vec<(watch::Sender<DependencyState>, Vec<watch::Receiver<DependencyState>>)> {
	let signals: Vec<_> = sources.iter().map(|_| watch::channel(DependencyState::Pending).0).collect();
	sources.iter().enumerate().map(|(ancestor, path)| {
		let waits = if ordered {
			sources.iter().enumerate()
				.filter(|(child, candidate)| *child != ancestor && candidate.starts_with(path))
				.map(|(child, _)| signals[child].subscribe()).collect()
		} else { Vec::new() };
		(signals[ancestor].clone(), waits)
	}).collect()
}

async fn wait_dependencies(id: TaskId, waits: Vec<watch::Receiver<DependencyState>>, tx: &UnboundedSender<Event>) -> bool {
	for mut wait in waits {
		loop {
			match *wait.borrow() {
				DependencyState::Succeeded => break,
				DependencyState::Blocked => {
					let _ = tx.send(Event::Task(TaskEvent::Blocked { id, reason: "Blocked by a failed or canceled child task".into() }));
					return false;
				}
				DependencyState::Pending => {},
			}
			if wait.changed().await.is_err() { return false }
		}
	}
	true
}

fn run_task(id: TaskId, source: &Path, target: &Path, cut: bool, cancel: &AtomicBool, tx: &UnboundedSender<Event>, config: &TaskConfig) -> io::Result<()> {
	let (files, bytes) = scan(source, cancel)?;
	let _ = tx.send(Event::Task(TaskEvent::Started { id, files, bytes }));
	let mut progress = Counters::new(Duration::from_millis(config.progress_interval_ms));
	if cut && fs::rename(source, target).is_ok() {
		let _ = tx.send(Event::Task(TaskEvent::Progress { id, files, bytes }));
		return Ok(())
	}
	copy_entry(id, source, target, cancel, tx, &mut progress, config.copy_buffer_size)?;
	if cut {
		if canceled(cancel) { return Err(io::Error::new(io::ErrorKind::Interrupted, "Canceled")) }
		remove(source)?;
	}
	Ok(())
}

fn outcome(result: Result<io::Result<()>, tokio::task::JoinError>, changed_on_cancel: bool) -> TaskOutcome {
	match result {
		Ok(Ok(())) => TaskOutcome::Succeeded,
		Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => TaskOutcome::Canceled { changed: changed_on_cancel },
		Ok(Err(error)) => TaskOutcome::Failed(error.to_string()),
		Err(error) => TaskOutcome::Failed(error.to_string()),
	}
}

fn scan(path: &Path, cancel: &AtomicBool) -> io::Result<(u64, u64)> {
	check(cancel)?;
	let meta = fs::symlink_metadata(path)?;
	if meta.file_type().is_symlink() { Ok((1, 0)) }
	else if meta.is_dir() {
		let mut total = (0, 0);
		for entry in fs::read_dir(path)? {
			let (files, bytes) = scan(&entry?.path(), cancel)?;
			total.0 += files;
			total.1 += bytes;
		}
		Ok(total)
	} else { Ok((1, meta.len())) }
}

struct Counters {
	files:     u64,
	bytes:     u64,
	last_emit: Option<Instant>,
	emit_interval: Duration,
}

impl Counters {
	fn new(emit_interval: Duration) -> Self { Self { files: 0, bytes: 0, last_emit: None, emit_interval } }

	/// Throttled to the configured interval — including per-file completions,
	/// not just the byte-level updates within one file. Without that, a
	/// task over many small files would emit (and force a full redraw)
	/// once per file, scaling UI cost with file *count* for no benefit:
	/// the progress bar looks identical whether it's told about every
	/// single file or just sampled a few times a second.
	fn emit(&mut self, id: TaskId, tx: &UnboundedSender<Event>) {
		let now = Instant::now();
		if self.last_emit.is_some_and(|last| now.duration_since(last) < self.emit_interval) {
			return;
		}
		self.last_emit = Some(now);
		let _ = tx.send(Event::Task(TaskEvent::Progress { id, files: self.files, bytes: self.bytes }));
	}
}

fn copy_entry(id: TaskId, source: &Path, target: &Path, cancel: &AtomicBool, tx: &UnboundedSender<Event>, progress: &mut Counters, copy_buffer_size: usize) -> io::Result<()> {
	check(cancel)?;
	let meta = fs::symlink_metadata(source)?;
	if meta.file_type().is_symlink() {
		copy_symlink(source, target)?;
		progress.files += 1;
		progress.emit(id, tx);
		return Ok(())
	}
	if meta.is_dir() {
		fs::create_dir_all(target)?;
		for entry in fs::read_dir(source)? {
			let entry = entry?;
			copy_entry(id, &entry.path(), &target.join(entry.file_name()), cancel, tx, progress, copy_buffer_size)?;
		}
		return Ok(())
	}

	if !meta.is_file() { return Err(io::Error::new(io::ErrorKind::Unsupported, format!("unsupported file type: {}", source.display()))) }
	let parent = target.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
	fs::create_dir_all(parent)?;
	let name = target.file_name().unwrap_or_default().to_string_lossy();
	let temporary = parent.join(format!(".{name}.tuzi-part-{id}"));
	let result = (|| {
		let mut input = fs::File::open(source)?;
		let mut output = fs::File::create(&temporary)?;
		let mut buffer = vec![0; copy_buffer_size];
		loop {
			check(cancel)?;
			let read = input.read(&mut buffer)?;
			if read == 0 { break }
			output.write_all(&buffer[..read])?;
			progress.bytes += read as u64;
			progress.emit(id, tx);
		}
		output.flush()?;
		fs::rename(&temporary, target)?;
		fs::set_permissions(target, fs::metadata(source)?.permissions())?;
		progress.files += 1;
		progress.emit(id, tx);
		Ok(())
	})();
	if result.is_err() { let _ = fs::remove_file(&temporary); }
	result
}

#[cfg(unix)]
fn copy_symlink(source: &Path, target: &Path) -> io::Result<()> {
	std::os::unix::fs::symlink(fs::read_link(source)?, target)
}

#[cfg(windows)]
fn copy_symlink(source: &Path, target: &Path) -> io::Result<()> {
	let link = fs::read_link(source)?;
	if fs::metadata(source)?.is_dir() { std::os::windows::fs::symlink_dir(link, target) }
	else { std::os::windows::fs::symlink_file(link, target) }
}

fn run_delete(id: TaskId, path: &Path, cancel: &AtomicBool, tx: &UnboundedSender<Event>, progress_interval: Duration) -> io::Result<()> {
	let (files, _) = scan(path, cancel)?;
	let _ = tx.send(Event::Task(TaskEvent::Started { id, files, bytes: 0 }));
	let mut progress = Counters::new(progress_interval);
	delete_entry(id, path, cancel, tx, &mut progress)
}

/// Walks and removes one file at a time (rather than `remove`'s single
/// `remove_dir_all`) so progress can be reported and cancellation between
/// files is possible on a large tree.
fn delete_entry(id: TaskId, path: &Path, cancel: &AtomicBool, tx: &UnboundedSender<Event>, progress: &mut Counters) -> io::Result<()> {
	check(cancel)?;
	if fs::symlink_metadata(path)?.is_dir() {
		for entry in fs::read_dir(path)? {
			delete_entry(id, &entry?.path(), cancel, tx, progress)?;
		}
		fs::remove_dir(path)
	} else {
		fs::remove_file(path)?;
		progress.files += 1;
		progress.emit(id, tx);
		Ok(())
	}
}

fn canceled(cancel: &AtomicBool) -> bool { cancel.load(Ordering::Relaxed) }
fn check(cancel: &AtomicBool) -> io::Result<()> {
	if canceled(cancel) { Err(io::Error::new(io::ErrorKind::Interrupted, "Canceled")) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Mutex;

	#[derive(Default)]
	struct MockTrash(Mutex<Vec<PathBuf>>);
	impl TrashBackend for MockTrash {
		fn delete(&self, path: &Path) -> Result<(), String> { self.0.lock().unwrap().push(path.to_path_buf()); Ok(()) }
	}

	#[test]
	fn scan_counts_nested_files_and_bytes() {
		let dir = std::env::temp_dir().join(format!("tuzi-task-scan-{}", std::process::id()));
		let _ = fs::remove_dir_all(&dir);
		fs::create_dir_all(dir.join("nested")).unwrap();
		fs::write(dir.join("a"), b"abc").unwrap();
		fs::write(dir.join("nested/b"), b"12345").unwrap();
		assert_eq!(scan(&dir, &AtomicBool::new(false)).unwrap(), (2, 8));
		fs::remove_dir_all(dir).unwrap();
	}

	#[tokio::test]
	async fn one_task_is_created_per_top_level_source() {
		let root = std::env::temp_dir().join(format!("tuzi-task-roots-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("dst")).unwrap();
		fs::write(root.join("a"), b"a").unwrap();
		fs::write(root.join("b"), b"bb").unwrap();
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
		let mut manager = TaskManager::new(tx);
		manager.enqueue(vec![root.join("a"), root.join("b")], root.join("dst"), false, 0);
		assert_eq!(manager.tasks.len(), 2);

		while !manager.tasks.is_empty() {
			let Event::Task(event) = rx.recv().await.unwrap() else { continue };
			manager.accept(event);
		}
		assert_eq!(fs::read(root.join("dst/a")).unwrap(), b"a");
		assert_eq!(fs::read(root.join("dst/b")).unwrap(), b"bb");
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn copying_many_small_files_does_not_emit_progress_once_per_file() {
		let root = std::env::temp_dir().join(format!("tuzi-task-throttle-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("src")).unwrap();
		fs::create_dir_all(root.join("dst")).unwrap();
		for i in 0..50 {
			fs::write(root.join(format!("src/file{i}")), b"x").unwrap();
		}
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
		let mut manager = TaskManager::new(tx);
		manager.enqueue(vec![root.join("src")], root.join("dst"), false, 0);

		let mut progress_events = 0;
		while !manager.tasks.is_empty() {
			let Event::Task(event) = rx.recv().await.unwrap() else { continue };
			if matches!(event, TaskEvent::Progress { .. }) {
				progress_events += 1;
			}
			manager.accept(event);
		}

		// One event per file (50) would mean every completion forces a full
		// UI redraw — the whole point of throttling per-file completions
		// the same as byte-level ones is that this stays a small, roughly
		// constant number regardless of file count.
		assert!(progress_events < 50, "expected per-file completions to be throttled together, got {progress_events} for 50 files");
		for i in 0..50 {
			assert!(root.join(format!("dst/src/file{i}")).exists());
		}
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn queued_task_can_be_canceled_immediately() {
		let root = std::env::temp_dir().join(format!("tuzi-task-cancel-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("dst")).unwrap();
		fs::write(root.join("a"), b"a").unwrap();
		let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
		let mut manager = TaskManager::new(tx);
		manager.permits = Arc::new(Semaphore::new(0));
		manager.enqueue(vec![root.join("a")], root.join("dst"), false, 0);
		manager.cancel_selected();
		assert!(manager.tasks.is_empty());
		assert!(!root.join("dst/a").exists());
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn queued_tasks_reserve_distinct_destinations_for_equal_names() {
		let root = std::env::temp_dir().join(format!("tuzi-task-reserve-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("one")).unwrap();
		fs::create_dir_all(root.join("two")).unwrap();
		fs::create_dir_all(root.join("dst")).unwrap();
		fs::write(root.join("one/same.txt"), b"one").unwrap();
		fs::write(root.join("two/same.txt"), b"two").unwrap();
		let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
		let mut manager = TaskManager::new(tx);
		manager.permits = Arc::new(Semaphore::new(0));
		manager.enqueue(vec![root.join("one/same.txt"), root.join("two/same.txt")], root.join("dst"), false, 0);

		assert_eq!(manager.tasks[0].reserved_target.as_deref(), Some(root.join("dst/same.txt").as_path()));
		assert_eq!(manager.tasks[1].reserved_target.as_deref(), Some(root.join("dst/same (copy).txt").as_path()));
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn error_conflict_policy_rejects_the_paste_as_one_batch() {
		let root = std::env::temp_dir().join(format!("tuzi-task-conflict-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("src")).unwrap();
		fs::create_dir_all(root.join("dst")).unwrap();
		fs::write(root.join("src/a"), b"new").unwrap();
		fs::write(root.join("dst/a"), b"old").unwrap();
		let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
		let mut manager = TaskManager::new(tx);
		let conflicts = manager.enqueue_with_policy(vec![root.join("src/a")], root.join("dst"), false, 0, ConflictPolicy::Error);
		assert_eq!(conflicts, 1);
		assert!(manager.tasks.is_empty());
		assert_eq!(fs::read(root.join("dst/a")).unwrap(), b"old");
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn enqueue_delete_removes_a_directory_tree_permanently() {
		let root = std::env::temp_dir().join(format!("tuzi-task-delete-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("nested")).unwrap();
		fs::write(root.join("a"), b"a").unwrap();
		fs::write(root.join("nested/b"), b"bb").unwrap();
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
		let mut manager = TaskManager::new(tx);
		manager.enqueue_delete(vec![root.clone()], 0);
		assert_eq!(manager.tasks.len(), 1);
		assert_eq!(manager.tasks[0].kind, TaskKind::Delete);

		while !manager.tasks.is_empty() {
			let Event::Task(event) = rx.recv().await.unwrap() else { continue };
			manager.accept(event);
		}
		assert!(!root.exists(), "the whole tree, including its own directory, is gone");
	}

	#[tokio::test]
	async fn queued_delete_can_be_canceled_immediately() {
		let root = std::env::temp_dir().join(format!("tuzi-task-delete-cancel-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("a"), b"a").unwrap();
		let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
		let mut manager = TaskManager::new(tx);
		manager.permits = Arc::new(Semaphore::new(0));
		manager.enqueue_delete(vec![root.join("a")], 0);
		manager.cancel_selected();
		assert!(manager.tasks.is_empty());
		assert!(root.join("a").exists(), "canceled before it started, so nothing was removed");
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn trash_backend_is_observable_without_touching_the_real_trash() {
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
		let backend = Arc::new(MockTrash::default());
		let mut manager = TaskManager::with_trash(tx, backend.clone());
		manager.enqueue_trash(vec![PathBuf::from("example")], 0);
		while !manager.tasks.is_empty() {
			let Event::Task(event) = rx.recv().await.unwrap() else { continue };
			manager.accept(event);
		}
		assert_eq!(*backend.0.lock().unwrap(), vec![PathBuf::from("example")]);
	}

	#[tokio::test]
	async fn a_running_trash_task_does_not_claim_to_be_canceled() {
		let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
		let mut manager = TaskManager::with_trash(tx, Arc::new(MockTrash::default()));
		manager.permits = Arc::new(Semaphore::new(0));
		manager.enqueue_trash(vec![PathBuf::from("example")], 0);
		manager.tasks[0].state = TaskState::Running;
		manager.cancel_selected();
		assert_eq!(manager.tasks[0].state, TaskState::Running);
		assert!(!manager.tasks[0].cancel.load(Ordering::Relaxed));
	}

	#[tokio::test]
	async fn trash_runs_selected_descendants_before_their_ancestors() {
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
		let backend = Arc::new(MockTrash::default());
		let mut manager = TaskManager::with_trash(tx, backend.clone());
		manager.enqueue_trash(vec![PathBuf::from("parent"), PathBuf::from("parent/child")], 0);
		while !manager.tasks.is_empty() {
			let Event::Task(event) = rx.recv().await.unwrap() else { continue };
			manager.accept(event);
		}
		assert_eq!(*backend.0.lock().unwrap(), vec![PathBuf::from("parent/child"), PathBuf::from("parent")]);
	}

	#[tokio::test]
	async fn move_runs_selected_descendants_before_their_ancestors() {
		let root = std::env::temp_dir().join(format!("tuzi-task-dependent-move-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("parent/child")).unwrap();
		fs::create_dir_all(root.join("dst")).unwrap();
		fs::write(root.join("parent/child/file"), b"data").unwrap();
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
		let mut manager = TaskManager::new(tx);
		manager.enqueue(vec![root.join("parent"), root.join("parent/child")], root.join("dst"), true, 0);
		while !manager.tasks.is_empty() {
			let Event::Task(event) = rx.recv().await.unwrap() else { continue };
			manager.accept(event);
		}
		assert!(root.join("dst/child/file").exists());
		assert!(root.join("dst/parent").is_dir());
		assert!(!root.join("dst/parent/child").exists());
		fs::remove_dir_all(root).unwrap();
	}

	#[cfg(unix)]
	#[test]
	fn copying_a_symlink_preserves_the_link() {
		let root = std::env::temp_dir().join(format!("tuzi-task-link-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		fs::write(root.join("real"), b"data").unwrap();
		std::os::unix::fs::symlink("real", root.join("link")).unwrap();
		let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
		let config = crate::config::Config::default().tasks;
		let mut counters = Counters::new(Duration::from_millis(config.progress_interval_ms));
		copy_entry(1, &root.join("link"), &root.join("copy"), &AtomicBool::new(false), &tx, &mut counters, config.copy_buffer_size).unwrap();
		assert_eq!(fs::read_link(root.join("copy")).unwrap(), PathBuf::from("real"));
		fs::remove_dir_all(root).unwrap();
	}
}
