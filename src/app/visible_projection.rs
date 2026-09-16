use std::{collections::HashMap, ops::Range, path::{Path, PathBuf}};

use crate::core::{Filter, Node, Tree};

/// An owned, indexable projection of the tree's visible rows. It stores
/// structural child indices rather than references into `Tree`, avoiding a
/// self-referential `Tab` while keeping row lookup proportional to tree depth.
pub struct VisibleProjection {
	rows:      Vec<VisibleRow>,
	positions: HashMap<PathBuf, usize>,
}

struct VisibleRow {
	depth:   usize,
	locator: Vec<usize>,
	path:    PathBuf,
}

impl VisibleProjection {
	pub fn new(root: &Node, filter: Option<&Filter>) -> Self {
		let mut projection = Self { rows: Vec::new(), positions: HashMap::new() };
		projection.rebuild(root, filter);
		projection
	}

	pub fn rebuild(&mut self, root: &Node, filter: Option<&Filter>) {
		self.rows.clear();
		append_node(&mut self.rows, root, 0, &mut Vec::new(), filter, true);
		self.reindex();
	}

	pub fn sync_subtree(&mut self, root: &Node, path: &Path, filter: Option<&Filter>) {
		if filter.is_some() {
			self.rebuild(root, filter);
			return;
		}
		let Some(&index) = self.positions.get(path) else {
			self.rebuild(root, filter);
			return;
		};
		let depth = self.rows[index].depth;
		let locator = self.rows[index].locator.clone();
		let end = self.descendants_end(index);
		let mut replacement = Vec::new();
		if let Some(node) = node_at(root, &locator)
			&& node.expanded
			&& let Some(children) = &node.children
		{
			let mut child_locator = locator;
			for (child_index, child) in children.iter().enumerate() {
				child_locator.push(child_index);
				append_node(&mut replacement, child, depth + 1, &mut child_locator, None, true);
				child_locator.pop();
			}
		}
		self.rows.splice(index + 1..end, replacement);
		self.reindex();
	}

	pub fn append_children(&mut self, root: &Node, path: &Path, start: usize, filter: Option<&Filter>) {
		if filter.is_some() {
			self.rebuild(root, filter);
			return;
		}
		let Some(&index) = self.positions.get(path) else { return };
		let depth = self.rows[index].depth;
		let locator = self.rows[index].locator.clone();
		let Some(children) = node_at(root, &locator).and_then(|node| node.children.as_ref()) else { return };
		let mut added = Vec::new();
		let mut child_locator = locator;
		for (child_index, child) in children.iter().enumerate().skip(start) {
			child_locator.push(child_index);
			append_node(&mut added, child, depth + 1, &mut child_locator, None, true);
			child_locator.pop();
		}
		let end = self.descendants_end(index);
		self.rows.splice(end..end, added);
		self.reindex();
	}

	pub fn len(&self) -> usize { self.rows.len() }

	pub fn position(&self, path: &Path) -> Option<usize> { self.positions.get(path).copied() }

	pub fn get<'a>(&self, tree: &'a Tree, index: usize) -> Option<(usize, &'a Node)> {
		let row = self.rows.get(index)?;
		Some((row.depth, node_at(&tree.root, &row.locator)?))
	}

	pub fn range<'a>(&self, tree: &'a Tree, range: Range<usize>) -> Vec<(usize, &'a Node)> {
		self.rows
			.get(range.start.min(self.rows.len())..range.end.min(self.rows.len()))
			.unwrap_or_default()
			.iter()
			.filter_map(|row| Some((row.depth, node_at(&tree.root, &row.locator)?)))
			.collect()
	}

	fn descendants_end(&self, index: usize) -> usize {
		let depth = self.rows[index].depth;
		self.rows[index + 1..]
			.iter()
			.position(|row| row.depth <= depth)
			.map_or(self.rows.len(), |offset| index + 1 + offset)
	}

	fn reindex(&mut self) {
		self.positions.clear();
		self.positions.extend(self.rows.iter().enumerate().map(|(index, row)| (row.path.clone(), index)));
	}
}

fn append_node(
	rows: &mut Vec<VisibleRow>,
	node: &Node,
	depth: usize,
	locator: &mut Vec<usize>,
	filter: Option<&Filter>,
	force: bool,
) {
	if !force && filter.is_some_and(|filter| !node.has_visible_match(filter)) {
		return;
	}
	rows.push(VisibleRow { depth, locator: locator.clone(), path: node.path.clone() });
	if node.expanded && let Some(children) = &node.children {
		for (index, child) in children.iter().enumerate() {
			locator.push(index);
			append_node(rows, child, depth + 1, locator, filter, false);
			locator.pop();
		}
	}
}

fn node_at<'a>(root: &'a Node, locator: &[usize]) -> Option<&'a Node> {
	let mut node = root;
	for &index in locator {
		node = node.children.as_ref()?.get(index)?;
	}
	Some(node)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::fs::Cha;

	fn cha(is_dir: bool) -> Cha { Cha { len: 0, is_dir, is_link: false, modified: None, mode: 0 } }

	fn node(path: &str, children: Option<Vec<Node>>, expanded: bool) -> Node {
		Node { path: PathBuf::from(path), cha: cha(children.is_some()), expanded, children, loading: false, load_error: None }
	}

	#[test]
	fn collapsing_replaces_only_the_projected_descendant_range() {
		let mut root = node("root", Some(vec![node("a", Some(vec![node("nested", None, false)]), true), node("b", None, false)]), true);
		let mut projection = VisibleProjection::new(&root, None);
		assert_eq!(projection.len(), 4);

		root.children.as_mut().unwrap()[0].expanded = false;
		projection.sync_subtree(&root, Path::new("a"), None);

		assert_eq!(projection.len(), 3);
		assert_eq!(projection.position(Path::new("b")), Some(2));
		assert_eq!(projection.get(&Tree { root }, 1).unwrap().1.path, Path::new("a"));
	}

	#[test]
	fn filter_keeps_matching_ancestors_and_indexes_only_projected_rows() {
		let root = node(
			"root",
			Some(vec![
				node("folder", Some(vec![node("target.txt", None, false), node("other.txt", None, false)]), true),
				node("unrelated.txt", None, false),
			]),
			true,
		);
		let filter = Filter::new("target".into()).unwrap();
		let projection = VisibleProjection::new(&root, Some(&filter));

		assert_eq!(projection.len(), 3);
		assert_eq!(projection.position(Path::new("folder")), Some(1));
		assert_eq!(projection.position(Path::new("target.txt")), Some(2));
		assert_eq!(projection.position(Path::new("other.txt")), None);
	}
}
