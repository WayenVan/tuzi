use std::path::PathBuf;

use super::Cha;

#[allow(dead_code)]
pub enum SortBy {
	Name,
	Size,
	Modified,
}

pub fn sort(entries: &mut [(PathBuf, Cha)], by: SortBy) {
	entries.sort_by(|(a_path, a_cha), (b_path, b_cha)| {
		b_cha.is_dir.cmp(&a_cha.is_dir).then_with(|| match by {
			SortBy::Name => a_path.file_name().cmp(&b_path.file_name()),
			SortBy::Size => a_cha.len.cmp(&b_cha.len),
			SortBy::Modified => a_cha.modified.cmp(&b_cha.modified),
		})
	});
}
