use std::io;

use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;

use super::Raterm;

/// Owns everything that must be released before an interactive child takes
/// over the TTY, and rebuilt after it exits.
pub struct TerminalSession {
	terminal: Option<Raterm>,
	events:   Option<EventStream>,
}

impl TerminalSession {
	pub fn start() -> io::Result<Self> { Ok(Self { terminal: Some(Raterm::start()?), events: Some(EventStream::new()) }) }

	pub fn terminal(&mut self) -> &mut Raterm { self.terminal.as_mut().expect("terminal session is active") }

	pub async fn next_event(&mut self) -> io::Result<Option<Event>> {
		match self.events.as_mut().expect("terminal event stream is active").next().await {
			Some(result) => result.map(Some),
			None => Ok(None),
		}
	}

	pub fn suspend(&mut self) {
		// EventStream::drop wakes and joins Crossterm's reader thread before
		// Raterm restores the normal screen and terminal mode.
		drop(self.events.take());
		drop(self.terminal.take());
	}

	pub fn resume(&mut self) -> io::Result<()> {
		self.terminal = Some(Raterm::start()?);
		self.events = Some(EventStream::new());
		Ok(())
	}
}
