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
	pub fn new(root: &Node, filter: Option<&Filter>, show_hidden: bool) -> Self {
		let mut projection = Self { rows: Vec::new(), positions: HashMap::new() };
		projection.rebuild(root, filter, show_hidden);
		projection
	}

	pub fn rebuild(&mut self, root: &Node, filter: Option<&Filter>, show_hidden: bool) {
		self.rows.clear();
		append_node(&mut self.rows, root, 0, &mut Vec::new(), filter, show_hidden, true);
		self.reindex();
	}

	pub fn sync_subtree(&mut self, root: &Node, path: &Path, filter: Option<&Filter>, show_hidden: bool) {
		if filter.is_some() {
			self.rebuild(root, filter, show_hidden);
			return;
		}
		let Some(&index) = self.positions.get(path) else {
			self.rebuild(root, filter, show_hidden);
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
				append_node(&mut replacement, child, depth + 1, &mut child_locator, None, show_hidden, false);
				child_locator.pop();
			}
		}
		self.rows.splice(index + 1..end, replacement);
		self.reindex();
	}

	pub fn append_children(&mut self, root: &Node, path: &Path, start: usize, filter: Option<&Filter>, show_hidden: bool) {
		if filter.is_some() {
			self.rebuild(root, filter, show_hidden);
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
			append_node(&mut added, child, depth + 1, &mut child_locator, None, show_hidden, false);
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
	show_hidden: bool,
	force: bool,
) {
	if !force && ((!show_hidden && is_hidden(node)) || filter.is_some_and(|filter| !has_visible_match(node, filter, show_hidden))) {
		return;
	}
	rows.push(VisibleRow { depth, locator: locator.clone(), path: node.path.clone() });
	if node.expanded && let Some(children) = &node.children {
		for (index, child) in children.iter().enumerate() {
			locator.push(index);
			append_node(rows, child, depth + 1, locator, filter, show_hidden, false);
			locator.pop();
		}
	}
}

fn is_hidden(node: &Node) -> bool {
	node.path.file_name().is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

fn has_visible_match(node: &Node, filter: &Filter, show_hidden: bool) -> bool {
	if !show_hidden && is_hidden(node) {
		return false;
	}
	node.path.file_name().is_some_and(|name| filter.matches(&name.to_string_lossy()))
		|| (node.expanded
			&& node.children.as_ref().is_some_and(|children| {
				children.iter().any(|child| has_visible_match(child, filter, show_hidden))
			}))
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
		let mut projection = VisibleProjection::new(&root, None, true);
		assert_eq!(projection.len(), 4);

		root.children.as_mut().unwrap()[0].expanded = false;
		projection.sync_subtree(&root, Path::new("a"), None, true);

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
		let projection = VisibleProjection::new(&root, Some(&filter), true);

		assert_eq!(projection.len(), 3);
		assert_eq!(projection.position(Path::new("folder")), Some(1));
		assert_eq!(projection.position(Path::new("target.txt")), Some(2));
		assert_eq!(projection.position(Path::new("other.txt")), None);
	}

	#[test]
	fn hidden_nodes_stay_out_of_the_projection_until_enabled() {
		let root = node(
			"root",
			Some(vec![node(".hidden", None, false), node("visible", None, false)]),
			true,
		);

		let hidden = VisibleProjection::new(&root, None, false);
		assert_eq!(hidden.len(), 2);
		assert_eq!(hidden.position(Path::new(".hidden")), None);

		let shown = VisibleProjection::new(&root, None, true);
		assert_eq!(shown.len(), 3);
		assert_eq!(shown.position(Path::new(".hidden")), Some(1));
	}

	#[test]
	fn filter_does_not_reveal_matches_inside_hidden_directories() {
		let root = node(
			"root",
			Some(vec![node(".hidden", Some(vec![node("target", None, false)]), true)]),
			true,
		);
		let filter = Filter::new("target".into()).unwrap();

		let projection = VisibleProjection::new(&root, Some(&filter), false);
		assert_eq!(projection.len(), 1);
		assert_eq!(projection.position(Path::new("target")), None);
	}
}
