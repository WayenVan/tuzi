use std::path::PathBuf;

#[cfg(test)]
const DEFAULT_MAX_ENTRIES: usize = 60;

#[derive(Debug)]
pub struct PathHistory {
	entries: Vec<HistoryEntry>,
	cursor:  usize,
	max_entries: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryEntry {
	pub path:     PathBuf,
	pub cursor:   Option<PathBuf>,
	pub expanded: Vec<PathBuf>,
}

impl PathHistory {
	pub fn new(initial: PathBuf, max_entries: usize) -> Self {
		Self { entries: vec![HistoryEntry { path: initial, cursor: None, expanded: Vec::new() }], cursor: 0, max_entries }
	}

	pub fn push(&mut self, path: PathBuf) {
		if self.current().is_some_and(|entry| entry.path == path) {
			return;
		}
		self.cursor += 1;
		self.entries.truncate(self.cursor);
		self.entries.push(HistoryEntry { path, cursor: None, expanded: Vec::new() });
		if self.entries.len() > self.max_entries {
			let excess = self.entries.len() - self.max_entries;
			self.entries.drain(..excess);
			self.cursor -= excess;
		}
	}

	pub fn remember_view(&mut self, cursor: Option<PathBuf>, expanded: Vec<PathBuf>) {
		if let Some(entry) = self.entries.get_mut(self.cursor) {
			entry.cursor = cursor;
			entry.expanded = expanded;
		}
	}

	pub fn back(&mut self) -> Option<HistoryEntry> {
		if self.cursor == 0 {
			return None;
		}
		self.cursor -= 1;
		self.current().cloned()
	}

	pub fn forward(&mut self) -> Option<HistoryEntry> {
		if self.cursor + 1 >= self.entries.len() {
			return None;
		}
		self.cursor += 1;
		self.current().cloned()
	}

	fn current(&self) -> Option<&HistoryEntry> { self.entries.get(self.cursor) }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn back_forward_and_new_branches_match_browser_history() {
		let mut history = PathHistory::new("a".into(), DEFAULT_MAX_ENTRIES);
		history.push("b".into());
		history.push("c".into());
		assert_eq!(history.back().map(|entry| entry.path), Some(PathBuf::from("b")));
		assert_eq!(history.back().map(|entry| entry.path), Some(PathBuf::from("a")));
		assert_eq!(history.back(), None);
		assert_eq!(history.forward().map(|entry| entry.path), Some(PathBuf::from("b")));

		history.push("d".into());
		assert_eq!(history.forward(), None);
		assert_eq!(history.back().map(|entry| entry.path), Some(PathBuf::from("b")));
	}

	#[test]
	fn duplicate_current_paths_are_ignored_and_history_is_bounded() {
		let mut history = PathHistory::new("0".into(), DEFAULT_MAX_ENTRIES);
		history.push("0".into());
		for i in 1..=DEFAULT_MAX_ENTRIES {
			history.push(i.to_string().into());
		}

		assert_eq!(history.entries.len(), DEFAULT_MAX_ENTRIES);
		assert_eq!(history.back().map(|entry| entry.path), Some(PathBuf::from((DEFAULT_MAX_ENTRIES - 1).to_string())));
	}

	#[test]
	fn cursor_is_saved_with_each_history_entry() {
		let mut history = PathHistory::new("a".into(), DEFAULT_MAX_ENTRIES);
		history.remember_view(Some("a/one".into()), vec!["a/open".into()]);
		history.push("b".into());
		history.remember_view(Some("b/two".into()), vec!["b/open".into()]);

		let a = history.back().unwrap();
		assert_eq!(a.cursor, Some(PathBuf::from("a/one")));
		assert_eq!(a.expanded, vec![PathBuf::from("a/open")]);
		let b = history.forward().unwrap();
		assert_eq!(b.cursor, Some(PathBuf::from("b/two")));
		assert_eq!(b.expanded, vec![PathBuf::from("b/open")]);
	}
}
