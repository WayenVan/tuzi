#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputKind {
	Rename,
	Cd,
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
	Expand,
	Collapse,
	ToggleSelect,
	VisualSelect { unset: bool },
	Delete,
	Yank,
	Paste,
	OpenInput(InputKind),
	NewTab,
	CloseTab,
	SwitchTab(isize),
}
