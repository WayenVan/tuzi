use std::io::{self, Stdout};

use crossterm::{event::{DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture}, execute, terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode}};
use ratatui::{Terminal, backend::CrosstermBackend};

pub struct Raterm {
	pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Raterm {
	pub fn start() -> io::Result<Self> {
		enable_raw_mode()?;
		let mut stdout = io::stdout();
		execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableFocusChange)?;
		Ok(Self { terminal: Terminal::new(CrosstermBackend::new(stdout))? })
	}
}

impl Drop for Raterm {
	fn drop(&mut self) {
		let _ = disable_raw_mode();
		let _ = execute!(self.terminal.backend_mut(), DisableFocusChange, DisableMouseCapture, LeaveAlternateScreen);
	}
}
