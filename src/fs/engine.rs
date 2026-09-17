use std::{
	fs, io,
	path::{Path, PathBuf},
};

use super::Cha;

pub trait Engine: Send + Sync {
	fn read_dir(&self, path: &Path) -> io::Result<Vec<(PathBuf, Cha)>>;

	fn read_dir_batches(&self, path: &Path, first_batch_size: usize, batch_size: usize, emit: &mut dyn FnMut(Vec<(PathBuf, Cha)>) -> bool) -> io::Result<()> {
		let mut entries = self.read_dir(path)?.into_iter();
		let mut limit = first_batch_size;
		loop {
			let batch: Vec<_> = entries.by_ref().take(limit).collect();
			if batch.is_empty() || !emit(batch) {
				return Ok(());
			}
			limit = batch_size;
		}
	}
}

pub struct LocalEngine;

impl Engine for LocalEngine {
	fn read_dir(&self, path: &Path) -> io::Result<Vec<(PathBuf, Cha)>> {
		fs::read_dir(path)?
			.map(|entry| {
				let entry = entry?;
				let cha = cha_for(&entry)?;
				Ok((entry.path(), cha))
			})
			.collect()
	}

	fn read_dir_batches(&self, path: &Path, first_batch_size: usize, batch_size: usize, emit: &mut dyn FnMut(Vec<(PathBuf, Cha)>) -> bool) -> io::Result<()> {
		let mut limit = first_batch_size;
		let mut batch = Vec::with_capacity(limit);
		for entry in fs::read_dir(path)? {
			let entry = entry?;
			let cha = cha_for(&entry)?;
			batch.push((entry.path(), cha));
			if batch.len() == limit {
				if !emit(std::mem::take(&mut batch)) {
					return Ok(());
				}
				limit = batch_size;
				batch.reserve(limit);
			}
		}
		if !batch.is_empty() {
			emit(batch);
		}
		Ok(())
	}
}

/// A directory entry's own metadata never follows a symlink (it's `lstat`
/// under the hood), so a symlink to a directory would otherwise show up as a
/// non-expandable leaf. Resolve `is_dir` through the link when there is one;
/// a broken link, or one pointing at a file, just stays a leaf.
fn cha_for(entry: &fs::DirEntry) -> io::Result<Cha> {
	let mut cha = Cha::from(entry.metadata()?);
	cha.resolve_symlink(&entry.path());
	Ok(cha)
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

	#[cfg(unix)]
	#[test]
	fn a_symlinked_directory_is_expandable_but_still_flagged_as_a_link() {
		let root = std::env::temp_dir().join(format!("tuzi-engine-symlink-test-{}", std::process::id()));
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("real")).unwrap();
		fs::write(root.join("target-file"), b"hi").unwrap();
		std::os::unix::fs::symlink("real", root.join("dir-link")).unwrap();
		std::os::unix::fs::symlink("target-file", root.join("file-link")).unwrap();
		std::os::unix::fs::symlink("missing", root.join("broken-link")).unwrap();

		let entries: std::collections::HashMap<_, _> = LocalEngine.read_dir(&root).unwrap().into_iter().map(|(path, cha)| (path.file_name().unwrap().to_owned(), cha)).collect();

		let dir_link = &entries[std::ffi::OsStr::new("dir-link")];
		assert!(dir_link.is_link && dir_link.is_dir, "a symlink to a directory must stay a link but become expandable");
		assert_eq!(dir_link.link_target, Some(PathBuf::from("real")));
		assert!(!dir_link.link_broken);

		let file_link = &entries[std::ffi::OsStr::new("file-link")];
		assert!(file_link.is_link && !file_link.is_dir, "a symlink to a file is still just a link, not a directory");
		assert_eq!(file_link.link_target, Some(PathBuf::from("target-file")));
		assert!(!file_link.link_broken);

		let broken_link = &entries[std::ffi::OsStr::new("broken-link")];
		assert!(broken_link.is_link && !broken_link.is_dir, "a broken symlink must not become expandable");
		assert_eq!(broken_link.link_target, Some(PathBuf::from("missing")));
		assert!(broken_link.link_broken, "a dangling target must be flagged broken");

		fs::remove_dir_all(&root).unwrap();
	}
}
