use std::{ffi::OsStr, fs, io, path::{Path, PathBuf}};

pub fn remove(path: &Path) -> io::Result<()> {
	if fs::symlink_metadata(path)?.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
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
