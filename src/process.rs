use std::{io, path::PathBuf, process::{ExitStatus, Stdio}};

use tokio::{io::AsyncWriteExt, process::Command};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessMode {
	Block,
	BlockCapture,
	Wait,
	Orphan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessPurpose {
	Open,
	Fzf { tab: usize, cwd: PathBuf, had_selection: bool },
	/// `zoxide query -i`: routes the picked directory back to whichever tab
	/// asked for it.
	Zoxide { tab: usize },
	/// `zoxide add`: fire-and-forget, nothing to route back — it always
	/// completes as `ProcessOutput::Detached`.
	ZoxideAdd,
}

pub struct ProcessRequest {
	command: Command,
	mode:    ProcessMode,
	purpose: ProcessPurpose,
	label:   String,
	input:   Option<Vec<u8>>,
}

pub struct ProcessCompletion {
	pub purpose: ProcessPurpose,
	pub label:   String,
	pub result:  io::Result<ProcessOutput>,
}

pub enum ProcessOutput {
	Detached,
	Completed { status: ExitStatus, stdout: Vec<u8> },
}

impl ProcessRequest {
	pub fn block(mut command: Command, purpose: ProcessPurpose, label: impl Into<String>) -> Self {
		command.kill_on_drop(true).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
		Self { command, mode: ProcessMode::Block, purpose, label: label.into(), input: None }
	}

	/// The child keeps the terminal for interaction while stdout is reserved
	/// for its machine-readable result (fzf follows this convention).
	pub fn block_capture(mut command: Command, purpose: ProcessPurpose, label: impl Into<String>) -> Self {
		command.kill_on_drop(true).stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::inherit());
		Self { command, mode: ProcessMode::BlockCapture, purpose, label: label.into(), input: None }
	}

	pub fn block_capture_with_input(mut command: Command, input: Vec<u8>, purpose: ProcessPurpose, label: impl Into<String>) -> Self {
		command.kill_on_drop(true).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
		Self { command, mode: ProcessMode::BlockCapture, purpose, label: label.into(), input: Some(input) }
	}

	pub fn orphan(mut command: Command, purpose: ProcessPurpose, label: impl Into<String>) -> Self {
		command.kill_on_drop(false).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
		Self { command, mode: ProcessMode::Orphan, purpose, label: label.into(), input: None }
	}

	pub fn wait(mut command: Command, purpose: ProcessPurpose, label: impl Into<String>) -> Self {
		command.kill_on_drop(true).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
		Self { command, mode: ProcessMode::Wait, purpose, label: label.into(), input: None }
	}

	pub const fn mode(&self) -> ProcessMode { self.mode }

	pub async fn execute(mut self) -> ProcessCompletion {
		let result = match self.mode {
			ProcessMode::Block => self
				.command
				.status()
				.await
				.map(|status| ProcessOutput::Completed { status, stdout: Vec::new() }),
			ProcessMode::BlockCapture => match self.command.spawn() {
				Ok(mut child) => {
					let mut stdin = child.stdin.take();
					let input = self.input.take();
					let write_input = async move {
						if let (Some(input), Some(stdin)) = (input, stdin.as_mut()) {
							stdin.write_all(&input).await?;
						}
						Ok::<_, io::Error>(())
					};
					match tokio::try_join!(child.wait_with_output(), write_input) {
						Ok((output, ())) => Ok(ProcessOutput::Completed {
							status: output.status,
							stdout: output.stdout,
						}),
						Err(error) => Err(error),
					}
				}
				Err(error) => Err(error),
			},
			ProcessMode::Wait => self.command.status().await.map(|status| ProcessOutput::Completed { status, stdout: Vec::new() }),
			ProcessMode::Orphan => self.command.spawn().map(drop).map(|()| ProcessOutput::Detached),
		};
		ProcessCompletion { purpose: self.purpose, label: self.label, result }
	}
}

impl ProcessMode {
	pub const fn blocks_terminal(self) -> bool { matches!(self, Self::Block | Self::BlockCapture) }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	#[cfg(not(target_os = "windows"))]
	async fn block_capture_returns_stdout() {
		let mut command = Command::new("sh");
		command.args(["-c", "printf 'chosen/folder\\n'"]);
		let completion = ProcessRequest::block_capture(command, ProcessPurpose::Open, "fzf-like").execute().await;
		let ProcessOutput::Completed { status, stdout, .. } = completion.result.unwrap() else { panic!("expected captured output") };
		assert!(status.success());
		assert_eq!(stdout, b"chosen/folder\n");
	}

	#[tokio::test]
	#[cfg(not(target_os = "windows"))]
	async fn block_capture_can_feed_candidates_to_stdin() {
		let command = Command::new("cat");
		let completion = ProcessRequest::block_capture_with_input(
			command,
			b"one\ntwo\n".to_vec(),
			ProcessPurpose::Open,
			"fzf-like",
		)
		.execute()
		.await;
		let ProcessOutput::Completed { status, stdout, .. } = completion.result.unwrap() else { panic!("expected captured output") };
		assert!(status.success());
		assert_eq!(stdout, b"one\ntwo\n");
	}
}
