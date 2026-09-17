use std::collections::HashMap;

use crate::command::Command;

use super::Body;

/// A subscriber handler reacts to a published `Body` by producing the
/// `Command`s it wants run — it never mutates `App` directly, so delivery
/// stays on the same synchronous execution path as a keybinding.
pub type Handler = Box<dyn Fn(&Body) -> Vec<Command> + Send + Sync>;

/// Per-app table of `kind -> {subscriber -> handler}`. A subscriber can
/// only register once per kind; re-subscribing without unsubscribing first
/// fails, mirroring Yazi DDS's dedup-by-name behavior.
#[derive(Default)]
pub struct Registry {
	handlers: HashMap<String, HashMap<String, Handler>>,
}

impl Registry {
	pub fn new() -> Self {
		Self::default()
	}

	// No internal subscriber exists yet (phase 1 of `.ai/dds-plan.md` only
	// wires publishers); `sub`/`unsub` are exercised by the tests below
	// until phase 2/3 add a real caller.
	#[allow(dead_code)]
	pub fn sub(&mut self, subscriber: &str, kind: &str, handler: Handler) -> bool {
		let table = self.handlers.entry(kind.to_string()).or_default();
		if table.contains_key(subscriber) {
			return false;
		}
		table.insert(subscriber.to_string(), handler);
		true
	}

	#[allow(dead_code)]
	pub fn unsub(&mut self, subscriber: &str, kind: &str) {
		if let Some(table) = self.handlers.get_mut(kind) {
			table.remove(subscriber);
		}
	}

	pub fn deliver(&self, body: &Body) -> Vec<Command> {
		let Some(table) = self.handlers.get(body.kind()) else { return Vec::new() };
		table.values().flat_map(|handler| handler(body)).collect()
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::*;

	#[test]
	fn delivers_to_every_subscriber_of_a_kind() {
		let mut registry = Registry::new();
		assert!(registry.sub("a", "cd", Box::new(|_| vec![Command::ToggleTasks])));
		assert!(registry.sub("b", "cd", Box::new(|_| vec![Command::CenterCursor])));

		let commands = registry.deliver(&Body::Cd { path: PathBuf::from("/tmp") });
		assert_eq!(commands.len(), 2);
	}

	#[test]
	fn a_subscriber_cannot_register_twice_for_the_same_kind_without_unsubscribing() {
		let mut registry = Registry::new();
		assert!(registry.sub("a", "cd", Box::new(|_| Vec::new())));
		assert!(!registry.sub("a", "cd", Box::new(|_| Vec::new())));
		registry.unsub("a", "cd");
		assert!(registry.sub("a", "cd", Box::new(|_| Vec::new())));
	}

	#[test]
	fn only_matching_kinds_are_delivered() {
		let mut registry = Registry::new();
		registry.sub("a", "yank", Box::new(|_| vec![Command::ToggleTasks]));
		let commands = registry.deliver(&Body::Cd { path: PathBuf::from("/tmp") });
		assert!(commands.is_empty());
	}
}
