use std::{io, path::Path};

#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::env;
use tokio::process::Command;

use crate::{opener::{OpenKind, OpenMode, OpenTarget}, process::{ProcessPurpose, ProcessRequest}};

pub struct OpenPlanner;

impl OpenPlanner {
	pub fn plan(mode: OpenMode, cwd: &Path, targets: &[OpenTarget]) -> io::Result<Vec<ProcessRequest>> {
		if targets.is_empty() {
			return Ok(Vec::new());
		}
		let mut requests = Vec::new();
		if mode == OpenMode::Open {
			let text: Vec<_> = targets.iter().filter(|target| target.kind() == OpenKind::Text).map(|target| &target.path).collect();
			if !text.is_empty() {
				requests.extend(open_external(OpenMode::Open, cwd, targets.iter().filter(|target| target.kind() != OpenKind::Text))?);
				requests.push(ProcessRequest::block(editor(cwd, &text)?, ProcessPurpose::Open, "editor"));
				return Ok(requests);
			}
		}

		open_external(mode, cwd, targets.iter())
	}
}

fn editor(cwd: &Path, paths: &[&std::path::PathBuf]) -> io::Result<Command> {
	#[cfg(not(target_os = "windows"))]
	let mut command = {
		let mut command = Command::new("sh");
		command.args(["-c", "exec ${VISUAL:-${EDITOR:-vi}} \"$@\"", "tuzi-editor"]);
		command
	};
	#[cfg(target_os = "windows")]
	let mut command = Command::new(env::var_os("VISUAL").or_else(|| env::var_os("EDITOR")).unwrap_or_else(|| "notepad".into()));

	command.current_dir(cwd).args(paths);
	Ok(command)
}

fn open_external<'a>(mode: OpenMode, cwd: &Path, targets: impl Iterator<Item = &'a OpenTarget>) -> io::Result<Vec<ProcessRequest>> {
	let targets: Vec<_> = targets.collect();
	if targets.is_empty() {
		return Ok(Vec::new());
	}
	let mut requests = Vec::new();

		#[cfg(target_os = "macos")]
		{
			if mode == OpenMode::Reveal {
				for target in targets {
					let mut command = Command::new("open");
					command.current_dir(cwd).arg("-R").arg(&target.path);
					requests.push(ProcessRequest::orphan(command, ProcessPurpose::Open, "reveal"));
				}
				return Ok(requests);
			}
			for kind in [OpenKind::Folder, OpenKind::Text, OpenKind::Image, OpenKind::Audio, OpenKind::Video, OpenKind::Archive, OpenKind::Other] {
				let paths: Vec<_> = targets.iter().filter(|target| target.kind() == kind).map(|target| &target.path).collect();
				if paths.is_empty() {
					continue;
				}
				let mut command = Command::new("open");
				command.current_dir(cwd).args(paths);
				requests.push(ProcessRequest::orphan(command, ProcessPurpose::Open, "open"));
			}
			return Ok(requests);
		}

		#[cfg(target_os = "linux")]
		{
			if env::var_os("DISPLAY").is_none() && env::var_os("WAYLAND_DISPLAY").is_none() {
				return Err(io::Error::new(io::ErrorKind::Unsupported, "no graphical session is available for this opener"));
			}
			for target in &targets {
				let path = if mode == OpenMode::Reveal { target.path.parent().unwrap_or(&target.path) } else { &target.path };
				let mut command = Command::new("xdg-open");
				command.current_dir(cwd).arg(path);
				requests.push(ProcessRequest::orphan(command, ProcessPurpose::Open, "xdg-open"));
			}
			return Ok(requests);
		}

		#[cfg(target_os = "windows")]
		{
			for target in &targets {
				let mut command = if mode == OpenMode::Reveal {
					let mut command = Command::new("explorer");
					command.current_dir(cwd).arg(format!("/select,{}", target.path.display()));
					command
				} else {
					let mut command = Command::new("cmd");
					command.current_dir(cwd).args(["/C", "start", ""]).arg(&target.path);
					command
				};
				requests.push(ProcessRequest::orphan(command, ProcessPurpose::Open, "open"));
			}
			return Ok(requests);
		}

		#[allow(unreachable_code)]
		Err(io::Error::new(io::ErrorKind::Unsupported, "opening files is unsupported on this platform"))
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::*;

	#[test]
	#[cfg(not(target_os = "windows"))]
	fn text_targets_become_one_blocking_editor_command() {
		let targets = [
			OpenTarget { path: PathBuf::from("one.txt"), mime: "text/plain".into() },
			OpenTarget { path: PathBuf::from("two.json"), mime: "application/json".into() },
		];
		let requests = OpenPlanner::plan(OpenMode::Open, Path::new("/tmp"), &targets).unwrap();
		assert_eq!(requests.len(), 1);
		assert_eq!(requests[0].mode(), crate::process::ProcessMode::Block);
	}
}
