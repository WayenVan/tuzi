use std::{io, path::{Path, PathBuf}};

use tokio::process::Command;

use crate::{notice::NoticeLevel, opener::{OpenMode, OpenPicker, OpenTarget}, process::{ProcessCompletion, ProcessOutput, ProcessPurpose, ProcessRequest}, runner::OpenPlanner};

use super::App;

impl App {
	pub(super) fn open_selected(&mut self, interactive: bool) {
		let (id, cwd, targets) = {
			let tab = self.active_tab_mut();
			(tab.id, tab.tree.root.path.clone(), tab.take_open_targets())
		};
		self.open.open(id, cwd, targets, interactive);
	}

	pub(super) fn on_open_resolved(&mut self, tab: usize, cwd: PathBuf, interactive: bool, result: io::Result<Vec<OpenTarget>>) {
		if interactive {
			if tab != self.active {
				return;
			}
			match result {
				Ok(targets) => self.open_picker = Some(OpenPicker { cwd, targets, selected: 0 }),
				Err(error) => self.raise_tab_notice(tab, NoticeLevel::Error, error.to_string()),
			}
			return;
		}
		self.enqueue_open(tab, result.and_then(|targets| OpenPlanner::plan(OpenMode::Open, &cwd, &targets)));
	}

	pub(super) fn move_open_picker(&mut self, delta: isize) {
		let Some(picker) = &mut self.open_picker else { return };
		picker.selected = (picker.selected as isize + delta).rem_euclid(OpenMode::ALL.len() as isize) as usize;
	}

	pub(super) fn submit_open_picker(&mut self) {
		let Some(picker) = self.open_picker.take() else { return };
		self.enqueue_open(self.active, OpenPlanner::plan(OpenMode::ALL[picker.selected], &picker.cwd, &picker.targets));
	}

	fn enqueue_open(&mut self, tab: usize, result: io::Result<Vec<ProcessRequest>>) {
		match result {
			Ok(requests) => self.processes.extend(requests),
			Err(error) => self.raise_tab_notice(tab, NoticeLevel::Error, error.to_string()),
		}
	}

	pub(super) fn start_fzf(&mut self) {
		let tab = self.active_tab();
		let selected: Vec<_> = tab.selection.iter().cloned().collect();
		let purpose = ProcessPurpose::Fzf {
			tab: tab.id,
			cwd: tab.tree.root.path.clone(),
			had_selection: !selected.is_empty(),
		};
		let mut command = Command::new("fzf");
		command.arg("-m").current_dir(&tab.tree.root.path);
		let request = if selected.is_empty() {
			ProcessRequest::block_capture(command, purpose, "fzf")
		} else {
			let mut input = Vec::new();
			for path in selected {
				input.extend_from_slice(path.to_string_lossy().as_bytes());
				input.push(b'\n');
			}
			ProcessRequest::block_capture_with_input(command, input, purpose, "fzf")
		};
		self.processes.push_back(request);
	}

	pub(super) fn on_process_completion(&mut self, completion: ProcessCompletion) {
		let ProcessCompletion { purpose, label, result } = completion;
		let output = match result {
			Ok(ProcessOutput::Detached) => return,
			Ok(ProcessOutput::Completed { status, .. }) if status.code() == Some(130) => return,
			Ok(ProcessOutput::Completed { status, .. }) if !status.success() => {
				self.active_tab_mut().raise(NoticeLevel::Warn, format!("{label} exited with {status}"));
				return;
			}
			Ok(output) => output,
			Err(error) => {
				self.active_tab_mut().raise(NoticeLevel::Error, format!("{label}: {error}"));
				return;
			}
		};

		match (purpose, output) {
			(ProcessPurpose::Open, _) => {},
			(ProcessPurpose::Fzf { tab, cwd, had_selection }, ProcessOutput::Completed { stdout, .. }) => {
				self.apply_fzf_output(tab, &cwd, had_selection, &stdout);
			}
			(ProcessPurpose::Fzf { .. }, ProcessOutput::Detached) => {},
		}
	}

	pub(super) fn apply_fzf_output(&mut self, tab_id: usize, cwd: &Path, had_selection: bool, stdout: &[u8]) {
		let paths = match resolve_paths(cwd, stdout) {
			Ok(paths) => paths,
			Err(error) => {
				self.raise_tab_notice(tab_id, NoticeLevel::Error, error.to_string());
				return;
			}
		};
		let Some(tab) = self.tab_mut(tab_id) else { return };
		let result = match paths.as_slice() {
			[] => Ok(()),
			[path] if path.is_dir() => tab.cd(path.clone()),
			[path] => tab.reveal(path.clone()),
			paths => {
				for path in paths {
					if had_selection {
						tab.selection.remove(path);
					} else {
						tab.selection.insert(path.clone());
					}
				}
				Ok(())
			}
		};
		if let Err(error) = result {
			tab.raise(NoticeLevel::Error, error.to_string());
		}
	}

	fn raise_tab_notice(&mut self, tab: usize, level: NoticeLevel, message: String) {
		if let Some(tab) = self.tab_mut(tab) {
			tab.raise(level, message);
		}
	}
}

fn resolve_paths(cwd: &Path, stdout: &[u8]) -> io::Result<Vec<PathBuf>> {
	String::from_utf8_lossy(stdout)
		.lines()
		.filter(|line| !line.is_empty())
		.map(|line| {
			let path = Path::new(line);
			if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) }.canonicalize()
		})
		.collect()
}
