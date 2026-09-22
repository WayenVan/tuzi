use std::sync::{Arc, atomic::{AtomicU64, Ordering}};

use tokio::{sync::mpsc::UnboundedSender, task::JoinHandle};

use crate::{config::Preview as PreviewConfig, event::Event, preview::{PreviewKey, render_preview}};

/// Runs at most one preview job for a tab. Results always return through
/// the application's event queue; this type never mutates Preview state.
pub struct PreviewScheduler {
	tab:        usize,
	tx:         UnboundedSender<Event>,
	ticket:     u64,
	generation: Arc<AtomicU64>,
	handle:     Option<JoinHandle<()>>,
	config:     PreviewConfig,
}

impl PreviewScheduler {
	pub fn new(tab: usize, tx: UnboundedSender<Event>, config: PreviewConfig) -> Self {
		Self { tab, tx, ticket: 0, generation: Arc::new(AtomicU64::new(0)), handle: None, config }
	}

	pub fn spawn(&mut self, key: PreviewKey) {
		self.cancel();
		self.ticket += 1;
		let ticket = self.ticket;
		let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
		let guard = self.generation.clone();
		let tx = self.tx.clone();
		let tab = self.tab;
		let config = self.config.clone();
		self.handle = Some(tokio::spawn(async move {
			let read_key = key.clone();
			let result = tokio::task::spawn_blocking(move || render_preview(&read_key, &guard, generation, &config))
				.await
				.map_err(|error| error.to_string())
				.and_then(|result| result);
			let _ = tx.send(Event::PreviewLoaded { tab, ticket, key, result });
		}));
	}

	pub fn accept(&mut self, ticket: u64) -> bool {
		if ticket != self.ticket {
			return false;
		}
		self.handle = None;
		true
	}

	pub fn cancel(&mut self) {
		self.generation.fetch_add(1, Ordering::Relaxed);
		if let Some(handle) = self.handle.take() {
			handle.abort();
		}
	}
}

impl Drop for PreviewScheduler {
	fn drop(&mut self) { self.cancel(); }
}
