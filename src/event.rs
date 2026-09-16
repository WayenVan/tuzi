use std::{io, path::PathBuf};

use crate::fs::Cha;
use crate::preview::{PreviewData, PreviewKey};

pub enum Event {
	Term(crossterm::event::Event),
	Redraw,

	Changed { tab: usize, path: PathBuf },
	Loaded { tab: usize, path: PathBuf, ticket: u64, result: io::Result<Vec<(PathBuf, Cha)>> },
	Deleted { tab: usize, paths: Vec<PathBuf> },
	Pasted { tab: usize, target: PathBuf },
	CompletionLoaded { tab: usize, input: u64, revision: u64, result: io::Result<Vec<String>> },
	PreviewLoaded { tab: usize, ticket: u64, key: PreviewKey, result: Result<PreviewData, String> },
}
