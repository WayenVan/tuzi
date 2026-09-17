use crate::{column_mode::ColumnMode, fs::SortPolicy};

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

/// Whether an armed delete confirmation sends its targets to the system
/// trash (recoverable) or removes them outright.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteMode {
	Trash,
	Permanent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyKind {
	Path,
	Url,
	DirectoryPath,
	DirectoryUrl,
	Filename,
	Stem,
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
	CdTrash,
	CdHome,
	CdConfig,
	CdDownloads,
	CdDesktop,
	HistoryBack,
	HistoryForward,
	Expand,
	ToggleExpand,
	Collapse,
	ToggleSelect,
	VisualSelect { unset: bool },
	Delete,
	DeletePermanently,
	Yank { cut: bool },
	Paste,
	PasteLink { absolute: bool },
	Copy(CopyKind),
	OpenInput(InputKind),
	NewTab,
	CloseTab,
	SwitchTab(isize),
	SetColumnMode(ColumnMode),
	SetSort(SortPolicy),
	ToggleHidden,
	TogglePreview,
	SeekPreview(i16),
	RepeatFind { opposite: bool },
	Fzf,
	Open { interactive: bool },
	ToggleTasks,
}
