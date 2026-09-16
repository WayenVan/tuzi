use std::{fs::Metadata, time::SystemTime};

pub struct Cha {
	pub len:      u64,
	pub is_dir:   bool,
	#[allow(dead_code)]
	pub is_link:  bool,
	pub modified: Option<SystemTime>,
}

impl From<Metadata> for Cha {
	fn from(meta: Metadata) -> Self {
		Self {
			len:      meta.len(),
			is_dir:   meta.is_dir(),
			is_link:  meta.is_symlink(),
			modified: meta.modified().ok(),
		}
	}
}
