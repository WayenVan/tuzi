use std::path::{Path, PathBuf};

use crate::fs::{Cha, SortBy, sort};

pub struct Node {
	pub path:     PathBuf,
	pub cha:      Cha,
	pub expanded: bool,
	pub children: Option<Vec<Node>>,
}

impl Node {
	pub fn new(path: PathBuf, cha: Cha) -> Self { Self { path, cha, expanded: false, children: None } }

	/// Marks this node open immediately (so the UI reacts right away) without
	/// touching disk. Returns whether a listing still needs to be fetched —
	/// `false` if `children` is already cached from a previous expand.
	pub fn mark_expanded(&mut self) -> bool {
		self.expanded = true;
		self.children.is_none()
	}

	pub fn collapse(&mut self) { self.expanded = false; }

	/// Reconciles a freshly-fetched directory listing into this node,
	/// keeping the cached subtree (and expanded state) of any entry that's
	/// still present — only genuinely new or removed entries change
	/// identity. Used both for a node's first load and for watcher-driven
	/// refreshes; the caller is responsible for actually fetching the
	/// listing off-thread and only calling this once it has one.
	pub fn apply_listing(&mut self, mut entries: Vec<(PathBuf, Cha)>) {
		sort(&mut entries, SortBy::Name);

		let mut old = self.children.take().unwrap_or_default();
		self.children = Some(
			entries
				.into_iter()
				.map(|(path, cha)| match old.iter().position(|n| n.path == path) {
					Some(i) => {
						let mut node = old.remove(i);
						node.cha = cha;
						node
					}
					None => Node::new(path, cha),
				})
				.collect(),
		);
	}

	pub fn find_mut(&mut self, path: &Path) -> Option<&mut Node> {
		if self.path == *path {
			return Some(self);
		}
		self.children.as_mut()?.iter_mut().find_map(|child| child.find_mut(path))
	}

	pub fn find_parent(&self, path: &Path) -> Option<&Node> {
		let children = self.children.as_ref()?;
		if children.iter().any(|child| child.path == *path) {
			return Some(self);
		}
		children.iter().find_map(|child| child.find_parent(path))
	}

	pub fn visible(&self, depth: usize) -> Vec<(usize, &Node)> {
		let mut rows = vec![(depth, self)];
		if self.expanded && let Some(children) = &self.children {
			rows.extend(children.iter().flat_map(|child| child.visible(depth + 1)));
		}
		rows
	}
}
