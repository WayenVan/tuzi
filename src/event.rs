use std::{io, path::PathBuf};

use crate::fs::{Cha, FsChange};
use crate::preview::{PreviewData, PreviewKey};
use crate::opener::OpenTarget;
use crate::tasks::TaskEvent;

pub enum Event {
	Term(crossterm::event::Event),
	Redraw,

	Changed { tab: usize, path: PathBuf },
	FilesChanged { tab: usize, parent: PathBuf, changes: Vec<FsChange> },
	Loaded { tab: usize, path: PathBuf, ticket: u64, result: io::Result<Vec<(PathBuf, Cha)>>, done: bool },
	Created { tab: usize, base: PathBuf, value: String, target: PathBuf, result: io::Result<()> },
	Linked { tab: usize, target: PathBuf, result: io::Result<()> },
	CompletionLoaded { tab: usize, input: u64, revision: u64, result: io::Result<Vec<String>> },
	PreviewLoaded { tab: usize, ticket: u64, key: PreviewKey, result: Result<PreviewData, String> },
	OpenResolved { tab: usize, cwd: PathBuf, interactive: bool, result: io::Result<Vec<OpenTarget>> },
	/// A tab actually changed its root directory (not just expanded a
	/// subtree in place) — recorded into `zoxide`'s frecency database the
	/// same way a shell's `cd` hook would.
	Visited(PathBuf),
	Task(TaskEvent),
}
