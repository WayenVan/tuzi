use std::{collections::HashSet, path::{Path, PathBuf}};

#[derive(Default)]
pub struct Selection(HashSet<PathBuf>);

impl Selection {
	pub fn toggle(&mut self, path: PathBuf) {
		if !self.0.remove(&path) {
			self.0.insert(path);
		}
	}

	pub fn contains(&self, path: &Path) -> bool { self.0.contains(path) }

	pub fn insert(&mut self, path: PathBuf) { self.0.insert(path); }

	pub fn is_empty(&self) -> bool { self.0.is_empty() }

	pub fn iter(&self) -> impl Iterator<Item = &PathBuf> { self.0.iter() }

	pub fn remove(&mut self, path: &Path) { self.0.remove(path); }

	pub fn clear(&mut self) { self.0.clear(); }
}
