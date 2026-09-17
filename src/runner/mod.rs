use std::{collections::HashMap, env, ffi::OsString, io, path::Path};

use tokio::process::Command;

use crate::{opener::{OpenConfig, OpenTarget, Opener}, process::{ProcessPurpose, ProcessRequest}};

pub struct OpenPlanner;

impl OpenPlanner {
	pub fn plan_default(config: &OpenConfig, cwd: &Path, targets: &[OpenTarget]) -> io::Result<Vec<ProcessRequest>> {
		let mut order = Vec::new();
		let mut groups: HashMap<&str, Vec<&OpenTarget>> = HashMap::new();
		for target in targets {
			let name = config.names_for(target).into_iter().find(|name| config.variant(name).is_some()).ok_or_else(|| io::Error::new(
				io::ErrorKind::NotFound,
				format!("no opener for this platform matches {} ({})", target.path.display(), target.mime),
			))?;
			if !groups.contains_key(name) { order.push(name); }
			groups.entry(name).or_default().push(target);
		}
		let mut requests = Vec::new();
		for name in order { requests.extend(plan(config, name, cwd, &groups[name])?); }
		Ok(requests)
	}

	pub fn plan_named(config: &OpenConfig, name: &str, cwd: &Path, targets: &[OpenTarget]) -> io::Result<Vec<ProcessRequest>> {
		plan(config, name, cwd, &targets.iter().collect::<Vec<_>>())
	}
}

fn plan(config: &OpenConfig, name: &str, cwd: &Path, targets: &[&OpenTarget]) -> io::Result<Vec<ProcessRequest>> {
	if targets.is_empty() { return Ok(Vec::new()); }
	let opener = config.variant(name).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("opener '{name}' has no variant for this platform")))?;
	if opener.per_file {
		targets.iter().map(|target| request(name, opener, cwd, std::slice::from_ref(target))).collect()
	} else {
		Ok(vec![request(name, opener, cwd, targets)?])
	}
}

fn request(name: &str, opener: &Opener, cwd: &Path, targets: &[&OpenTarget]) -> io::Result<ProcessRequest> {
	let mut command = Command::new(resolve_executable(&opener.run));
	command.current_dir(cwd);
	for arg in &opener.args {
		match arg.as_str() {
			"{files}" => { command.args(targets.iter().map(|target| &target.path)); }
			"{file}" => { command.arg(&targets[0].path); }
			"{dir}" => { command.arg(targets[0].path.parent().unwrap_or(&targets[0].path)); }
			_ => { command.arg(arg); }
		}
	}
	Ok(if opener.block {
		ProcessRequest::block(command, ProcessPurpose::Open, name)
	} else if opener.orphan {
		ProcessRequest::orphan(command, ProcessPurpose::Open, name)
	} else {
		ProcessRequest::wait(command, ProcessPurpose::Open, name)
	})
}

fn resolve_executable(run: &str) -> OsString {
	if run != "$EDITOR" { return run.into(); }
	env::var_os("VISUAL").or_else(|| env::var_os("EDITOR")).unwrap_or_else(|| if cfg!(windows) { "notepad".into() } else { "vi".into() })
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::*;
	use crate::{config::Config, process::ProcessMode};

	fn target(path: &str, mime: &str) -> OpenTarget { OpenTarget { path: PathBuf::from(path), mime: mime.into() } }

	#[test]
	fn default_rules_choose_editor_for_text() {
		let config = Config::default();
		let requests = OpenPlanner::plan_default(&config.opener, Path::new("/tmp"), &[target("one.txt", "text/plain")]).unwrap();
		assert_eq!(requests.len(), 1);
		assert_eq!(requests[0].mode(), ProcessMode::Block);
	}

	#[test]
	fn named_system_opener_is_detached() {
		let config = Config::default();
		let requests = OpenPlanner::plan_named(&config.opener, "open", Path::new("/tmp"), &[target("one.png", "image/png")]).unwrap();
		assert_eq!(requests.len(), 1);
		assert_eq!(requests[0].mode(), ProcessMode::Orphan);
	}
}
