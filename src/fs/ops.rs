use std::{ffi::OsStr, fs, io, path::{Path, PathBuf}};

pub fn remove(path: &Path) -> io::Result<()> {
	if fs::symlink_metadata(path)?.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

pub fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
	if fs::symlink_metadata(src)?.is_dir() {
		fs::create_dir_all(dst)?;
		for entry in fs::read_dir(src)? {
			let entry = entry?;
			copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
		}
		Ok(())
	} else {
		fs::copy(src, dst).map(drop)
	}
}

/// Picks a free name in `dir` for `name`, appending "(copy)", "(copy 2)", …
/// so pasting into the folder you copied from duplicates instead of failing.
pub fn unique_dest(dir: &Path, name: &OsStr) -> PathBuf {
	let direct = dir.join(name);
	if !direct.exists() {
		return direct;
	}

	let name = name.to_string_lossy();
	let (stem, ext) = name.rsplit_once('.').map_or((&*name, ""), |(s, e)| (s, e));

	let mut n = 1u32;
	loop {
		let suffix = if n == 1 { "copy".to_owned() } else { format!("copy {n}") };
		let candidate = dir.join(if ext.is_empty() { format!("{stem} ({suffix})") } else { format!("{stem} ({suffix}).{ext}") });
		if !candidate.exists() {
			return candidate;
		}
		n += 1;
	}
}
