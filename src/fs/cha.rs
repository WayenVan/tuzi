use std::{
	fs::Metadata,
	os::unix::fs::PermissionsExt,
	path::{Path, PathBuf},
	time::SystemTime,
};

pub struct Cha {
	pub len: u64,
	pub is_dir: bool,
	pub is_link: bool,
	/// What a symlink's own `readlink` says it points at, verbatim (not
	/// resolved) — `None` unless `is_link` is set.
	pub link_target: Option<PathBuf>,
	/// A symlink whose target can't be stat'd — missing, a permission
	/// error, a loop, doesn't matter which. Meaningless unless `is_link`.
	pub link_broken: bool,
	pub modified: Option<SystemTime>,
	pub mode: u32,
}

impl From<Metadata> for Cha {
	fn from(meta: Metadata) -> Self {
		Self {
			len: meta.len(),
			is_dir: meta.is_dir(),
			is_link: meta.is_symlink(),
			link_target: None,
			link_broken: false,
			modified: meta.modified().ok(),
			mode: meta.permissions().mode(),
		}
	}
}

impl Cha {
	/// `From<Metadata>` alone can't see a symlink's target, whether it
	/// points at a directory, or whether it even resolves at all — all
	/// three need `path` itself, not just the `lstat`-style metadata
	/// already folded into `self`. A non-symlink `Cha` is left untouched.
	pub fn resolve_symlink(&mut self, path: &Path) {
		if !self.is_link {
			return;
		}
		let target = std::fs::metadata(path);
		self.is_dir = target.as_ref().is_ok_and(|meta| meta.is_dir());
		self.link_broken = target.is_err();
		self.link_target = std::fs::read_link(path).ok();
	}
}

impl Cha {
	/// Renders the Unix mode bits the way `ls -l` does: a type char, then
	/// three owner/group/other `rwx` triplets, with setuid/setgid/sticky
	/// folded into each triplet's executable slot (capitalized when the
	/// special bit is set but the plain executable bit isn't).
	pub fn permissions(&self) -> String {
		let kind = if self.is_dir {
			'd'
		} else if self.is_link {
			'l'
		} else {
			'-'
		};
		let bit = |mask: u32, ch: char| if self.mode & mask != 0 { ch } else { '-' };

		let mut out = String::with_capacity(10);
		out.push(kind);
		for (r, w, x, special, special_ch) in [(0o400, 0o200, 0o100, 0o4000, 's'), (0o040, 0o020, 0o010, 0o2000, 's'), (0o004, 0o002, 0o001, 0o1000, 't')] {
			out.push(bit(r, 'r'));
			out.push(bit(w, 'w'));
			out.push(match (self.mode & special != 0, self.mode & x != 0) {
				(true, true) => special_ch,
				(true, false) => special_ch.to_ascii_uppercase(),
				(false, true) => 'x',
				(false, false) => '-',
			});
		}
		out
	}
}

/// Formats a byte count the way `ls -h` does: one decimal place past the
/// first two digits, in whichever binary unit keeps that (`1013B`, `4.2K`,
/// `822M`, …).
pub fn format_size(bytes: u64) -> String {
	const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
	let mut size = bytes as f64;
	let mut unit = 0;
	while size >= 1024.0 && unit < UNITS.len() - 1 {
		size /= 1024.0;
		unit += 1;
	}
	if unit == 0 { format!("{bytes}B") } else { format!("{size:.1}{}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn formats_a_directory_with_setgid_and_others_writable() {
		let cha = Cha {
			len: 0,
			is_dir: true,
			is_link: false,
			link_target: None,
			link_broken: false,
			modified: None,
			mode: 0o2775,
		};
		assert_eq!(cha.permissions(), "drwxrwsr-x");
	}

	#[test]
	fn formats_a_plain_file_with_no_group_or_other_access() {
		let cha = Cha {
			len: 0,
			is_dir: false,
			is_link: false,
			link_target: None,
			link_broken: false,
			modified: None,
			mode: 0o600,
		};
		assert_eq!(cha.permissions(), "-rw-------");
	}

	#[test]
	fn formats_byte_counts_in_the_appropriate_unit() {
		assert_eq!(format_size(42), "42B");
		assert_eq!(format_size(1536), "1.5K");
		assert_eq!(format_size(5 * 1024 * 1024), "5.0M");
	}
}
