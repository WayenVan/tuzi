use crate::column_mode::ColumnMode;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputKind {
	Rename,
	Cd,
	Create,
	Find { previous: bool },
	Filter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CursorTarget {
	Top,
	Bottom,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
	Quit,
	Escape,
	MoveCursor(isize),
	MovePage(i8),
	MoveTo(CursorTarget),
	CdParent,
	CdSelected,
	Expand,
	ToggleExpand,
	Collapse,
	ToggleSelect,
	VisualSelect { unset: bool },
	Delete,
	Yank { cut: bool },
	Paste,
	OpenInput(InputKind),
	NewTab,
	CloseTab,
	SwitchTab(isize),
	SetColumnMode(ColumnMode),
	TogglePreview,
	SeekPreview(i16),
	RepeatFind { opposite: bool },
	Fzf,
	Open { interactive: bool },
}
