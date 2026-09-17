use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 60;

#[derive(Debug)]
pub struct PathHistory {
	entries: Vec<PathBuf>,
	cursor:  usize,
}

impl PathHistory {
	pub fn new(initial: PathBuf) -> Self { Self { entries: vec![initial], cursor: 0 } }

	pub fn push(&mut self, path: PathBuf) {
		if self.current() == Some(path.as_path()) {
			return;
		}
		self.cursor += 1;
		self.entries.truncate(self.cursor);
		self.entries.push(path);
		if self.entries.len() > MAX_ENTRIES {
			let excess = self.entries.len() - MAX_ENTRIES;
			self.entries.drain(..excess);
			self.cursor -= excess;
		}
	}

	pub fn back(&mut self) -> Option<&Path> {
		if self.cursor == 0 {
			return None;
		}
		self.cursor -= 1;
		self.current()
	}

	pub fn forward(&mut self) -> Option<&Path> {
		if self.cursor + 1 >= self.entries.len() {
			return None;
		}
		self.cursor += 1;
		self.current()
	}

	fn current(&self) -> Option<&Path> { self.entries.get(self.cursor).map(PathBuf::as_path) }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn back_forward_and_new_branches_match_browser_history() {
		let mut history = PathHistory::new("a".into());
		history.push("b".into());
		history.push("c".into());
		assert_eq!(history.back(), Some(Path::new("b")));
		assert_eq!(history.back(), Some(Path::new("a")));
		assert_eq!(history.back(), None);
		assert_eq!(history.forward(), Some(Path::new("b")));

		history.push("d".into());
		assert_eq!(history.forward(), None);
		assert_eq!(history.back(), Some(Path::new("b")));
	}

	#[test]
	fn duplicate_current_paths_are_ignored_and_history_is_bounded() {
		let mut history = PathHistory::new("0".into());
		history.push("0".into());
		for i in 1..=MAX_ENTRIES {
			history.push(i.to_string().into());
		}

		assert_eq!(history.entries.len(), MAX_ENTRIES);
		assert_eq!(history.back(), Some(Path::new(&(MAX_ENTRIES - 1).to_string())));
	}
}
