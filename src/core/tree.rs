use std::{
	fs, io,
	path::{Path, PathBuf},
};

use crate::fs::{Cha, FsChange, SortPolicy, absolute_lexical};

use super::Node;

pub struct Tree {
	pub root: Node,
}

impl Tree {
	pub fn open(path: PathBuf) -> io::Result<Self> {
		// Keep the path the user navigated through, including a symlinked root.
		// The watcher maintains its own canonical-to-visible mapping.
		let path = absolute_lexical(&path)?;
		let metadata = fs::metadata(&path)?;
		if !metadata.is_dir() {
			return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not a directory: {}", path.display())));
		}
		let cha = Cha::from(metadata);
		Ok(Self { root: Node::new(path, cha) })
	}

	/// Marks a node open right away; returns `Some(true)` if its listing
	/// still needs to be fetched, `Some(false)` if it's already cached, or
	/// `None` if the path isn't in the tree.
	pub fn mark_expanded(&mut self, path: &Path) -> Option<bool> {
		Some(self.root.find_mut(path)?.mark_expanded())
	}

	pub fn collapse(&mut self, path: &Path) -> bool {
		match self.root.find_mut(path) {
			Some(node) => {
				node.collapse();
				true
			}
			None => false,
		}
	}

	pub fn collapse_subtree(&mut self, path: &Path) -> Vec<PathBuf> {
		let mut collapsed = Vec::new();
		if let Some(node) = self.root.find_mut(path) {
			node.collapse_subtree(&mut collapsed);
		}
		collapsed
	}

	/// Collapses every directory subtree beside `path`, including `path`
	/// itself when it is a directory, without affecting any other level.
	pub fn collapse_siblings(&mut self, path: &Path) -> Vec<PathBuf> {
		let mut collapsed = Vec::new();
		let Some(parent_path) = self.root.find_parent(path).map(|parent| parent.path.clone()) else { return collapsed };
		let Some(parent) = self.root.find_mut(&parent_path) else { return collapsed };
		if let Some(children) = &mut parent.children {
			for child in children.iter_mut().filter(|child| child.cha.is_dir) {
				child.collapse_subtree(&mut collapsed);
			}
		}
		collapsed
	}

	/// Collapses every cached directory below the root while keeping the
	/// root itself open, so the current directory's immediate entries remain
	/// usable after the operation.
	pub fn collapse_all(&mut self) -> Vec<PathBuf> {
		let mut collapsed = Vec::new();
		if let Some(children) = &mut self.root.children {
			for child in children {
				child.collapse_subtree(&mut collapsed);
			}
		}
		collapsed
	}

	pub fn apply_listing(&mut self, path: &Path, entries: Vec<(PathBuf, Cha)>, policy: SortPolicy) -> bool {
		match self.root.find_mut(path) {
			Some(node) => {
				node.apply_listing(entries, policy);
				true
			}
			None => false,
		}
	}

	pub fn apply_changes(&mut self, path: &Path, changes: Vec<FsChange>, policy: SortPolicy) -> bool {
		let Some(node) = self.root.find_mut(path) else {
			return false;
		};
		if node.children.is_none() {
			return false;
		}
		node.apply_changes(changes, policy);
		true
	}

	/// Records why a listing attempt for `path` failed, and collapses it so
	/// re-expanding retries instead of just re-showing the stale attempt.
	pub fn fail_listing(&mut self, path: &Path, error: String) -> bool {
		match self.root.find_mut(path) {
			Some(node) => {
				node.collapse();
				node.loading = false;
				node.load_error = Some(error);
				true
			}
			None => false,
		}
	}

	pub fn begin_incremental_listing(&mut self, path: &Path) -> bool {
		self.root.find_mut(path).is_some_and(Node::begin_incremental_listing)
	}

	pub fn append_listing(&mut self, path: &Path, entries: Vec<(PathBuf, Cha)>) -> bool {
		let Some(node) = self.root.find_mut(path) else {
			return false;
		};
		node.append_listing(entries);
		true
	}

	pub fn finish_incremental_listing(&mut self, path: &Path, policy: SortPolicy) -> Option<Vec<usize>> {
		let Some(node) = self.root.find_mut(path) else {
			return None;
		};
		Some(node.finish_incremental_listing(policy))
	}

	pub fn discard_incremental_listing(&mut self, path: &Path) -> bool {
		let Some(node) = self.root.find_mut(path) else {
			return false;
		};
		node.discard_incremental_listing();
		true
	}

	pub fn parent_of(&self, path: &Path) -> Option<PathBuf> {
		self.root.find_parent(path).map(|node| node.path.clone())
	}

	/// The directories that should be watched: every one that is open on
	/// screen. This is a pure function of the tree, so whatever watches the
	/// filesystem can be made to match it at any time.
	pub fn watch_set(&self) -> std::collections::HashSet<PathBuf> {
		let mut set = std::collections::HashSet::new();
		self.root.collect_watched(&mut set);
		set
	}

	/// The root and every expanded directory below it whose listing is loaded:
	/// what is on screen, and so what is worth reading again.
	pub fn open_dirs(&self) -> Vec<PathBuf> {
		let mut dirs = vec![self.root.path.clone()];
		self.root.collect_open_dirs(&mut dirs);
		dirs
	}

	pub fn is_loaded(&self, path: &Path) -> bool {
		self.root.find(path).is_some_and(|node| node.children.is_some())
	}

	pub fn sort(&mut self, policy: SortPolicy) {
		self.root.sort_recursive(policy);
	}
}

#[cfg(test)]
mod tests {
	use crate::fs::{Engine, LocalEngine};

	use super::*;

	/// Drives the tree the way `App` + `FsScheduler` do, minus the background
	/// task: mark expanded (immediate), then apply whatever a listing would
	/// have come back with (here, fetched inline since this test doesn't
	/// need to be async).
	fn load(tree: &mut Tree, path: &Path) {
		tree.mark_expanded(path);
		tree.apply_listing(path, LocalEngine.read_dir(path).unwrap(), SortPolicy::default());
	}

	#[test]
	fn expand_is_lazy_and_per_node() {
		let root = std::env::temp_dir().join("tuzi-tree-test");
		let nested = root.join("a/b");
		fs::create_dir_all(&nested).unwrap();
		let root = root.canonicalize().unwrap();

		let mut tree = Tree::open(root.clone()).unwrap();
		assert!(tree.root.children.is_none());

		load(&mut tree, &root);
		let a = tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("a")).unwrap();
		assert!(a.children.is_none(), "child of an unexpanded node must stay unloaded");

		load(&mut tree, &root.join("a"));
		let a = tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("a")).unwrap();
		assert_eq!(a.children.as_ref().unwrap().len(), 1);

		tree.collapse(&root.join("a"));
		let a = tree.root.children.as_ref().unwrap().iter().find(|n| n.path == root.join("a")).unwrap();
		assert!(!a.expanded);
		assert!(a.children.is_some(), "collapsing keeps the cached listing");

		fs::remove_dir_all(&root).unwrap();
	}
}
