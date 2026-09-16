use std::{
	io::{self, Write},
	process::Stdio,
	sync::{Mutex, OnceLock},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use tokio::{io::AsyncWriteExt, process::Command};

static CONTENT: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();

/// Mirrors Yazi's clipboard strategy: retain an in-process copy, emit OSC
/// 52 unconditionally (which also works through a suitably configured tmux),
/// then try the platform clipboard helpers in the background.
pub fn set(content: Vec<u8>) {
	content.clone_into(&mut CONTENT.get_or_init(Default::default).lock().unwrap());
	let sequence = osc52(&content);
	let mut stdout = io::stdout().lock();
	let _ = stdout.write_all(sequence.as_bytes()).and_then(|()| stdout.flush());

	tokio::spawn(write_system_clipboard(content));
}

fn osc52(content: &[u8]) -> String {
	format!("\x1b]52;c;{}\x1b\\", STANDARD.encode(content))
}

async fn write_system_clipboard(content: Vec<u8>) {
	let commands: [(&str, &[&str]); 5] = [
		("pbcopy", &[]),
		("termux-clipboard-set", &[]),
		("wl-copy", &[]),
		("xclip", &["-selection", "clipboard"]),
		("xsel", &["-ib"]),
	];
	for (program, args) in commands {
		let Ok(mut child) = Command::new(program)
			.args(args)
			.stdin(Stdio::piped())
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.kill_on_drop(true)
			.spawn()
		else {
			continue;
		};
		let Some(mut stdin) = child.stdin.take() else {
			continue;
		};
		if stdin.write_all(&content).await.is_err() {
			continue;
		}
		drop(stdin);
		if child.wait().await.is_ok_and(|status| status.success()) {
			break;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn osc52_uses_the_standard_clipboard_selection_and_base64() {
		assert_eq!(osc52(b"hello"), "\x1b]52;c;aGVsbG8=\x1b\\");
	}
}
