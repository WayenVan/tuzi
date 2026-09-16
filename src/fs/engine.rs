use std::{fs, io, path::{Path, PathBuf}};

use super::Cha;

pub trait Engine: Send + Sync {
	fn read_dir(&self, path: &Path) -> io::Result<Vec<(PathBuf, Cha)>>;
}

pub struct LocalEngine;

impl Engine for LocalEngine {
	fn read_dir(&self, path: &Path) -> io::Result<Vec<(PathBuf, Cha)>> {
		fs::read_dir(path)?
			.map(|entry| {
				let entry = entry?;
				let cha = Cha::from(entry.metadata()?);
				Ok((entry.path(), cha))
			})
			.collect()
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	#[test]
	fn reads_one_level_without_recursing() {
		let root = std::env::temp_dir().join("tuzi-engine-test");
		let nested = root.join("a/b");
		fs::create_dir_all(&nested).unwrap();
		fs::write(root.join("a/leaf.txt"), b"hi").unwrap();

		let entries = LocalEngine.read_dir(&root).unwrap();

		assert_eq!(entries.len(), 1);
		let (path, cha) = &entries[0];
		assert_eq!(path, &root.join("a"));
		assert!(cha.is_dir);

		fs::remove_dir_all(&root).unwrap();
	}
}
