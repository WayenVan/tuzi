//! One-level watches served from a few recursive ones, for FSEvents.
//!
//! notify's FSEvents backend stops and restarts its whole stream on every
//! `watch` and `unwatch`, and a restarted stream starts from "now": whatever
//! happened anywhere in the meantime is never reported. Expanding or
//! collapsing a directory would each open such a gap. FSEvents watches a
//! whole tree as cheaply as one directory, so [`Covering`] keeps one recursive
//! watch per covering root (a wanted directory with no wanted ancestor, in
//! practice the tab's root) and only restarts the stream when those change.
//! The directories the worker asks for are kept as a set, and events are let
//! through only for them and their direct entries, which is what a one-level
//! watch would have seen.

use std::{
	collections::HashSet,
	path::{Path, PathBuf},
	sync::{Arc, Mutex},
};

use notify::{Config, Event, EventHandler, EventKind, RecursiveMode, WatcherKind};

use super::{Backend, Handler, NotifyWatcher};

pub(super) struct Covering {
	inner: Backend,
	coverage: Arc<Mutex<Coverage>>,
}

impl Covering {
	/// Builds `inner` around a handler that only passes what `handler` would
	/// have received from one-level watches.
	pub(super) fn wrap(handler: Handler, inner: impl FnOnce(Handler) -> notify::Result<Backend>) -> notify::Result<Backend> {
		Ok(Box::new(Self::build(handler, inner)?))
	}

	fn build(mut handler: Handler, inner: impl FnOnce(Handler) -> notify::Result<Backend>) -> notify::Result<Self> {
		let coverage = Arc::new(Mutex::new(Coverage::default()));
		let filter = coverage.clone();
		let inner = inner(Box::new(move |result| {
			let result = match result {
				Ok(event) => match filter.lock().unwrap().filter(event) {
					Some(event) => Ok(event),
					None => return,
				},
				Err(error) => Err(error),
			};
			handler(result);
		}))?;
		Ok(Self { inner, coverage })
	}

	/// Applies a change of roots to the inner backend as one restart. The
	/// coverage lock must not be held here: stopping the stream waits for its
	/// thread, which may be waiting on that lock in the filter.
	fn apply(&mut self, change: RootChange) -> notify::Result<()> {
		if change.add.is_empty() && change.remove.is_empty() {
			return Ok(());
		}
		let mut paths = self.inner.paths_mut();
		let mut result = Ok(());
		for root in &change.add {
			if let Err(error) = paths.add(root, RecursiveMode::Recursive) {
				result = result.and(Err(error));
			}
		}
		// A root that could not be added is still covered by the ones it was
		// to replace, so keep those.
		if result.is_ok() {
			for root in &change.remove {
				let _ = paths.remove(root);
			}
		}
		// Committing even after a failure restarts the stream that `paths_mut`
		// stopped.
		paths.commit()?;
		result
	}
}

impl NotifyWatcher for Covering {
	fn new<F: EventHandler>(mut event_handler: F, config: Config) -> notify::Result<Self> {
		Self::build(Box::new(move |event| event_handler.handle_event(event)), |handler| Ok(Box::new(notify::RecommendedWatcher::new(handler, config)?)))
	}

	fn watch(&mut self, path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
		if !path.exists() {
			return Err(notify::Error::path_not_found().add_path(path.to_path_buf()));
		}
		let change = self.coverage.lock().unwrap().watch(path);
		if let Err(error) = self.apply(change) {
			// Undo the bookkeeping only: the roots it would have replaced were
			// never removed from the backend.
			self.coverage.lock().unwrap().unwatch(path);
			return Err(error);
		}
		Ok(())
	}

	fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
		let change = self.coverage.lock().unwrap().unwatch(path);
		// A directory that took over as a root may be gone by now; its own
		// reconcile will find that out.
		let _ = self.apply(change);
		Ok(())
	}

	fn kind() -> WatcherKind {
		notify::RecommendedWatcher::kind()
	}
}

/// How the recursive roots must change.
#[derive(Debug, Default, PartialEq, Eq)]
struct RootChange {
	add: Vec<PathBuf>,
	remove: Vec<PathBuf>,
}

#[derive(Default)]
struct Coverage {
	/// The directories watched one level deep, as the worker declared them.
	dirs: HashSet<PathBuf>,
	/// The members of `dirs` with no ancestor in `dirs`: what is actually
	/// watched, recursively.
	roots: HashSet<PathBuf>,
}

impl Coverage {
	fn watch(&mut self, dir: &Path) -> RootChange {
		if !self.dirs.insert(dir.to_path_buf()) || dir.ancestors().skip(1).any(|ancestor| self.roots.contains(ancestor)) {
			return RootChange::default();
		}
		let remove: Vec<PathBuf> = self.roots.iter().filter(|root| root.starts_with(dir)).cloned().collect();
		for root in &remove {
			self.roots.remove(root);
		}
		self.roots.insert(dir.to_path_buf());
		RootChange { add: vec![dir.to_path_buf()], remove }
	}

	fn unwatch(&mut self, dir: &Path) -> RootChange {
		self.dirs.remove(dir);
		if !self.roots.remove(dir) {
			return RootChange::default();
		}
		// What `dir` covered and nothing else does now: the wanted directories
		// under it with no wanted directory between them and it.
		let add: Vec<PathBuf> = self
			.dirs
			.iter()
			.filter(|candidate| candidate.starts_with(dir) && !candidate.ancestors().skip(1).take_while(|ancestor| *ancestor != dir).any(|ancestor| self.dirs.contains(ancestor)))
			.cloned()
			.collect();
		self.roots.extend(add.iter().cloned());
		RootChange { add, remove: vec![dir.to_path_buf()] }
	}

	/// Keeps what a one-level watch on `dirs` would report. A rescan is turned
	/// into a plain change of the watched directories it concerns, so that
	/// only they are read again instead of everything open.
	fn filter(&self, mut event: Event) -> Option<Event> {
		if event.need_rescan() {
			if event.paths.is_empty() {
				return Some(event);
			}
			let mut targets: HashSet<PathBuf> = HashSet::new();
			for path in &event.paths {
				targets.extend(self.dirs.iter().filter(|dir| dir.starts_with(path)).cloned());
				if let Some(parent) = path.parent().filter(|parent| self.dirs.contains(*parent)) {
					targets.insert(parent.to_path_buf());
				}
			}
			if targets.is_empty() {
				return None;
			}
			let mut targets: Vec<PathBuf> = targets.into_iter().collect();
			targets.sort();
			let mut refresh = Event::new(EventKind::Other);
			refresh.paths = targets;
			return Some(refresh);
		}
		event.paths.retain(|path| self.dirs.contains(path) || path.parent().is_some_and(|parent| self.dirs.contains(parent)));
		(!event.paths.is_empty()).then_some(event)
	}
}

#[cfg(test)]
mod tests {
	use notify::event::{CreateKind, Flag};

	use super::*;

	fn paths(paths: &[&str]) -> Vec<PathBuf> {
		paths.iter().map(PathBuf::from).collect()
	}

	fn sorted(mut change: RootChange) -> RootChange {
		change.add.sort();
		change.remove.sort();
		change
	}

	#[test]
	fn directories_under_a_root_do_not_touch_the_backend() {
		let mut coverage = Coverage::default();
		assert_eq!(coverage.watch(Path::new("/r")), RootChange { add: paths(&["/r"]), remove: vec![] });
		assert_eq!(coverage.watch(Path::new("/r/a")), RootChange::default());
		assert_eq!(coverage.watch(Path::new("/r/a/b")), RootChange::default());
		assert_eq!(coverage.unwatch(Path::new("/r/a/b")), RootChange::default());
		assert_eq!(coverage.unwatch(Path::new("/r/a")), RootChange::default());
		assert_eq!(coverage.roots, HashSet::from([PathBuf::from("/r")]));
	}

	#[test]
	fn a_new_ancestor_takes_over_the_roots_below_it() {
		let mut coverage = Coverage::default();
		coverage.watch(Path::new("/r/a"));
		coverage.watch(Path::new("/r/b/c"));
		coverage.watch(Path::new("/other"));
		assert_eq!(sorted(coverage.watch(Path::new("/r"))), RootChange { add: paths(&["/r"]), remove: paths(&["/r/a", "/r/b/c"]) });
		assert_eq!(coverage.roots, HashSet::from([PathBuf::from("/r"), PathBuf::from("/other")]));
	}

	#[test]
	fn dropping_a_root_hands_its_topmost_directories_their_own_watch() {
		let mut coverage = Coverage::default();
		for dir in ["/r", "/r/a", "/r/a/deep", "/r/b/c", "/r/b/c/d"] {
			coverage.watch(Path::new(dir));
		}
		assert_eq!(sorted(coverage.unwatch(Path::new("/r"))), RootChange { add: paths(&["/r/a", "/r/b/c"]), remove: paths(&["/r"]) });
		assert_eq!(coverage.roots, HashSet::from([PathBuf::from("/r/a"), PathBuf::from("/r/b/c")]));
	}

	#[test]
	fn a_sibling_that_merely_shares_a_prefix_is_not_covered() {
		let mut coverage = Coverage::default();
		coverage.watch(Path::new("/r/ab"));
		assert_eq!(coverage.watch(Path::new("/r/a")), RootChange { add: paths(&["/r/a"]), remove: vec![] });
	}

	#[test]
	fn only_the_watched_directories_and_their_entries_get_through() {
		let mut coverage = Coverage::default();
		coverage.watch(Path::new("/r"));
		coverage.watch(Path::new("/r/open"));
		let event = |path: &str| Event::new(EventKind::Create(CreateKind::File)).add_path(PathBuf::from(path));
		assert!(coverage.filter(event("/r")).is_some(), "the directory itself");
		assert!(coverage.filter(event("/r/file")).is_some(), "an entry of it");
		assert!(coverage.filter(event("/r/open/file")).is_some(), "an entry of an open directory");
		assert!(coverage.filter(event("/r/closed/file")).is_none(), "inside a directory nobody opened");
		let mixed = Event::new(EventKind::Other).add_path(PathBuf::from("/r/closed/x")).add_path(PathBuf::from("/r/y"));
		assert_eq!(coverage.filter(mixed).unwrap().paths, paths(&["/r/y"]));
	}

	#[test]
	fn a_rescan_refreshes_only_the_watched_directories_it_concerns() {
		let mut coverage = Coverage::default();
		for dir in ["/r", "/r/a", "/r/a/b", "/r/c"] {
			coverage.watch(Path::new(dir));
		}
		let rescan = |path: Option<&str>| {
			let event = Event::new(EventKind::Other).set_flag(Flag::Rescan);
			match path {
				Some(path) => event.add_path(PathBuf::from(path)),
				None => event,
			}
		};
		let refreshed = coverage.filter(rescan(Some("/r/a"))).unwrap();
		assert!(!refreshed.need_rescan());
		assert_eq!(refreshed.paths, paths(&["/r", "/r/a", "/r/a/b"]), "what is under it, and the listing it is an entry of");
		assert!(coverage.filter(rescan(Some("/elsewhere"))).is_none());
		assert!(coverage.filter(rescan(None)).unwrap().need_rescan(), "no path: anything may have changed");
	}

	/// A backend that records what it is asked to do.
	struct Recording(Arc<Mutex<Vec<String>>>);

	impl NotifyWatcher for Recording {
		fn new<F: EventHandler>(_event_handler: F, _config: Config) -> notify::Result<Self> {
			Ok(Self(Arc::default()))
		}
		fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
			self.0.lock().unwrap().push(format!("watch {} {recursive_mode:?}", path.display()));
			Ok(())
		}
		fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
			self.0.lock().unwrap().push(format!("unwatch {}", path.display()));
			Ok(())
		}
		fn kind() -> WatcherKind {
			WatcherKind::NullWatcher
		}
	}

	#[test]
	fn expanding_and_collapsing_inside_the_root_never_restarts_the_backend() {
		let root = std::env::temp_dir().join(format!("tuzi-covering-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(root.join("a/b")).unwrap();
		let calls = Arc::new(Mutex::new(Vec::new()));
		let recorded = calls.clone();
		let mut backend = Covering::wrap(Box::new(|_| {}), move |_| Ok(Box::new(Recording(recorded)))).unwrap();

		backend.watch(&root, RecursiveMode::NonRecursive).unwrap();
		backend.watch(&root.join("a"), RecursiveMode::NonRecursive).unwrap();
		backend.watch(&root.join("a/b"), RecursiveMode::NonRecursive).unwrap();
		backend.unwatch(&root.join("a/b")).unwrap();
		backend.unwatch(&root.join("a")).unwrap();
		assert_eq!(*calls.lock().unwrap(), vec![format!("watch {} Recursive", root.display())]);

		assert!(backend.watch(&root.join("missing"), RecursiveMode::NonRecursive).is_err(), "a directory that is not there is refused, as the native backends do");
		std::fs::remove_dir_all(&root).unwrap();
	}
}
