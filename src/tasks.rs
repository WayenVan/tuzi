use std::{fs, io::{self, Read, Write}, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};

use tokio::{sync::{Semaphore, mpsc::UnboundedSender}, task::JoinHandle};

use crate::{event::Event, fs::{format_size, unique_dest}};

pub type TaskId = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskKind { Copy, Move }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState { Queued, Scanning, Running, Canceling, Failed }

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
			TaskState::Scanning => "Scanning…".into(),
			TaskState::Canceling => "Canceling…".into(),
			TaskState::Failed => self.error.clone().unwrap_or_else(|| "Failed".into()),
			TaskState::Running => format!("{}/{} files  {} / {}", self.files_done, self.files_total, format_size(self.bytes_done), format_size(self.bytes_total)),
		}
	}
}

pub enum TaskEvent {
	Scanning(TaskId),
	Started { id: TaskId, files: u64, bytes: u64 },
	Progress { id: TaskId, files: u64, bytes: u64 },
	Finished { id: TaskId, tab: usize, target: PathBuf, result: Result<(), String> },
}

pub struct TaskManager {
	pub visible: bool,
	pub cursor: usize,
	pub tasks: Vec<Task>,
	next_id: TaskId,
	tx: UnboundedSender<Event>,
	permits: Arc<Semaphore>,
}

impl TaskManager {
	pub fn new(tx: UnboundedSender<Event>) -> Self {
		Self { visible: false, cursor: 0, tasks: Vec::new(), next_id: 0, tx, permits: Arc::new(Semaphore::new(2)) }
	}

	pub fn enqueue(&mut self, sources: Vec<PathBuf>, target_dir: PathBuf, cut: bool, tab: usize) {
		for source in sources {
			let Some(name) = source.file_name() else { continue };
			let target = unique_dest(&target_dir, name);
			let id = self.next_id;
			self.next_id += 1;
			let kind = if cut { TaskKind::Move } else { TaskKind::Copy };
			let verb = if cut { "Move" } else { "Copy" };
			let title = format!("{verb} {} → {}", source.display(), target.display());
			let cancel = Arc::new(AtomicBool::new(false));
			let handle = self.spawn(id, tab, source, target, target_dir.clone(), cut, cancel.clone());
			self.tasks.push(Task {
				id, kind, title, state: TaskState::Queued, files_total: 0, files_done: 0,
				bytes_total: 0, bytes_done: 0, error: None, cancel, handle: Some(handle),
			});
		}
	}

	#[allow(clippy::too_many_arguments)]
	fn spawn(&self, id: TaskId, tab: usize, source: PathBuf, target: PathBuf, refresh_dir: PathBuf, cut: bool, cancel: Arc<AtomicBool>) -> JoinHandle<()> {
		let tx = self.tx.clone();
		let permits = self.permits.clone();
		tokio::spawn(async move {
			let Ok(_permit) = permits.acquire_owned().await else { return };
			if canceled(&cancel) {
				let _ = tx.send(Event::Task(TaskEvent::Finished { id, tab, target: refresh_dir, result: Err("Canceled".into()) }));
				return;
			}
			let _ = tx.send(Event::Task(TaskEvent::Scanning(id)));
			let worker_tx = tx.clone();
			let finish_target = refresh_dir;
			let result = tokio::task::spawn_blocking(move || run_task(id, &source, &target, cut, &cancel, &worker_tx))
				.await.map_err(|error| error.to_string()).and_then(|result| result.map_err(|error| error.to_string()));
			let _ = tx.send(Event::Task(TaskEvent::Finished { id, tab, target: finish_target, result }));
		})
	}

	pub fn accept(&mut self, event: TaskEvent) -> Option<(usize, PathBuf)> {
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
			TaskEvent::Finished { id, tab, target, result } => match result {
				Ok(()) => {
					self.remove(id);
					return Some((tab, target));
				}
				Err(error) if error == "Canceled" => self.remove(id),
				Err(error) => {
					let task = self.find_mut(id)?;
					task.state = TaskState::Failed;
					task.error = Some(error);
					task.handle = None;
				}
			},
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
		if task.state == TaskState::Failed {
			let id = task.id;
			self.remove(id);
			return;
		}
		task.cancel.store(true, Ordering::Relaxed);
		if task.state == TaskState::Queued {
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
	fn remove(&mut self, id: TaskId) { self.tasks.retain(|task| task.id != id); self.clamp_cursor(); }
	fn clamp_cursor(&mut self) { self.cursor = self.cursor.min(self.tasks.len().saturating_sub(1)); }
}

fn run_task(id: TaskId, source: &Path, target: &Path, cut: bool, cancel: &AtomicBool, tx: &UnboundedSender<Event>) -> io::Result<()> {
	let (files, bytes) = scan(source, cancel)?;
	let _ = tx.send(Event::Task(TaskEvent::Started { id, files, bytes }));
	let mut progress = Counters::default();
	if cut && fs::rename(source, target).is_ok() {
		let _ = tx.send(Event::Task(TaskEvent::Progress { id, files, bytes }));
		return Ok(())
	}
	copy_entry(id, source, target, cancel, tx, &mut progress)?;
	if cut {
		if canceled(cancel) { return Err(io::Error::new(io::ErrorKind::Interrupted, "Canceled")) }
		remove(source)?;
	}
	Ok(())
}

fn scan(path: &Path, cancel: &AtomicBool) -> io::Result<(u64, u64)> {
	check(cancel)?;
	let meta = fs::symlink_metadata(path)?;
	if meta.is_dir() {
		let mut total = (0, 0);
		for entry in fs::read_dir(path)? {
			let (files, bytes) = scan(&entry?.path(), cancel)?;
			total.0 += files;
			total.1 += bytes;
		}
		Ok(total)
	} else { Ok((1, meta.len())) }
}

#[derive(Default)]
struct Counters {
	files:     u64,
	bytes:     u64,
	last_emit: Option<Instant>,
}

impl Counters {
	fn emit(&mut self, id: TaskId, tx: &UnboundedSender<Event>, force: bool) {
		let now = Instant::now();
		if !force && self.last_emit.is_some_and(|last| now.duration_since(last) < Duration::from_millis(75)) {
			return;
		}
		self.last_emit = Some(now);
		let _ = tx.send(Event::Task(TaskEvent::Progress { id, files: self.files, bytes: self.bytes }));
	}
}

fn copy_entry(id: TaskId, source: &Path, target: &Path, cancel: &AtomicBool, tx: &UnboundedSender<Event>, progress: &mut Counters) -> io::Result<()> {
	check(cancel)?;
	if fs::symlink_metadata(source)?.is_dir() {
		fs::create_dir_all(target)?;
		for entry in fs::read_dir(source)? {
			let entry = entry?;
			copy_entry(id, &entry.path(), &target.join(entry.file_name()), cancel, tx, progress)?;
		}
		return Ok(())
	}

	let parent = target.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
	fs::create_dir_all(parent)?;
	let name = target.file_name().unwrap_or_default().to_string_lossy();
	let temporary = parent.join(format!(".{name}.tuzi-part-{id}"));
	let result = (|| {
		let mut input = fs::File::open(source)?;
		let mut output = fs::File::create(&temporary)?;
		let mut buffer = vec![0; 512 * 1024];
		loop {
			check(cancel)?;
			let read = input.read(&mut buffer)?;
			if read == 0 { break }
			output.write_all(&buffer[..read])?;
			progress.bytes += read as u64;
			progress.emit(id, tx, false);
		}
		output.flush()?;
		fs::rename(&temporary, target)?;
		fs::set_permissions(target, fs::metadata(source)?.permissions())?;
		progress.files += 1;
		progress.emit(id, tx, true);
		Ok(())
	})();
	if result.is_err() { let _ = fs::remove_file(&temporary); }
	result
}

fn remove(path: &Path) -> io::Result<()> {
	if fs::symlink_metadata(path)?.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

fn canceled(cancel: &AtomicBool) -> bool { cancel.load(Ordering::Relaxed) }
fn check(cancel: &AtomicBool) -> io::Result<()> {
	if canceled(cancel) { Err(io::Error::new(io::ErrorKind::Interrupted, "Canceled")) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
	use super::*;

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
}
