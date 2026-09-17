use std::{ffi::OsStr, fs, io, path::{Path, PathBuf}};

pub fn remove(path: &Path) -> io::Result<()> {
	if fs::symlink_metadata(path)?.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

/// What a "paste as symlink" at `link_dir` should store for `source`: the
/// source path unchanged for an absolute link, or `source` expressed
/// relative to `link_dir` so the link keeps working if the pasted-into tree
/// moves as a whole (e.g. it's checked into version control alongside it).
pub fn symlink_target(link_dir: &Path, source: &Path, absolute: bool) -> PathBuf {
	if absolute { source.to_path_buf() } else { relative_from(link_dir, source) }
}

fn relative_from(base: &Path, target: &Path) -> PathBuf {
	let base: Vec<_> = base.components().collect();
	let target: Vec<_> = target.components().collect();
	let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();

	let mut result = PathBuf::new();
	for _ in &base[common..] {
		result.push("..");
	}
	for component in &target[common..] {
		result.push(component.as_os_str());
	}
	result
}

#[cfg(unix)]
pub fn create_symlink(content: &Path, link: &Path, _is_dir: bool) -> io::Result<()> { std::os::unix::fs::symlink(content, link) }

#[cfg(windows)]
pub fn create_symlink(content: &Path, link: &Path, is_dir: bool) -> io::Result<()> {
	if is_dir { std::os::windows::fs::symlink_dir(content, link) } else { std::os::windows::fs::symlink_file(content, link) }
}

/// Picks a free name in `dir` for `name`, appending "(copy)", "(copy 2)", …;
/// paths reserved by queued/running operations are unavailable even before
/// they exist on disk.
pub fn unique_dest_avoiding(dir: &Path, name: &OsStr, reserved: impl Fn(&Path) -> bool) -> PathBuf {
	let direct = dir.join(name);
	if !direct.exists() && !reserved(&direct) {
		return direct;
	}

	let name = name.to_string_lossy();
	let (stem, ext) = name.rsplit_once('.').map_or((&*name, ""), |(s, e)| (s, e));

	let mut n = 1u32;
	loop {
		let suffix = if n == 1 { "copy".to_owned() } else { format!("copy {n}") };
		let candidate = dir.join(if ext.is_empty() { format!("{stem} ({suffix})") } else { format!("{stem} ({suffix}).{ext}") });
		if !candidate.exists() && !reserved(&candidate) {
			return candidate;
		}
		n += 1;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn relative_symlink_target_climbs_out_to_a_sibling_subtree() {
		let content = symlink_target(Path::new("/root/a/b"), Path::new("/root/a/c/file.txt"), false);
		assert_eq!(content, PathBuf::from("../c/file.txt"));
	}

	#[test]
	fn relative_symlink_target_within_the_same_directory_has_no_ascent() {
		let content = symlink_target(Path::new("/root/a"), Path::new("/root/a/file.txt"), false);
		assert_eq!(content, PathBuf::from("file.txt"));
	}

	#[test]
	fn absolute_symlink_target_is_the_source_path_unchanged() {
		let content = symlink_target(Path::new("/root/a/b"), Path::new("/root/a/c/file.txt"), true);
		assert_eq!(content, PathBuf::from("/root/a/c/file.txt"));
	}
}
