use std::{io, path::PathBuf};

use crate::fs::Cha;

pub enum Event {
	Term(crossterm::event::Event),

	MoveUp,
	MoveDown,
	Expand,
	Collapse,
	ToggleSelect,
	VisualSelect,
	VisualUnset,
	Delete,
	Yank,
	Paste,
	Escape,

	Rename,
	RenameKey(crossterm::event::KeyEvent),

	Changed(PathBuf),
	Loaded { path: PathBuf, ticket: u64, result: io::Result<Vec<(PathBuf, Cha)>> },
	Deleted(Vec<PathBuf>),
	Pasted(PathBuf),
	Quit,
}
