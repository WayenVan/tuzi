use std::{ops::Range, path::PathBuf};

use edtui::{EditorEventHandler, EditorMode, EditorState, Index2, Lines};

pub(super) enum InputPurpose {
	Rename { target: PathBuf },
	Cd { base: PathBuf },
}

pub(super) struct Completion {
	pub(super) candidates: Vec<String>,
	pub(super) selected:   usize,
}

pub(super) struct InputSession {
	pub(super) id:         u64,
	pub(super) purpose:    InputPurpose,
	pub(super) state:      EditorState,
	pub(super) handler:    EditorEventHandler,
	pub(super) revision:   u64,
	pub(super) completion: Option<Completion>,
	pub(super) completion_task: Option<tokio::task::JoinHandle<()>>,
	pub(super) error:      Option<String>,
}

impl InputSession {
	pub(super) fn new(id: u64, purpose: InputPurpose, value: &str) -> Self {
		let mut state = EditorState::new(Lines::from(value));
		state.set_single_line(true);
		state.mode = EditorMode::Insert;
		state.cursor = Index2::new(0, value.chars().count());
		Self { id, purpose, state, handler: EditorEventHandler::vim_mode(), revision: 0, completion: None, completion_task: None, error: None }
	}

	pub(super) fn title(&self) -> &'static str {
		match self.purpose {
			InputPurpose::Rename { .. } => "Rename",
			InputPurpose::Cd { .. } => "Go to directory",
		}
	}

	pub(super) fn value(&self) -> String {
		self.state.lines.to_vecs().into_iter().next().unwrap_or_default().into_iter().collect()
	}

	pub(super) fn is_cd(&self) -> bool { matches!(self.purpose, InputPurpose::Cd { .. }) }

	pub(super) fn move_completion(&mut self, delta: isize) {
		let Some(cmp) = &mut self.completion else { return };
		if cmp.candidates.is_empty() {
			return;
		}
		cmp.selected = (cmp.selected as isize + delta).rem_euclid(cmp.candidates.len() as isize) as usize;
	}

	pub(super) fn complete_selected(&mut self) -> bool {
		let Some(name) = self.completion.as_ref().and_then(|c| c.candidates.get(c.selected)).cloned() else { return false };
		let value = self.value();
		let cursor = self.state.cursor.col.min(value.chars().count());
		let Range { start, end } = completion_fragment(&value, cursor);
		let mut chars: Vec<char> = value.chars().collect();
		let replacement = format!("{name}{}", std::path::MAIN_SEPARATOR);
		chars.splice(start..end, replacement.chars());
		let value: String = chars.into_iter().collect();
		self.state.lines = Lines::from(value.as_str());
		self.state.cursor = Index2::new(0, start + replacement.chars().count());
		self.completion = None;
		self.error = None;
		true
	}
}

impl Drop for InputSession {
	fn drop(&mut self) {
		if let Some(task) = self.completion_task.take() {
			task.abort();
		}
	}
}

fn completion_fragment(value: &str, cursor: usize) -> Range<usize> {
	let chars: Vec<char> = value.chars().collect();
	let start = chars[..cursor]
		.iter()
		.rposition(|c| *c == '/' || *c == '\\')
		.map_or(0, |i| i + 1);
	start..cursor
}
