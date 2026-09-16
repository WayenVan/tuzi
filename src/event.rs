use std::{io, path::PathBuf};

use crate::fs::Cha;

pub enum Event {
	Term(crossterm::event::Event),

	Changed { tab: usize, path: PathBuf },
	Loaded { tab: usize, path: PathBuf, ticket: u64, result: io::Result<Vec<(PathBuf, Cha)>> },
	Deleted { tab: usize, paths: Vec<PathBuf> },
	Pasted { tab: usize, target: PathBuf },
	CompletionLoaded { tab: usize, input: u64, revision: u64, result: io::Result<Vec<String>> },
}
