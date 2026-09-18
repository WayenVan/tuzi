use std::{collections::{HashSet, VecDeque}, fs, path::PathBuf, sync::Arc};

use tokio::sync::mpsc::UnboundedSender;

use crate::{
	config::Config,
	event::Event,
	fs::Cha,
	session_state::{SessionState, TabState},
};

use super::tab::{ListingCompletion, Tab};

#[derive(Debug, Eq, PartialEq)]
pub(super) enum RestoreStep {
	Pending,
	Complete,
	Failed(String),
}

/// A tab that is built off-screen from a normalized snapshot. Directory
/// levels advance only after every listing in the current level completes.
pub(super) struct StagedTab {
	pub tab: Tab,
	levels: VecDeque<Vec<PathBuf>>,
	waiting: HashSet<PathBuf>,
	expanded: Vec<PathBuf>,
	cursor: Option<PathBuf>,
	selection: Vec<PathBuf>,
	finished: bool,
}

pub(super) enum SessionRestoreStep {
	Pending,
	Complete,
	Failed(String),
}

pub(super) struct StagedSession {
	tabs: Vec<StagedTab>,
	active_tab: usize,
}

impl StagedSession {
	pub fn open(first_id: usize, state: SessionState, tx: UnboundedSender<Event>, config: Arc<Config>) -> Result<Self, String> {
		let active_tab = state.active_tab;
		let tabs = state.tabs.into_iter().enumerate()
			.map(|(offset, tab)| StagedTab::open(first_id + offset, tab, tx.clone(), config.clone()))
			.collect::<Result<_, _>>()?;
		Ok(Self { tabs, active_tab })
	}

	pub fn contains(&self, id: usize) -> bool {
		self.tabs.iter().any(|staged| staged.tab.id == id)
	}

	pub fn tab_mut(&mut self, id: usize) -> Option<&mut Tab> {
		self.tabs.iter_mut().find(|staged| staged.tab.id == id).map(|staged| &mut staged.tab)
	}

	pub fn on_loaded(&mut self, id: usize, path: PathBuf, ticket: u64, result: std::io::Result<Vec<(PathBuf, Cha)>>, done: bool) -> SessionRestoreStep {
		let Some(tab) = self.tabs.iter_mut().find(|staged| staged.tab.id == id) else {
			return SessionRestoreStep::Pending;
		};
		match tab.on_loaded(path, ticket, result, done) {
			RestoreStep::Failed(error) => SessionRestoreStep::Failed(error),
			RestoreStep::Complete if self.tabs.iter().all(|tab| tab.finished) => SessionRestoreStep::Complete,
			_ => SessionRestoreStep::Pending,
		}
	}

	pub fn into_tabs(self) -> (Vec<Tab>, usize) {
		let active = self.tabs[self.active_tab].tab.id;
		(self.tabs.into_iter().map(|staged| staged.tab).collect(), active)
	}
}

impl StagedTab {
	pub fn open(id: usize, state: TabState, tx: UnboundedSender<Event>, config: Arc<Config>) -> Result<Self, String> {
		let cwd = state.cwd.clone();
		let tab = Tab::open_configured(id, cwd.clone(), tx, config)
			.map_err(|error| format!("cannot open staged tab {}: {error}", cwd.display()))?;
		let mut levels = VecDeque::new();
		for path in state.expanded.iter().filter(|path| **path != cwd) {
			let depth = path.strip_prefix(&cwd).expect("normalized expanded path must be within cwd").components().count();
			if levels.back().is_none_or(|level: &Vec<PathBuf>| {
				let first = level.first().expect("restore levels are never empty");
				first.strip_prefix(&cwd).unwrap().components().count() != depth
			}) {
				levels.push_back(Vec::new());
			}
			levels.back_mut().unwrap().push(path.clone());
		}
		Ok(Self {
			tab,
			levels,
			waiting: HashSet::from([cwd]),
			expanded: state.expanded,
			cursor: state.cursor,
			selection: state.selection,
			finished: false,
		})
	}

	pub fn on_loaded(&mut self, path: PathBuf, ticket: u64, result: std::io::Result<Vec<(PathBuf, Cha)>>, done: bool) -> RestoreStep {
		let completion = self.tab.on_loaded(path.clone(), ticket, result, done);
		match completion {
			Some(ListingCompletion::Failed(error)) if self.waiting.contains(&path) => {
				return RestoreStep::Failed(format!("cannot list {} while restoring: {error}", path.display()));
			}
			Some(ListingCompletion::Loaded) => { self.waiting.remove(&path); }
			_ => return RestoreStep::Pending,
		}
		if !self.waiting.is_empty() { return RestoreStep::Pending }
		self.advance()
	}

	fn advance(&mut self) -> RestoreStep {
		while let Some(level) = self.levels.pop_front() {
			for path in level {
				match self.tab.restore_expand(&path) {
					Ok(true) => { self.waiting.insert(path); }
					Ok(false) => {}
					Err(error) => return RestoreStep::Failed(error),
				}
			}
			if !self.waiting.is_empty() { return RestoreStep::Pending }
		}
		self.finish()
	}

	fn finish(&mut self) -> RestoreStep {
		if self.finished { return RestoreStep::Complete }
		for path in &self.expanded {
			let valid = fs::metadata(path).is_ok_and(|metadata| metadata.is_dir())
				&& self.tab.tree.root.find(path).is_some_and(|node| node.expanded);
			if !valid {
				return RestoreStep::Failed(format!("expanded path disappeared while restoring: {}", path.display()));
			}
		}
		if let Some(cursor) = &self.cursor {
			if !cursor.exists() || !self.tab.restore_cursor(cursor) {
				return RestoreStep::Failed(format!("cursor disappeared while restoring: {}", cursor.display()));
			}
		}
		if let Some(path) = self.selection.iter().find(|path| !path.exists()) {
			return RestoreStep::Failed(format!("selection path disappeared while restoring: {}", path.display()));
		}
		self.tab.set_selection(std::mem::take(&mut self.selection));
		self.finished = true;
		RestoreStep::Complete
	}
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicUsize, Ordering};

	use tokio::sync::mpsc;

	use crate::session_state::{SESSION_STATE_VERSION, SessionState, validate_and_normalize};

	use super::*;

	static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

	fn test_root() -> PathBuf {
		let root = std::env::temp_dir().join(format!("tuzi-staged-tab-{}-{}", std::process::id(), NEXT_DIR.fetch_add(1, Ordering::Relaxed)));
		fs::create_dir(&root).unwrap();
		root
	}

	async fn restore(state: TabState) -> Result<StagedTab, String> {
		let normalized = validate_and_normalize(SessionState { version: SESSION_STATE_VERSION, active_tab: 0, tabs: vec![state] }).unwrap();
		let (tx, mut rx) = mpsc::unbounded_channel();
		let mut staged = StagedTab::open(42, normalized.tabs.into_iter().next().unwrap(), tx, Arc::new(Config::default()))?;
		loop {
			let event = rx.recv().await.ok_or("restore event channel closed")?;
			if let Event::Loaded { tab, path, ticket, result, done } = event {
				assert_eq!(tab, 42);
				match staged.on_loaded(path, ticket, result, done) {
					RestoreStep::Pending => {}
					RestoreStep::Complete => return Ok(staged),
					RestoreStep::Failed(error) => return Err(error),
				}
			}
		}
	}

	#[tokio::test]
	async fn restores_multiple_branches_by_level_then_cursor_and_selection() {
		let root = test_root();
		fs::create_dir_all(root.join("a/deep")).unwrap();
		fs::create_dir_all(root.join("b/deep")).unwrap();
		fs::write(root.join("a/deep/cursor"), "").unwrap();
		fs::write(root.join("selected"), "").unwrap();
		let staged = restore(TabState {
			cwd: root.clone(),
			cursor: Some(root.join("a/deep/cursor")),
			selection: vec![root.join("selected")],
			expanded: vec![root.join("b"), root.join("b/deep")],
		}).await.unwrap();
		assert_eq!(staged.tab.visible_at(staged.tab.cursor).unwrap().1.path, root.canonicalize().unwrap().join("a/deep/cursor"));
		assert!(staged.tab.selection.contains(&root.canonicalize().unwrap().join("selected")));
		assert!(staged.tab.tree.root.find(&root.canonicalize().unwrap().join("b/deep")).unwrap().expanded);
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn selection_does_not_expand_its_parent() {
		let root = test_root();
		fs::create_dir(root.join("closed")).unwrap();
		fs::write(root.join("closed/selected"), "").unwrap();
		let staged = restore(TabState {
			cwd: root.clone(), cursor: None, selection: vec![root.join("closed/selected")], expanded: Vec::new(),
		}).await.unwrap();
		let cwd = root.canonicalize().unwrap();
		assert!(!staged.tab.tree.root.find(&cwd.join("closed")).unwrap().expanded);
		assert!(staged.tab.selection.contains(&cwd.join("closed/selected")));
		fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn fails_if_an_expanded_directory_disappears_after_validation() {
		let root = test_root();
		fs::create_dir(root.join("gone")).unwrap();
		let normalized = validate_and_normalize(SessionState {
			version: SESSION_STATE_VERSION,
			active_tab: 0,
			tabs: vec![TabState {
				cwd: root.clone(), cursor: None, selection: Vec::new(), expanded: vec![root.join("gone")],
			}],
		}).unwrap();
		fs::remove_dir(root.join("gone")).unwrap();
		let (tx, mut rx) = mpsc::unbounded_channel();
		let mut staged = StagedTab::open(43, normalized.tabs.into_iter().next().unwrap(), tx, Arc::new(Config::default())).unwrap();
		let error = loop {
			let Event::Loaded { path, ticket, result, done, .. } = rx.recv().await.unwrap() else { continue };
			if let RestoreStep::Failed(error) = staged.on_loaded(path, ticket, result, done) { break error }
		};
		assert!(error.contains("disappeared"));
		fs::remove_dir_all(root).unwrap();
	}
}
