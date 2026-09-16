#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputKind {
	Rename,
	Cd,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
	Quit,
	Escape,
	MoveCursor(isize),
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
