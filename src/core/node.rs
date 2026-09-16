use std::path::{Path, PathBuf};

use super::Filter;
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

	/// Like `visible`, but a node only appears if it matches `filter` itself
	/// or some visible descendant does — hiding non-matches entirely instead
	/// of just highlighting them, the way `find` does. `None` means neither,
	/// so the caller drops this node (and, since it's never called, its
	/// whole subtree) from the result.
	pub fn visible_filtered<'a>(&'a self, depth: usize, filter: &Filter) -> Option<Vec<(usize, &'a Node)>> {
		let matches = self.path.file_name().is_some_and(|name| filter.matches(&name.to_string_lossy()));

		let mut rows = vec![(depth, self)];
		let mut kept = matches;
		if self.expanded && let Some(children) = &self.children {
			for child in children {
				if let Some(child_rows) = child.visible_filtered(depth + 1, filter) {
					kept = true;
					rows.extend(child_rows);
				}
			}
		}
		kept.then_some(rows)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn cha(is_dir: bool) -> Cha { Cha { len: 0, is_dir, is_link: false, modified: None, mode: 0 } }

	fn file(name: &str) -> Node { Node::new(PathBuf::from(name), cha(false)) }

	fn dir(name: &str, children: Vec<Node>) -> Node {
		let mut node = Node::new(PathBuf::from(name), cha(true));
		node.expanded = true;
		node.children = Some(children);
		node
	}

	fn names<'a>(rows: &'a [(usize, &'a Node)]) -> Vec<&'a str> {
		rows.iter().map(|(_, node)| node.path.to_str().unwrap()).collect()
	}

	#[test]
	fn filtering_hides_non_matching_siblings_but_keeps_matches() {
		let root = dir("root", vec![file("keep.txt"), file("skip.txt")]);
		let filter = Filter::new("keep".into()).unwrap();

		let rows = root.visible_filtered(0, &filter).unwrap();
		assert_eq!(names(&rows), ["root", "keep.txt"]);
	}

	#[test]
	fn filtering_keeps_a_directory_for_a_matching_descendant_even_if_its_own_name_does_not_match() {
		let root = dir("root", vec![dir("sub", vec![file("target.txt"), file("other.txt")])]);
		let filter = Filter::new("target".into()).unwrap();

		let rows = root.visible_filtered(0, &filter).unwrap();
		assert_eq!(names(&rows), ["root", "sub", "target.txt"]);
	}

	#[test]
	fn filtering_does_not_look_inside_a_collapsed_directory() {
		let mut root = dir("root", vec![dir("sub", vec![file("target.txt")])]);
		root.children.as_mut().unwrap()[0].expanded = false;
		let filter = Filter::new("target".into()).unwrap();

		assert!(root.visible_filtered(0, &filter).is_none(), "a collapsed subtree's contents aren't visible to filter into");
	}

	#[test]
	fn filtering_returns_none_when_nothing_matches() {
		let root = dir("root", vec![file("a.txt"), file("b.txt")]);
		let filter = Filter::new("nope".into()).unwrap();

		assert!(root.visible_filtered(0, &filter).is_none());
	}
}
