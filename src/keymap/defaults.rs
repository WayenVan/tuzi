use crossterm::event::{KeyCode, KeyModifiers};

use crate::{action::{Action, CursorTarget, InputKind}, column_mode::ColumnMode};

use super::{Binding, Key};

pub fn bindings() -> Vec<Binding> {
	use Action as A;

	vec![
		Binding::manager(vec![Key::char('q')], A::Quit, "Quit"),
		Binding::manager(vec![Key::plain(KeyCode::Esc)], A::Escape, "Cancel"),
		Binding::manager(vec![Key::char('j')], A::MoveCursor(1), "Move down"),
		Binding::manager(vec![Key::plain(KeyCode::Down)], A::MoveCursor(1), "Move down"),
		Binding::manager(vec![Key::char('k')], A::MoveCursor(-1), "Move up"),
		Binding::manager(vec![Key::plain(KeyCode::Up)], A::MoveCursor(-1), "Move up"),
		Binding::manager(vec![Key::new(KeyCode::Char('u'), KeyModifiers::CONTROL)], A::MovePage(-50), "Move up half page"),
		Binding::manager(vec![Key::new(KeyCode::Char('d'), KeyModifiers::CONTROL)], A::MovePage(50), "Move down half page"),
		Binding::manager(vec![Key::new(KeyCode::Char('b'), KeyModifiers::CONTROL)], A::MovePage(-100), "Move up one page"),
		Binding::manager(vec![Key::new(KeyCode::Char('f'), KeyModifiers::CONTROL)], A::MovePage(100), "Move down one page"),
		Binding::manager(vec![Key::char('g'), Key::char('g')], A::MoveTo(CursorTarget::Top), "Move to top"),
		Binding::manager(vec![Key::char('g'), Key::char('h')], A::CdParent, "Go to parent directory"),
		Binding::manager(vec![Key::char('g'), Key::char('l')], A::CdSelected, "Enter selected directory"),
		Binding::manager(vec![Key::char('G')], A::MoveTo(CursorTarget::Bottom), "Move to bottom"),
		Binding::manager(vec![Key::char('l')], A::Expand, "Expand"),
		Binding::manager(vec![Key::plain(KeyCode::Right)], A::Expand, "Expand"),
		Binding::manager(vec![Key::plain(KeyCode::Enter)], A::ToggleExpand, "Toggle directory"),
		Binding::manager(vec![Key::char('h')], A::Collapse, "Collapse"),
		Binding::manager(vec![Key::plain(KeyCode::Left)], A::Collapse, "Collapse"),
		Binding::manager(vec![Key::char(';')], A::ToggleSelect, "Toggle selection"),
		Binding::manager(vec![Key::char('v')], A::VisualSelect { unset: false }, "Visual select"),
		Binding::manager(vec![Key::char('V')], A::VisualSelect { unset: true }, "Visual unset"),
		Binding::manager(vec![Key::char('d')], A::Delete, "Delete"),
		Binding::manager(vec![Key::char('y')], A::Yank { cut: false }, "Yank selected files (copy)"),
		Binding::manager(vec![Key::char('x')], A::Yank { cut: true }, "Yank selected files (cut)"),
		Binding::manager(vec![Key::char('p')], A::Paste, "Paste"),
		Binding::manager(vec![Key::char('r')], A::OpenInput(InputKind::Rename), "Rename"),
		Binding::manager(vec![Key::char('a')], A::OpenInput(InputKind::Create), "Create a file (end with / for directories)"),
		Binding::manager(vec![Key::char('/')], A::OpenInput(InputKind::Find { previous: false }), "Find next file"),
		Binding::manager(vec![Key::char('?')], A::OpenInput(InputKind::Find { previous: true }), "Find previous file"),
		Binding::manager(vec![Key::char('n')], A::RepeatFind { opposite: false }, "Repeat find"),
		Binding::manager(vec![Key::char('N')], A::RepeatFind { opposite: true }, "Repeat find in reverse"),
		Binding::manager(vec![Key::char('f')], A::OpenInput(InputKind::Filter), "Filter files"),
		Binding::manager(vec![Key::char('z')], A::Fzf, "Jump with fzf"),
		Binding::manager(vec![Key::char('o')], A::Open { interactive: false }, "Open selected files"),
		Binding::manager(vec![Key::char('O')], A::Open { interactive: true }, "Open selected files interactively"),
		Binding::manager(vec![Key::char('w')], A::ToggleTasks, "Show task manager"),
		Binding::manager(vec![Key::char('W')], A::CloseTab, "Close tab"),
		Binding::manager(vec![Key::char(']')], A::SwitchTab(1), "Next tab"),
		Binding::manager(vec![Key::char('[')], A::SwitchTab(-1), "Previous tab"),
		Binding::manager(vec![Key::char('t'), Key::char('t')], A::NewTab, "Create tab"),
		Binding::manager(vec![Key::char('g'), Key::char(' ')], A::OpenInput(InputKind::Cd), "Go to directory"),
		Binding::manager(vec![Key::char('m'), Key::char('n')], A::SetColumnMode(ColumnMode::None), "Hide column"),
		Binding::manager(vec![Key::char('m'), Key::char('s')], A::SetColumnMode(ColumnMode::Size), "Show size column"),
		Binding::manager(vec![Key::char('m'), Key::char('p')], A::SetColumnMode(ColumnMode::Permissions), "Show permissions column"),
		Binding::manager(vec![Key::char('m'), Key::char('m')], A::SetColumnMode(ColumnMode::Modified), "Show modified column"),
		Binding::manager(vec![Key::new(KeyCode::Char('p'), KeyModifiers::CONTROL)], A::TogglePreview, "Toggle preview"),
		Binding::manager(vec![Key::new(KeyCode::Char('j'), KeyModifiers::ALT)], A::SeekPreview(1), "Scroll preview down"),
		Binding::manager(vec![Key::new(KeyCode::Char('k'), KeyModifiers::ALT)], A::SeekPreview(-1), "Scroll preview up"),
	]
}
