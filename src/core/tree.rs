use std::{
	fs, io,
	path::{Path, PathBuf},
};

use crate::fs::{Cha, SortPolicy};

use super::Node;

pub struct Tree {
	pub root: Node,
}

impl Tree {
	pub fn open(path: PathBuf) -> io::Result<Self> {
		// Canonicalized once here, at the root: every descendant path is a
		// plain join off of it, so the whole tree then agrees with whatever
		// realpath-resolved form the OS filesystem watcher reports back.
		let path = path.canonicalize()?;
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

	pub fn apply_listing(&mut self, path: &Path, entries: Vec<(PathBuf, Cha)>, policy: SortPolicy) -> bool {
		match self.root.find_mut(path) {
			Some(node) => {
				node.apply_listing(entries, policy);
				true
			}
			None => false,
		}
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

	pub fn finish_incremental_listing(&mut self, path: &Path, policy: SortPolicy) -> bool {
		let Some(node) = self.root.find_mut(path) else {
			return false;
		};
		node.finish_incremental_listing(policy);
		true
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
