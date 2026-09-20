use std::{
	collections::{HashMap, HashSet},
	io,
	path::{Path, PathBuf},
	sync::{Arc, Mutex, mpsc},
	thread,
	time::Duration,
};

use notify::{Config, EventKind, RecursiveMode, Watcher as NotifyWatcher, event::ModifyKind};
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
	/// its own. Directories that no longer exist, or that cannot be read, are
	/// not reported: their own listing already shows that.
	RegisterFailed { path: PathBuf, error: String },
	/// The native backend could not be used (typically the OS limit on inotify
	/// instances or watches), so watching switched to polling. Anything may have
	/// changed while switching.
	FellBackToPolling(String),
}

/// Watches the directories an app says it wants watched, and keeps that true.
///
/// The interface is one declaration, [`Watcher::sync`]: *this* is the set of
/// directories to watch. Everything else follows from it inside the worker
/// thread: registering what is missing, dropping what is not wanted, and
/// noticing when a watch has been lost (the kernel drops a watch when its
/// directory is deleted or moved away, and does not say so) and arming it
/// again. Callers never track individual watches, so they cannot get out of
/// step with the OS.
pub struct Watcher {
	commands: mpsc::Sender<Command>,
	worker: Option<thread::JoinHandle<()>>,
	pending: PendingChanges,
	#[cfg(test)]
	registry: Arc<Mutex<Registry>>,
}

enum Command {
	/// The complete set of directories that should be watched. `done` is
	/// signalled once it has been applied (used by tests).
	Sync { wanted: HashSet<PathBuf>, done: Option<mpsc::SyncSender<()>> },
	/// A hint that a wanted directory may need its watch registered again.
	/// `force` says the watch is known to be dead (the directory itself was
	/// deleted or moved, which is the kernel's cue to drop it): register again
	/// without checking identity, which cannot be trusted after a reused inode.
	Heal { path: PathBuf, force: bool },
	Shutdown,
}

type Handler = Box<dyn FnMut(notify::Result<notify::Event>) + Send + 'static>;
type Backend = Box<dyn NotifyWatcher + Send>;
/// Builds a backend around an event handler.
type BackendCtor = Box<dyn FnOnce(Handler) -> notify::Result<Backend> + Send>;

/// Tells the app what the watcher found out.
#[derive(Clone)]
struct Notifier {
	tab: usize,
	tx: UnboundedSender<Event>,
}

impl Notifier {
	fn issue(&self, issue: WatchIssue) {
		let _ = self.tx.send(Event::WatchIssue { tab: self.tab, issue });
	}

	/// A directory should be read again: its watch was lost and is back, so
	/// changes made in between were never reported.
	fn changed(&self, path: PathBuf) {
		let _ = self.tx.send(Event::Changed { tab: self.tab, path });
	}
}

/// `notify` needs the canonical (symlink-resolved) path to actually register
/// an OS-level watch, and its backends sometimes report that resolved form
/// back in events too, but the tree indexes nodes by their own, possibly
/// symlinked, path. This keeps both directions so raw events can be matched in
/// canonical space while everything handed back to the app stays in the tree's
/// own path space. Shared by the worker (which writes it) and the event
/// handler (which reads it).
#[derive(Default)]
struct Registry {
	by_canonical: HashMap<PathBuf, PathBuf>,
	by_original: HashMap<PathBuf, Registration>,
	/// What the app last said it wants watched.
	wanted: HashSet<PathBuf>,
}

/// What is known about a registered directory, enough to tell later whether
/// the watch still belongs to the directory now found at that path.
struct Registration {
	canonical: PathBuf,
	identity: Option<Identity>,
}

/// (device, inode, creation time): a directory deleted and recreated at the
/// same path is a different one, and the watch on the old one is dead. The
/// inode number alone cannot tell them apart: filesystems reuse a freed inode
/// at once (ext4 did, every time it was tried), so the creation time, where
/// the OS reports one, is part of the identity. This is only a fallback for a
/// loss the events did not announce; see `Command::Heal`.
type Identity = (u64, u64, Option<std::time::SystemTime>);

fn identity(path: &Path) -> Option<Identity> {
	#[cfg(unix)]
	{
		use std::os::unix::fs::MetadataExt;
		std::fs::metadata(path).ok().map(|metadata| (metadata.dev(), metadata.ino(), metadata.created().ok()))
	}
	#[cfg(not(unix))]
	{
		let _ = path;
		None
	}
}

/// Caps how long a directory under nonstop churn (e.g. a log file being
/// appended to faster than the configured debounce apart) can be starved of an
/// update: the sliding debounce below would otherwise keep resetting forever.
const MAX_CHANGE_BATCH: usize = 1000;

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
	/// Never fails because of the backend: if the native one cannot be created
	/// (say the per-user limit on inotify instances is used up) it falls back to
	/// polling and says so, and if even that fails it watches nothing and says
	/// so. A view that cannot update itself is still a view.
	pub fn new(tab: usize, tx: UnboundedSender<Event>, debounce: Duration, max_wait: Duration, poll_interval: Duration) -> io::Result<Self> {
		let runtime = Handle::try_current().map_err(io::Error::other)?;
		let native: BackendCtor = Box::new(move |handler| Ok(Box::new(notify::RecommendedWatcher::new(handler, Config::default().with_poll_interval(poll_interval))?)));
		let polling: BackendCtor = Box::new(move |handler| Ok(Box::new(notify::PollWatcher::new(handler, Config::default().with_poll_interval(poll_interval))?)));
		Ok(Self::with_backends(tab, tx, runtime, debounce, max_wait, native, polling))
	}

	#[cfg(test)]
	fn new_polling(tab: usize, tx: UnboundedSender<Event>, interval: Duration) -> io::Result<Self> {
		let runtime = Handle::try_current().map_err(io::Error::other)?;
		let polling = move || -> BackendCtor { Box::new(move |handler| Ok(Box::new(notify::PollWatcher::new(handler, Config::default().with_poll_interval(interval).with_compare_contents(true))?))) };
		Ok(Self::with_backends(tab, tx, runtime, Duration::from_millis(250), Duration::from_secs(1), polling(), polling()))
	}

	fn with_backends(tab: usize, tx: UnboundedSender<Event>, runtime: Handle, debounce: Duration, max_wait: Duration, native: BackendCtor, polling: BackendCtor) -> Self {
		let registry = Arc::new(Mutex::new(Registry::default()));
		let pending: PendingChanges = Arc::new(Mutex::new(HashMap::new()));
		let notifier = Notifier { tab, tx };
		let (commands, rx) = mpsc::channel();

		let context = HandlerContext {
			notifier: notifier.clone(),
			registry: registry.clone(),
			pending: pending.clone(),
			commands: commands.clone(),
			runtime,
			debounce,
			max_wait,
			gate: Arc::new(IssueGate::default()),
		};
		let make_handler: Arc<dyn Fn() -> Handler + Send + Sync> = Arc::new(move || event_handler(context.clone()));

		let mut polling = Some(polling);
		let backend = match native(make_handler()) {
			Ok(backend) => backend,
			Err(error) => match polling.take().expect("set just above")(make_handler()) {
				Ok(backend) => {
					notifier.issue(WatchIssue::FellBackToPolling(error.to_string()));
					backend
				}
				Err(second) => {
					notifier.issue(WatchIssue::BackendError(format!("file watching is unavailable ({error}); polling failed too ({second})")));
					Box::new(notify::NullWatcher::new(make_handler(), Config::default()).expect("a null watcher cannot fail"))
				}
			},
		};

		let worker = Worker {
			backend,
			polling,
			registry: registry.clone(),
			pending: pending.clone(),
			notifier,
			make_handler,
			failed: HashSet::new(),
			lost: HashSet::new(),
		};
		let worker = thread::Builder::new().name("tuzi-watcher".into()).spawn(move || worker.run(rx)).expect("watcher worker thread");
		Self {
			commands,
			worker: Some(worker),
			pending,
			#[cfg(test)]
			registry,
		}
	}

	/// Declares the complete set of directories to watch, each one level and
	/// not recursively, the way nodes are open on screen. Applied on the
	/// worker thread, so it never waits on a slow symlink target or network
	/// mount.
	///
	/// A directory's parent should be in the set too (the tree only ever asks
	/// for expanded directories, whose parents are open): the parent's watch is
	/// how a directory that came back after being deleted is noticed.
	pub fn sync(&self, wanted: HashSet<PathBuf>) {
		let _ = self.commands.send(Command::Sync { wanted, done: None });
	}

	/// Like [`Watcher::sync`], but returns once it has been applied. For when
	/// something must not happen before the watches are in place: a directory
	/// listing read before its watch exists can miss a change made in between,
	/// and nothing would ever report it.
	pub(crate) fn sync_wait(&self, wanted: HashSet<PathBuf>) {
		let (done, applied) = mpsc::sync_channel(1);
		self.commands.send(Command::Sync { wanted, done: Some(done) }).unwrap();
		applied.recv().unwrap();
	}

	/// The tree paths currently registered with the backend.
	#[cfg(test)]
	pub(crate) fn registered(&self) -> HashSet<PathBuf> {
		self.registry.lock().unwrap().by_original.keys().cloned().collect()
	}
}

impl Drop for Watcher {
	fn drop(&mut self) {
		let _ = self.commands.send(Command::Shutdown);
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

/// The thread that owns the backend and makes the registered watches match
/// what is wanted.
struct Worker {
	backend: Backend,
	/// Still unused: the polling backend to switch to if the native one runs
	/// out of room.
	polling: Option<BackendCtor>,
	registry: Arc<Mutex<Registry>>,
	pending: PendingChanges,
	notifier: Notifier,
	make_handler: Arc<dyn Fn() -> Handler + Send + Sync>,
	/// Directories whose registration failure was already reported, so that
	/// asking again does not report it again.
	failed: HashSet<PathBuf>,
	/// Wanted directories whose watch was lost. When one is registered again,
	/// what changed in the meantime was never reported, so it is read again.
	lost: HashSet<PathBuf>,
}

impl Worker {
	fn run(mut self, commands: mpsc::Receiver<Command>) {
		while let Ok(command) = commands.recv() {
			match command {
				Command::Sync { wanted, done } => {
					self.sync(wanted);
					if let Some(done) = done {
						let _ = done.send(());
					}
				}
				Command::Heal { path, force } => {
					if self.registry.lock().unwrap().wanted.contains(&path) {
						self.reconcile_with(&path, force);
					}
				}
				Command::Shutdown => break,
			}
		}
	}

	fn sync(&mut self, wanted: HashSet<PathBuf>) {
		let dropped: Vec<PathBuf> = self.registry.lock().unwrap().by_original.keys().filter(|path| !wanted.contains(*path)).cloned().collect();
		for path in &dropped {
			self.unregister(path);
			// Nobody is looking at that directory any more.
			if let Some(change) = self.pending.lock().unwrap().remove(path)
				&& let Some(abort) = change.abort
			{
				abort.abort();
			}
		}
		self.failed.retain(|path| wanted.contains(path));
		self.lost.retain(|path| wanted.contains(path));
		self.registry.lock().unwrap().wanted = wanted.clone();
		for path in &wanted {
			self.reconcile(path);
		}
	}

	/// Makes one wanted directory's registration correct: present, and
	/// belonging to the directory that is at that path now.
	fn reconcile(&mut self, path: &Path) {
		self.reconcile_with(path, false);
	}

	/// With `force`, a registration that looks current is replaced anyway.
	fn reconcile_with(&mut self, path: &Path, force: bool) {
		let Ok(canonical) = path.canonicalize() else {
			// Gone, or out of reach. Whatever was registered is dead; remember
			// that, so that when it comes back it is registered and re-read.
			if self.unregister(path) {
				self.lost.insert(path.to_path_buf());
			}
			return;
		};
		let now = identity(&canonical);
		let current = self.registry.lock().unwrap().by_original.get(path).map(|registered| (registered.canonical.clone(), registered.identity));
		match current {
			Some((old_canonical, old_identity)) if !force && old_canonical == canonical && now.is_some() && old_identity == now => return,
			Some(_) => {
				// Registered, but for a directory that is no longer the one here.
				self.unregister(path);
				self.lost.insert(path.to_path_buf());
			}
			None => {}
		}
		self.register(path, canonical, now);
	}

	fn register(&mut self, path: &Path, canonical: PathBuf, identity: Option<Identity>) {
		{
			// Two tree paths can resolve to one directory (a symlink); the first
			// to register keeps the events.
			let registry = self.registry.lock().unwrap();
			if let Some(owner) = registry.by_canonical.get(&canonical)
				&& owner != path
				&& registry.by_original.contains_key(owner)
			{
				return;
			}
		}
		match self.backend.watch(&canonical, RecursiveMode::NonRecursive) {
			Ok(()) => {
				{
					let mut registry = self.registry.lock().unwrap();
					registry.by_canonical.insert(canonical.clone(), path.to_path_buf());
					registry.by_original.insert(path.to_path_buf(), Registration { canonical, identity });
				}
				self.failed.remove(path);
				if self.lost.remove(path) {
					self.notifier.changed(path.to_path_buf());
				}
			}
			Err(error) => self.registration_failed(path, error),
		}
	}

	/// Returns whether the path was registered.
	fn unregister(&mut self, path: &Path) -> bool {
		let removed = {
			let mut registry = self.registry.lock().unwrap();
			let removed = registry.by_original.remove(path);
			if let Some(registration) = &removed {
				registry.by_canonical.remove(&registration.canonical);
			}
			removed
		};
		match removed {
			Some(registration) => {
				let _ = self.backend.unwatch(&registration.canonical);
				true
			}
			None => false,
		}
	}

	fn registration_failed(&mut self, path: &Path, error: notify::Error) {
		if is_limit(&error) && self.polling.is_some() {
			self.fall_back(error);
			return;
		}
		let error = notify_to_io(error);
		// A directory that is gone, or that we may not read, is that
		// directory's own problem and its listing already shows it. Only a
		// failure that says something about watching itself is worth
		// interrupting the user for.
		if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied) {
			return;
		}
		if self.failed.insert(path.to_path_buf()) {
			self.notifier.issue(WatchIssue::RegisterFailed { path: path.to_path_buf(), error: error.to_string() });
		}
	}

	/// The native backend has no room left: watch by polling instead.
	fn fall_back(&mut self, reason: notify::Error) {
		let Some(polling) = self.polling.take() else { return };
		match polling((self.make_handler)()) {
			Ok(backend) => {
				// Dropping the old backend closes it, and its watches with it.
				self.backend = backend;
				{
					let mut registry = self.registry.lock().unwrap();
					registry.by_canonical.clear();
					registry.by_original.clear();
				}
				self.notifier.issue(WatchIssue::FellBackToPolling(reason.to_string()));
				let wanted: Vec<PathBuf> = self.registry.lock().unwrap().wanted.iter().cloned().collect();
				for path in &wanted {
					self.reconcile(path);
				}
			}
			Err(error) => self.notifier.issue(WatchIssue::BackendError(format!("polling fallback failed: {error}"))),
		}
	}
}

/// Whether the backend refused because it is out of room, as opposed to
/// something being wrong with one directory.
fn is_limit(error: &notify::Error) -> bool {
	match &error.kind {
		notify::ErrorKind::MaxFilesWatch => true,
		#[cfg(unix)]
		notify::ErrorKind::Io(source) => matches!(source.raw_os_error(), Some(libc::EMFILE | libc::ENFILE | libc::ENOSPC)),
		_ => false,
	}
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

fn raise_issue(context: &HandlerContext, issue: WatchIssue) {
	{
		let mut state = context.gate.state.lock().unwrap();
		if state.running {
			state.queued = Some(issue);
			return;
		}
		state.running = true;
	}
	let (notifier, gate) = (context.notifier.clone(), context.gate.clone());
	let delay = context.debounce.max(Duration::from_millis(200));
	context.runtime.spawn(async move {
		tokio::time::sleep(delay).await;
		notifier.issue(issue);
		loop {
			tokio::time::sleep(ISSUE_COOLDOWN).await;
			let next = {
				let mut state = gate.state.lock().unwrap();
				let next = state.queued.take();
				state.running = next.is_some();
				next
			};
			match next {
				Some(issue) => notifier.issue(issue),
				None => break,
			}
		}
	});
}

/// Everything an event handler needs; cloned for each backend it is built for.
#[derive(Clone)]
struct HandlerContext {
	notifier: Notifier,
	registry: Arc<Mutex<Registry>>,
	pending: PendingChanges,
	commands: mpsc::Sender<Command>,
	runtime: Handle,
	debounce: Duration,
	max_wait: Duration,
	gate: Arc<IssueGate>,
}

// The backend (FSEvents/inotify/...) doesn't consistently report a changed
// child's path vs. its watched parent, and may resolve symlinks along the
// way, so the ancestor walk below matches in canonical space. Everything
// downstream of that match (the `changed` map, debounce bookkeeping, the
// event finally sent out) is then translated back to the tree's own path,
// which is what `Tab` actually indexes its nodes by.
fn event_handler(context: HandlerContext) -> Handler {
	Box::new(move |res| {
		let event = match res {
			Ok(event) => event,
			// A path that vanished mid-flight is the parent directory's news
			// to report; anything else means we may have missed changes.
			Err(error) if matches!(error.kind, notify::ErrorKind::PathNotFound | notify::ErrorKind::WatchNotFound) => return,
			Err(error) => {
				raise_issue(&context, WatchIssue::BackendError(error.to_string()));
				return;
			}
		};
		if event.need_rescan() {
			raise_issue(&context, WatchIssue::EventsDropped);
		}
		if event.kind.is_access() {
			return;
		}
		let directory_itself_left = event.kind.is_remove() || matches!(event.kind, EventKind::Modify(ModifyKind::Name(_)));
		let mut changed: HashMap<PathBuf, (HashSet<PathBuf>, bool)> = HashMap::new();
		let mut heal: Vec<(PathBuf, bool)> = Vec::new();
		{
			let registry = context.registry.lock().unwrap();
			for path in event.paths {
				// A path can be two things at once. If it is a directory we watch,
				// this is news about that directory itself: it changed, or it was
				// deleted or moved, which the kernel answers by dropping its watch.
				if let Some(original_dir) = registry.by_canonical.get(&path) {
					changed.entry(original_dir.clone()).or_default().1 = true;
					if directory_itself_left {
						heal.push((original_dir.clone(), true));
					}
				}
				// And it is an entry of the nearest watched directory above it. That
				// matters even for a watched directory: its parent's listing has to
				// hear that it appeared, was renamed, or is gone. Attributing the
				// path only to itself, as this once did, left a deleted directory in
				// its parent's listing for good.
				let Some(canonical_dir) = path.parent().and_then(|parent| nearest_watched(&registry.by_canonical, parent)) else {
					continue;
				};
				let Some(original_dir) = registry.by_canonical.get(&canonical_dir) else {
					continue;
				};
				if let Ok(relative) = path.strip_prefix(&canonical_dir)
					&& let Some(component) = relative.components().next()
				{
					let child = original_dir.join(component.as_os_str());
					// A directory that is still wanted but was lost has just been
					// mentioned by its parent: it is back.
					if registry.wanted.contains(&child) && !registry.by_original.contains_key(&child) {
						heal.push((child.clone(), false));
					}
					changed.entry(original_dir.clone()).or_default().0.insert(child);
				}
			}
		}
		for (path, force) in heal {
			let _ = context.commands.send(Command::Heal { path, force });
		}
		for (parent, (paths, refresh)) in changed {
			debounce_changes(context.notifier.tab, context.notifier.tx.clone(), context.pending.clone(), &context.runtime, ChangeSet { parent, paths, refresh }, context.debounce, context.max_wait);
		}
	})
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
		let event = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.expect("max_wait did not force a flush").unwrap();
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
		watcher.sync_wait(HashSet::from([dir.clone()]));
		fs::write(dir.join("new.txt"), b"hi").unwrap();

		let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("timed out").expect("channel closed");
		match &event {
			Event::FilesChanged { tab: 7, parent, .. } => assert_eq!(parent, &dir),
			Event::Changed { tab: 7, path } => assert_eq!(path, &dir),
			_ => panic!("unexpected event"),
		}
		assert!(reports_new_file(&event, &dir, &dir.join("new.txt")));

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
		watcher.sync_wait(HashSet::from([link.clone()]));
		fs::write(real.join("new.txt"), b"hi").unwrap();

		let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("timed out").expect("channel closed");
		match &event {
			Event::FilesChanged { tab: 11, parent, .. } => assert_eq!(parent, &link, "must be tagged with the tree's own path, not the symlink-resolved one"),
			Event::Changed { tab: 11, path } => assert_eq!(path, &link, "must be tagged with the tree's own path, not the symlink-resolved one"),
			_ => panic!("unexpected event"),
		}
		assert!(reports_new_file(&event, &link, &link.join("new.txt")));

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

	fn probe_handler(tx: mpsc::UnboundedSender<Event>) -> Handler {
		let (commands, _unread) = std::sync::mpsc::channel();
		event_handler(HandlerContext {
			notifier: Notifier { tab: 1, tx },
			registry: Arc::default(),
			pending: Arc::default(),
			commands,
			runtime: Handle::current(),
			debounce: Duration::from_millis(20),
			max_wait: Duration::from_millis(200),
			gate: Arc::default(),
		})
	}

	fn native_watcher(tx: mpsc::UnboundedSender<Event>) -> Watcher {
		Watcher::new(1, tx, Duration::from_millis(20), Duration::from_millis(200), Duration::from_millis(50)).unwrap()
	}

	fn temp_dir(name: &str) -> PathBuf {
		let root = std::env::temp_dir().join(format!("tuzi-{name}-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(&root).unwrap();
		root.canonicalize().unwrap()
	}

	fn set(paths: &[&Path]) -> HashSet<PathBuf> {
		paths.iter().map(|path| path.to_path_buf()).collect()
	}

	async fn next_issue(rx: &mut mpsc::UnboundedReceiver<Event>, within: Duration) -> Option<WatchIssue> {
		match tokio::time::timeout(within, rx.recv()).await {
			Ok(Some(Event::WatchIssue { tab: 1, issue })) => Some(issue),
			Ok(Some(_)) => panic!("unexpected event"),
			_ => None,
		}
	}

	/// Drains events for `window`, returning what arrived.
	async fn events_for(rx: &mut mpsc::UnboundedReceiver<Event>, window: Duration) -> Vec<Event> {
		let mut seen = Vec::new();
		while let Ok(Some(event)) = tokio::time::timeout(window, rx.recv()).await {
			seen.push(event);
		}
		seen
	}

	/// What a watcher must report for a file created in `dir`: that file's
	/// upsert or, when the backend also saw the directory itself change, that
	/// the whole directory changed. Polling does the latter once the
	/// directory's mtime has moved on, and mtimes are coarse, so which one
	/// arrives depends on timing. Either one gets the view right.
	fn reports_new_file(event: &Event, dir: &Path, file: &Path) -> bool {
		is_upsert_of(event, file) || matches!(event, Event::Changed { path, .. } if path == dir)
	}

	fn is_upsert_of(event: &Event, path: &Path) -> bool {
		matches!(event, Event::FilesChanged { changes, .. } if changes.iter().any(|change| matches!(change, FsChange::Upsert { path: changed, .. } if changed == path)))
	}

	/// Waits, up to `within`, for `check` to hold.
	async fn eventually(within: Duration, mut check: impl FnMut() -> bool) -> bool {
		let deadline = std::time::Instant::now() + within;
		while std::time::Instant::now() < deadline {
			if check() {
				return true;
			}
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
		check()
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
	async fn sync_registers_what_is_wanted_and_drops_what_is_not() {
		let root = temp_dir("watch-sync");
		fs::create_dir_all(root.join("a")).unwrap();
		fs::create_dir_all(root.join("b")).unwrap();
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = native_watcher(tx);
		let (a, b) = (root.join("a"), root.join("b"));

		watcher.sync_wait(set(&[&a, &b]));
		assert_eq!(watcher.registered(), set(&[&a, &b]));
		watcher.sync_wait(set(&[&a]));
		assert_eq!(watcher.registered(), set(&[&a]), "b is not wanted any more");

		fs::write(b.join("ignored"), b"").unwrap();
		fs::write(a.join("heard"), b"").unwrap();
		let seen = events_for(&mut rx, Duration::from_millis(600)).await;
		assert!(seen.iter().any(|event| is_upsert_of(event, &a.join("heard"))));
		assert!(!seen.iter().any(|event| is_upsert_of(event, &b.join("ignored"))), "a directory that was dropped must not report");

		watcher.sync_wait(HashSet::new());
		assert!(watcher.registered().is_empty());
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn declaring_the_same_set_again_changes_nothing() {
		let root = temp_dir("watch-same");
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = native_watcher(tx);
		watcher.sync_wait(set(&[&root]));
		let before = watcher.registered();
		for _ in 0..5 {
			watcher.sync_wait(set(&[&root]));
		}
		assert_eq!(watcher.registered(), before);
		assert!(events_for(&mut rx, Duration::from_millis(500)).await.is_empty(), "declaring the same set must not look like a change");
		fs::write(root.join("real"), b"").unwrap();
		assert!(events_for(&mut rx, Duration::from_secs(3)).await.iter().any(|event| is_upsert_of(event, &root.join("real"))), "and the watch still works afterwards");
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	/// The caller only ever declares the set. That a deleted and recreated
	/// directory is watched again is the watcher's own doing.
	#[tokio::test]
	async fn a_directory_deleted_and_recreated_is_watched_again_without_being_asked() {
		let root = temp_dir("watch-heal");
		let sub = root.join("sub");
		fs::create_dir_all(&sub).unwrap();
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = native_watcher(tx);
		watcher.sync_wait(set(&[&root, &sub]));

		// On most filesystems the new directory gets the old one's inode number,
		// which is what makes a dead watch look current. To make sure the
		// watcher looks at the directory only once it is back, whatever its
		// timing, it is kept from acting while this happens (the event handler
		// needs the registry to act). Were it to look in between it would just
		// drop the watch, and the parent's news that the directory returned
		// would register it again: the other recovery path, which would hide a
		// fault in this one.
		{
			let _held = watcher.registry.lock().unwrap();
			fs::remove_dir_all(&sub).unwrap();
			fs::create_dir(&sub).unwrap();
		}
		assert!(eventually(Duration::from_secs(3), || watcher.registered().contains(&sub)).await);
		events_for(&mut rx, Duration::from_millis(800)).await;

		fs::write(sub.join("after"), b"").unwrap();
		assert!(events_for(&mut rx, Duration::from_secs(3)).await.iter().any(|event| is_upsert_of(event, &sub.join("after"))), "the recreated directory must deliver events again");
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_watch_that_was_lost_is_reported_back_so_what_was_missed_is_read() {
		let root = temp_dir("watch-lost");
		let sub = root.join("sub");
		fs::create_dir_all(&sub).unwrap();
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = native_watcher(tx);
		watcher.sync_wait(set(&[&root, &sub]));
		events_for(&mut rx, Duration::from_millis(300)).await;

		fs::rename(&sub, root.join("sub.old")).unwrap();
		fs::create_dir(&sub).unwrap();
		fs::write(sub.join("made_before_the_watch_was_back"), b"").unwrap();
		let seen = events_for(&mut rx, Duration::from_secs(2)).await;
		assert!(seen.iter().any(|event| matches!(event, Event::Changed { path, .. } if path == &sub)), "the directory must be reported as changed so its listing is read again");
		assert!(watcher.registered().contains(&sub));
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_directory_that_comes_back_later_is_found_through_its_parent() {
		let root = temp_dir("watch-later");
		let sub = root.join("sub");
		fs::create_dir_all(&sub).unwrap();
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = native_watcher(tx);
		watcher.sync_wait(set(&[&root, &sub]));

		fs::remove_dir_all(&sub).unwrap();
		assert!(eventually(Duration::from_secs(3), || !watcher.registered().contains(&sub)).await, "while it is gone there is nothing to watch");
		events_for(&mut rx, Duration::from_millis(400)).await;

		// Much later: nobody asks again, and the deleted directory's own watch is long dead.
		fs::create_dir(&sub).unwrap();
		assert!(eventually(Duration::from_secs(3), || watcher.registered().contains(&sub)).await, "the parent reports the new directory, which is wanted, so it is watched");
		fs::write(sub.join("later"), b"").unwrap();
		assert!(events_for(&mut rx, Duration::from_secs(3)).await.iter().any(|event| is_upsert_of(event, &sub.join("later"))));
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	/// Deleting or renaming a directory we watch is news for its parent too: the
	/// parent's listing has to lose the entry. The path used to be attributed to
	/// the watched directory alone, so the parent never heard.
	#[tokio::test]
	async fn a_watched_directory_that_is_deleted_or_renamed_is_reported_to_its_parent() {
		let root = temp_dir("watch-parent");
		let (gone, moved) = (root.join("gone"), root.join("moved"));
		fs::create_dir_all(&gone).unwrap();
		fs::create_dir_all(&moved).unwrap();
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = native_watcher(tx);
		watcher.sync_wait(set(&[&root, &gone, &moved]));

		fs::remove_dir_all(&gone).unwrap();
		fs::rename(&moved, root.join("renamed")).unwrap();
		let seen = events_for(&mut rx, Duration::from_millis(900)).await;
		let told_root = |wanted: &dyn Fn(&FsChange) -> bool| seen.iter().any(|event| matches!(event, Event::FilesChanged { parent, changes, .. } if parent == &root && changes.iter().any(wanted)));
		assert!(told_root(&|change| matches!(change, FsChange::Delete { path } if path == &gone)), "the parent must hear that `gone` is gone");
		assert!(told_root(&|change| matches!(change, FsChange::Delete { path } if path == &moved)), "and that `moved` is no longer there");
		assert!(told_root(&|change| matches!(change, FsChange::Upsert { path, .. } if path == &root.join("renamed"))), "and that `renamed` is");
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	/// A backend whose every `watch` fails with the given error.
	struct RefusingWatcher(fn() -> notify::Error);

	impl NotifyWatcher for RefusingWatcher {
		fn new<F: notify::EventHandler>(_: F, _: Config) -> notify::Result<Self> {
			Ok(Self(|| notify::Error::generic("unused")))
		}
		fn watch(&mut self, _: &Path, _: RecursiveMode) -> notify::Result<()> {
			Err((self.0)())
		}
		fn unwatch(&mut self, _: &Path) -> notify::Result<()> {
			Ok(())
		}
		fn kind() -> notify::WatcherKind {
			notify::WatcherKind::NullWatcher
		}
	}

	fn refusing(error: fn() -> notify::Error) -> BackendCtor {
		Box::new(move |_| Ok(Box::new(RefusingWatcher(error)) as Backend))
	}

	fn failing_to_start() -> BackendCtor {
		Box::new(|_| Err(notify::Error::new(notify::ErrorKind::Io(io::Error::from_raw_os_error(libc::EMFILE)))))
	}

	fn polling(interval_ms: u64) -> BackendCtor {
		Box::new(move |handler| Ok(Box::new(notify::PollWatcher::new(handler, Config::default().with_poll_interval(Duration::from_millis(interval_ms)))?) as Backend))
	}

	fn watcher_with(tx: mpsc::UnboundedSender<Event>, native: BackendCtor, polling: BackendCtor) -> Watcher {
		Watcher::with_backends(1, tx, Handle::current(), Duration::from_millis(20), Duration::from_millis(200), native, polling)
	}

	#[tokio::test]
	async fn a_failure_of_watching_itself_is_reported_once() {
		let root = temp_dir("watch-refused");
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = watcher_with(tx, refusing(|| notify::Error::generic("the backend rejected this watch")), polling(50));

		watcher.sync_wait(set(&[&root]));
		match next_issue(&mut rx, Duration::from_secs(2)).await {
			Some(WatchIssue::RegisterFailed { path, error }) => {
				assert_eq!(path, root);
				assert!(error.contains("rejected"), "the reason must survive: {error}");
			}
			other => panic!("expected RegisterFailed, got {other:?}"),
		}
		// Declaring it again does not make the same complaint again.
		watcher.sync_wait(set(&[&root]));
		watcher.sync_wait(set(&[&root]));
		assert_eq!(next_issue(&mut rx, Duration::from_millis(400)).await, None);
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_directorys_own_access_problems_are_left_to_its_listing() {
		use std::os::unix::fs::PermissionsExt;

		let root = temp_dir("watch-own");
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = native_watcher(tx);

		watcher.sync_wait(set(&[&root.join("does-not-exist")]));
		assert_eq!(next_issue(&mut rx, Duration::from_millis(500)).await, None, "a directory that is gone is not a watching failure");

		let locked = root.join("locked");
		fs::create_dir(&locked).unwrap();
		fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
		if fs::read_dir(&locked).is_err() {
			// Not root, so the permission really applies: the listing shows the
			// error on the node, and a second message here would be noise.
			watcher.sync_wait(set(&[&locked]));
			assert_eq!(next_issue(&mut rx, Duration::from_millis(500)).await, None, "unreadable is not a watching failure either");
		}
		fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn running_out_of_watches_falls_back_to_polling_and_keeps_working() {
		let root = temp_dir("watch-fallback");
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = watcher_with(tx, refusing(|| notify::Error::new(notify::ErrorKind::MaxFilesWatch)), polling(25));

		watcher.sync_wait(set(&[&root]));
		match next_issue(&mut rx, Duration::from_secs(2)).await {
			Some(WatchIssue::FellBackToPolling(reason)) => assert!(reason.to_lowercase().contains("limit"), "{reason}"),
			other => panic!("expected FellBackToPolling, got {other:?}"),
		}
		assert!(watcher.registered().contains(&root), "what was wanted is watched on the new backend");

		fs::write(root.join("seen_by_polling"), b"").unwrap();
		assert!(events_for(&mut rx, Duration::from_secs(5)).await.iter().any(|event| reports_new_file(event, &root, &root.join("seen_by_polling"))));
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn a_native_backend_that_cannot_start_falls_back_to_polling() {
		let root = temp_dir("watch-nostart");
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = watcher_with(tx, failing_to_start(), polling(25));
		assert!(matches!(next_issue(&mut rx, Duration::from_secs(1)).await, Some(WatchIssue::FellBackToPolling(_))), "the user is told at once");

		watcher.sync_wait(set(&[&root]));
		fs::write(root.join("still_works"), b"").unwrap();
		assert!(events_for(&mut rx, Duration::from_secs(5)).await.iter().any(|event| reports_new_file(event, &root, &root.join("still_works"))));
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}

	#[tokio::test]
	async fn with_no_working_backend_watching_is_off_but_nothing_fails() {
		let root = temp_dir("watch-none");
		let (tx, mut rx) = mpsc::unbounded_channel();
		let watcher = watcher_with(tx, failing_to_start(), failing_to_start());
		assert!(matches!(next_issue(&mut rx, Duration::from_secs(1)).await, Some(WatchIssue::BackendError(text)) if text.contains("unavailable")));
		watcher.sync_wait(set(&[&root])); // must not panic or hang
		drop(watcher);
		fs::remove_dir_all(&root).unwrap();
	}
}
