use crossterm::event::KeyCode;

use crate::action::{Action, InputKind};

use super::{Binding, Key, KeyContext};

pub fn bindings() -> Vec<Binding> {
	use Action as A;
	use KeyContext as C;

	vec![
		Binding::new(C::Manager, vec![Key::char('q')], A::Quit, "Quit"),
		Binding::new(C::Manager, vec![Key::plain(KeyCode::Esc)], A::Escape, "Cancel"),
		Binding::new(C::Manager, vec![Key::char('j')], A::MoveCursor(1), "Move down"),
		Binding::new(C::Manager, vec![Key::plain(KeyCode::Down)], A::MoveCursor(1), "Move down"),
		Binding::new(C::Manager, vec![Key::char('k')], A::MoveCursor(-1), "Move up"),
		Binding::new(C::Manager, vec![Key::plain(KeyCode::Up)], A::MoveCursor(-1), "Move up"),
		Binding::new(C::Manager, vec![Key::char('l')], A::Expand, "Expand"),
		Binding::new(C::Manager, vec![Key::plain(KeyCode::Right)], A::Expand, "Expand"),
		Binding::new(C::Manager, vec![Key::plain(KeyCode::Enter)], A::Expand, "Expand"),
		Binding::new(C::Manager, vec![Key::char('h')], A::Collapse, "Collapse"),
		Binding::new(C::Manager, vec![Key::plain(KeyCode::Left)], A::Collapse, "Collapse"),
		Binding::new(C::Manager, vec![Key::char(' ')], A::ToggleSelect, "Toggle selection"),
		Binding::new(C::Manager, vec![Key::char('v')], A::VisualSelect { unset: false }, "Visual select"),
		Binding::new(C::Manager, vec![Key::char('V')], A::VisualSelect { unset: true }, "Visual unset"),
		Binding::new(C::Manager, vec![Key::char('d')], A::Delete, "Delete"),
		Binding::new(C::Manager, vec![Key::char('y')], A::Yank, "Yank"),
		Binding::new(C::Manager, vec![Key::char('p')], A::Paste, "Paste"),
		Binding::new(C::Manager, vec![Key::char('r')], A::OpenInput(InputKind::Rename), "Rename"),
		Binding::new(C::Manager, vec![Key::char('w')], A::CloseTab, "Close tab"),
		Binding::new(C::Manager, vec![Key::char(']')], A::SwitchTab(1), "Next tab"),
		Binding::new(C::Manager, vec![Key::char('[')], A::SwitchTab(-1), "Previous tab"),
		Binding::new(C::Manager, vec![Key::char('t'), Key::char('t')], A::NewTab, "Create tab"),
		Binding::new(C::Manager, vec![Key::char('g'), Key::char(' ')], A::OpenInput(InputKind::Cd), "Go to directory"),
	]
}

pub fn status_hint() -> &'static str {
	"j/k move  h/l collapse/expand  space select  y/p yank/paste  d delete  r rename  g<space> cd  tt new tab  [/] switch tab  w close tab  q quit"
}
