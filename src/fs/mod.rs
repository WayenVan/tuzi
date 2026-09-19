mod cha;
mod engine;
mod ops;
mod path;
mod sorter;

pub use cha::{Cha, format_size};
pub use engine::{Engine, LocalEngine};
pub use ops::{create_symlink, remove, symlink_target, unique_dest_avoiding};
pub(crate) use path::absolute_lexical;
pub(crate) use sorter::compare as compare_for_sort;
pub use sorter::{SortBy, SortPolicy, sort};

pub enum FsChange {
	Upsert { path: std::path::PathBuf, cha: Cha },
	Delete { path: std::path::PathBuf },
}
