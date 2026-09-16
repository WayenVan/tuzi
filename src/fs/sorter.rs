use std::{
	cmp::Ordering,
	ffi::OsStr,
	os::unix::ffi::OsStrExt,
	path::{Path, PathBuf},
};

use super::Cha;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortBy {
	#[default]
	Name,
	Modified,
	Size,
	Extension,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SortPolicy {
	pub by: SortBy,
	pub reverse: bool,
}

impl SortPolicy {
	pub const fn new(by: SortBy, reverse: bool) -> Self {
		Self { by, reverse }
	}
}

pub fn sort(entries: &mut [(PathBuf, Cha)], policy: SortPolicy) {
	entries.sort_by(|(a_path, a_cha), (b_path, b_cha)| compare(a_path, a_cha, b_path, b_cha, policy));
}

pub(crate) fn compare(a_path: &Path, a_cha: &Cha, b_path: &Path, b_cha: &Cha, policy: SortPolicy) -> Ordering {
	let directories_first = b_cha.is_dir.cmp(&a_cha.is_dir);
	if directories_first != Ordering::Equal {
		return directories_first;
	}

	let name = || compare_text(a_path.file_name(), b_path.file_name());
	let ordering = match policy.by {
		SortBy::Name => name(),
		SortBy::Modified => a_cha.modified.cmp(&b_cha.modified).then_with(name),
		SortBy::Size => a_cha.len.cmp(&b_cha.len).then_with(name),
		SortBy::Extension => compare_text(a_path.extension(), b_path.extension()).then_with(name),
	};
	if policy.reverse { ordering.reverse() } else { ordering }
}

fn compare_text(a: Option<&OsStr>, b: Option<&OsStr>) -> Ordering {
	match (a, b) {
		(Some(a), Some(b)) => {
			let a = a.as_bytes();
			let b = b.as_bytes();
			for (&a, &b) in a.iter().zip(b) {
				match a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()) {
					Ordering::Equal => {}
					ordering => return ordering,
				}
			}
			a.len().cmp(&b.len()).then_with(|| a.cmp(b))
		}
		(None, None) => Ordering::Equal,
		(None, Some(_)) => Ordering::Less,
		(Some(_), None) => Ordering::Greater,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn cha(len: u64, is_dir: bool) -> Cha {
		Cha {
			len,
			is_dir,
			is_link: false,
			modified: None,
			mode: 0,
		}
	}

	#[test]
	fn reverse_never_moves_directories_behind_files() {
		let mut entries = vec![
			(PathBuf::from("small.txt"), cha(1, false)),
			(PathBuf::from("large-dir"), cha(100, true)),
			(PathBuf::from("large.txt"), cha(100, false)),
		];
		sort(&mut entries, SortPolicy::new(SortBy::Size, true));
		assert_eq!(
			entries.iter().map(|entry| entry.0.as_path()).collect::<Vec<_>>(),
			[Path::new("large-dir"), Path::new("large.txt"), Path::new("small.txt")]
		);
	}

	#[test]
	fn extension_sort_falls_back_to_name() {
		let mut entries = vec![
			(PathBuf::from("z.rs"), cha(0, false)),
			(PathBuf::from("a.txt"), cha(0, false)),
			(PathBuf::from("a.rs"), cha(0, false)),
		];
		sort(&mut entries, SortPolicy::new(SortBy::Extension, false));
		assert_eq!(
			entries.iter().map(|entry| entry.0.as_path()).collect::<Vec<_>>(),
			[Path::new("a.rs"), Path::new("z.rs"), Path::new("a.txt")]
		);
	}
}
