use std::{
	collections::HashMap,
	path::{Path, PathBuf},
};

#[cfg(test)]
use super::Filter;
use crate::fs::{Cha, FsChange, SortBy, SortPolicy, compare_for_sort, sort};

pub struct Node {
	pub path: PathBuf,
	pub cha: Cha,
	pub expanded: bool,
	pub children: Option<Vec<Node>>,
	pub loading: bool,
	/// Set when the most recent listing attempt for this node failed
	/// (permission denied, the directory vanished, …). Unlike a toast, this
	/// stays pinned to the node — and thus visible in the tree — until the
	/// next expand attempt either clears it or replaces it with a fresh one.
	pub load_error: Option<String>,
}

impl Node {
	pub fn new(path: PathBuf, cha: Cha) -> Self {
		Self {
			path,
			cha,
			expanded: false,
			children: None,
			loading: false,
			load_error: None,
		}
	}

	/// Marks this node open immediately (so the UI reacts right away) without
	/// touching disk. Returns whether a listing still needs to be fetched —
	/// `false` if `children` is already cached from a previous expand.
	pub fn mark_expanded(&mut self) -> bool {
		self.expanded = true;
		self.load_error = None;
		let needs_fetch = self.children.is_none();
		self.loading = needs_fetch;
		needs_fetch
	}

	pub fn collapse(&mut self) {
		self.expanded = false;
		self.loading = false;
	}

	/// Adds this directory, if it is expanded, and every expanded directory
	/// below it that can be reached through expanded ones: the directories that
	/// are open on screen, whether or not their listing has arrived yet.
	pub fn collect_watched(&self, out: &mut std::collections::HashSet<PathBuf>) {
		if !self.expanded {
			return;
		}
		out.insert(self.path.clone());
		for child in self.children.iter().flatten() {
			child.collect_watched(out);
		}
	}

	/// Appends every expanded, loaded directory below this node, following
	/// only expanded ones, which are the ones actually on screen.
	pub fn collect_open_dirs(&self, out: &mut Vec<PathBuf>) {
		for child in self.children.iter().flatten() {
			if child.expanded && child.children.is_some() {
				out.push(child.path.clone());
				child.collect_open_dirs(out);
			}
		}
	}

	pub fn collapse_subtree(&mut self, collapsed: &mut Vec<PathBuf>) {
		if let Some(children) = &mut self.children {
			for child in children {
				child.collapse_subtree(collapsed);
			}
		}
		if self.expanded || self.loading {
			collapsed.push(self.path.clone());
		}
		self.collapse();
	}

	/// Reconciles a freshly-fetched directory listing into this node,
	/// keeping the cached subtree (and expanded state) of any entry that's
	/// still present — only genuinely new or removed entries change
	/// identity. Used both for a node's first load and for watcher-driven
	/// refreshes; the caller is responsible for actually fetching the
	/// listing off-thread and only calling this once it has one.
	pub fn apply_listing(&mut self, mut entries: Vec<(PathBuf, Cha)>, policy: SortPolicy) {
		self.load_error = None;
		self.loading = false;
		sort(&mut entries, policy);

		let mut old: HashMap<_, _> = self.children.take().unwrap_or_default().into_iter().map(|node| (node.path.clone(), node)).collect();
		self.children = Some(
			entries
				.into_iter()
				.map(|(path, cha)| match old.remove(&path) {
					Some(mut node) => {
						node.cha = cha;
						node
					}
					None => Node::new(path, cha),
				})
				.collect(),
		);
	}

	pub fn apply_changes(&mut self, changes: Vec<FsChange>, policy: SortPolicy) {
		let Some(children) = &mut self.children else {
			return;
		};
		let mut pending: HashMap<PathBuf, Option<Cha>> = changes
			.into_iter()
			.map(|change| match change {
				FsChange::Upsert { path, cha } => (path, Some(cha)),
				FsChange::Delete { path } => (path, None),
			})
			.collect();

		let mut reorder = matches!(policy.by, SortBy::Modified | SortBy::Size);
		children.retain_mut(|node| match pending.remove(&node.path) {
			Some(Some(cha)) => {
				reorder |= node.cha.is_dir != cha.is_dir;
				node.cha = cha;
				true
			}
			Some(None) => false,
			None => true,
		});
		let mut added: Vec<_> = pending.into_iter().filter_map(|(path, cha)| cha.map(|cha| Node::new(path, cha))).collect();
		if added.is_empty() {
			if reorder {
				children.sort_by(|a, b| compare_for_sort(&a.path, &a.cha, &b.path, &b.cha, policy));
			}
			return;
		}
		if reorder {
			children.extend(added);
			children.sort_by(|a, b| compare_for_sort(&a.path, &a.cha, &b.path, &b.cha, policy));
			return;
		}

		added.sort_by(|a, b| compare_for_sort(&a.path, &a.cha, &b.path, &b.cha, policy));
		let mut old = std::mem::take(children).into_iter().peekable();
		let mut new = added.into_iter().peekable();
		while let (Some(a), Some(b)) = (old.peek(), new.peek()) {
			if compare_for_sort(&a.path, &a.cha, &b.path, &b.cha, policy).is_le() {
				children.push(old.next().unwrap());
			} else {
				children.push(new.next().unwrap());
			}
		}
		children.extend(old);
		children.extend(new);
	}

	pub fn find_mut(&mut self, path: &Path) -> Option<&mut Node> {
		if self.path == *path {
			return Some(self);
		}
		self.children.as_mut()?.iter_mut().find_map(|child| child.find_mut(path))
	}

	pub fn find(&self, path: &Path) -> Option<&Node> {
		if self.path == *path {
			return Some(self);
		}
		self.children.as_ref()?.iter().find_map(|child| child.find(path))
	}

	pub fn begin_incremental_listing(&mut self) -> bool {
		if self.children.is_some() {
			return false;
		}
		self.load_error = None;
		self.loading = true;
		self.children = Some(Vec::new());
		true
	}

	pub fn append_listing(&mut self, entries: Vec<(PathBuf, Cha)>) {
		self.children.get_or_insert_with(Vec::new).extend(entries.into_iter().map(|(path, cha)| Node::new(path, cha)));
	}

	pub fn finish_incremental_listing(&mut self, policy: SortPolicy) -> Vec<usize> {
		let Some(children) = &mut self.children else {
			self.loading = false;
			return Vec::new();
		};
		let mut order: Vec<_> = (0..children.len()).collect();
		order.sort_by(|&a, &b| compare_for_sort(&children[a].path, &children[a].cha, &children[b].path, &children[b].cha, policy));
		let mut old: Vec<_> = std::mem::take(children).into_iter().map(Some).collect();
		children.extend(order.iter().map(|&index| old[index].take().unwrap()));
		self.loading = false;
		order
	}

	pub fn sort_recursive(&mut self, policy: SortPolicy) {
		if let Some(children) = &mut self.children {
			for child in children.iter_mut() {
				child.sort_recursive(policy);
			}
			children.sort_by(|a, b| compare_for_sort(&a.path, &a.cha, &b.path, &b.cha, policy));
		}
	}

	pub fn discard_incremental_listing(&mut self) {
		self.children = None;
		self.loading = false;
	}

	pub fn find_parent(&self, path: &Path) -> Option<&Node> {
		let children = self.children.as_ref()?;
		if children.iter().any(|child| child.path == *path) {
			return Some(self);
		}
		children.iter().find_map(|child| child.find_parent(path))
	}

	/// Walks visible nodes in display order. Returning `false` from the
	/// visitor stops immediately, allowing cursor and viewport lookups to
	/// avoid building (or even traversing) the rest of a large directory.
	#[cfg(test)]
	pub fn visit_visible<'a>(&'a self, depth: usize, visitor: &mut impl FnMut(usize, &'a Node) -> bool) -> bool {
		if !visitor(depth, self) {
			return false;
		}
		if self.expanded
			&& let Some(children) = &self.children
		{
			for child in children {
				if !child.visit_visible(depth + 1, visitor) {
					return false;
				}
			}
		}
		true
	}

	/// Like `visible`, but a node only appears if it matches `filter` itself
	/// or some visible descendant does — hiding non-matches entirely instead
	/// of just highlighting them, the way `find` does. `None` means neither,
	/// so the caller drops this node (and, since it's never called, its
	/// whole subtree) from the result.
	#[cfg(test)]
	pub fn visible_filtered<'a>(&'a self, depth: usize, filter: &Filter) -> Option<Vec<(usize, &'a Node)>> {
		let matches = self.path.file_name().is_some_and(|name| filter.matches(&name.to_string_lossy()));

		let mut rows = vec![(depth, self)];
		let mut kept = matches;
		if self.expanded
			&& let Some(children) = &self.children
		{
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

	fn cha(is_dir: bool) -> Cha {
		Cha {
			len: 0,
			is_dir,
			is_link: false,
			link_target: None,
			link_broken: false,
			modified: None,
			mode: 0,
		}
	}

	fn file(name: &str) -> Node {
		Node::new(PathBuf::from(name), cha(false))
	}

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

	#[test]
	fn apply_listing_preserves_cached_state_for_existing_nodes() {
		let mut kept = dir("kept", vec![file("nested.txt")]);
		kept.load_error = Some("old error".into());
		let mut root = dir("root", vec![file("removed.txt"), kept]);

		let mut refreshed = cha(true);
		refreshed.len = 42;
		root.apply_listing(vec![(PathBuf::from("new.txt"), cha(false)), (PathBuf::from("kept"), refreshed)], SortPolicy::default());

		let children = root.children.as_ref().unwrap();
		assert_eq!(children.iter().map(|node| node.path.as_path()).collect::<Vec<_>>(), [Path::new("kept"), Path::new("new.txt")]);

		let kept = &children[0];
		assert!(kept.expanded);
		assert_eq!(kept.cha.len, 42);
		assert_eq!(kept.load_error.as_deref(), Some("old error"));
		assert_eq!(kept.children.as_ref().unwrap()[0].path, Path::new("nested.txt"));
	}

	#[test]
	fn incremental_changes_preserve_cached_subtrees_and_apply_deletes_and_inserts() {
		let kept = dir("kept", vec![file("nested.txt")]);
		let mut root = dir("root", vec![kept, file("removed.txt")]);
		let mut refreshed = cha(true);
		refreshed.len = 42;

		root.apply_changes(
			vec![
				FsChange::Upsert { path: PathBuf::from("kept"), cha: refreshed },
				FsChange::Delete { path: PathBuf::from("removed.txt") },
				FsChange::Upsert {
					path: PathBuf::from("new.txt"),
					cha: cha(false),
				},
			],
			SortPolicy::default(),
		);

		let children = root.children.as_ref().unwrap();
		assert_eq!(children.iter().map(|node| node.path.as_path()).collect::<Vec<_>>(), [Path::new("kept"), Path::new("new.txt")]);
		assert_eq!(children[0].cha.len, 42);
		assert_eq!(children[0].children.as_ref().unwrap()[0].path, Path::new("nested.txt"));
	}

	#[test]
	fn visible_walk_stops_without_visiting_the_rest_of_a_large_directory() {
		let root = dir("root", (0..10_000).map(|i| file(&format!("file-{i}"))).collect());
		let mut visited = 0;

		root.visit_visible(0, &mut |_, _| {
			visited += 1;
			visited < 3
		});

		assert_eq!(visited, 3);
	}

	#[test]
	fn incremental_listing_is_visible_while_loading_and_sorted_when_finished() {
		let mut root = Node::new(PathBuf::from("root"), cha(true));
		assert!(root.mark_expanded());
		assert!(root.loading);
		assert!(root.begin_incremental_listing());

		root.append_listing(vec![(PathBuf::from("z.txt"), cha(false))]);
		root.append_listing(vec![(PathBuf::from("dir"), cha(true)), (PathBuf::from("a.txt"), cha(false))]);
		assert_eq!(root.children.as_ref().unwrap().len(), 3);
		assert!(root.loading);

		root.finish_incremental_listing(SortPolicy::default());
		assert!(!root.loading);
		assert_eq!(
			root.children.as_ref().unwrap().iter().map(|node| node.path.as_path()).collect::<Vec<_>>(),
			[Path::new("dir"), Path::new("a.txt"), Path::new("z.txt")]
		);
	}

	#[test]
	fn sorting_recurses_per_directory_without_flattening_the_tree() {
		let mut root = dir("root", vec![dir("nested", vec![file("small"), file("large")]), file("sibling")]);
		root.children.as_mut().unwrap()[0].children.as_mut().unwrap()[0].cha.len = 1;
		root.children.as_mut().unwrap()[0].children.as_mut().unwrap()[1].cha.len = 2;

		root.sort_recursive(SortPolicy::new(crate::fs::SortBy::Size, true));

		let nested = &root.children.as_ref().unwrap()[0];
		assert_eq!(nested.children.as_ref().unwrap().iter().map(|node| node.path.as_path()).collect::<Vec<_>>(), [Path::new("large"), Path::new("small")]);
	}
}
