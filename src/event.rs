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
	InputChar(char),
	InputReplaceChar(char),
	InputBackspace,
	InputDeleteUnder,
	InputDeleteToEol,
	InputDeleteVisual,
	InputOpDelete,
	InputMoveLeft,
	InputMoveRight,
	InputMoveBol,
	InputMoveEol,
	InputMoveWordForward,
	InputMoveWordBack,
	InputMoveWordEnd,
	InputEnterInsert,
	InputEnterInsertBol,
	InputEnterAppend,
	InputEnterAppendEol,
	InputEnterReplace,
	InputToggleVisual,
	InputEscape,
	InputConfirm,

	Changed(PathBuf),
	Loaded { path: PathBuf, ticket: u64, result: io::Result<Vec<(PathBuf, Cha)>> },
	Deleted(Vec<PathBuf>),
	Pasted(PathBuf),
	Quit,
}
