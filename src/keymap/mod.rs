mod binding;
mod config;
mod key;
mod router;

pub use binding::{Binding, KeyContext};
pub use key::Key;
pub use router::{Route, Router, WhichCandidate};

pub struct Keymap {
	bindings: Vec<Binding>,
}

impl Default for Keymap {
	fn default() -> Self { Self::load(&crate::config::LoadOptions { config_dir: None, no_config: true }).expect("built-in keymap must be valid") }
}

impl Keymap {
	pub fn load(options: &crate::config::LoadOptions) -> Result<Self, String> { config::load(options) }

	pub fn new(bindings: Vec<Binding>) -> Result<Self, String> {
		for (i, binding) in bindings.iter().enumerate() {
			if binding.keys.is_empty() {
				return Err(format!("binding {i} has no keys"));
			}
			if binding.commands.is_empty() {
				return Err(format!("binding {i} has no commands"));
			}
			if binding.description.is_empty() {
				return Err(format!("binding {i} has no description"));
			}
			for previous in &bindings[..i] {
				if previous.context == binding.context
					&& (previous.keys.starts_with(&binding.keys) || binding.keys.starts_with(&previous.keys))
				{
					return Err(format!("binding {i} conflicts with an existing key sequence"));
				}
			}
		}
		Ok(Self { bindings })
	}

	pub fn bindings(&self, context: KeyContext) -> impl Iterator<Item = &Binding> {
		self.bindings.iter().filter(move |binding| binding.context == context)
	}
}

#[cfg(test)]
mod tests {
	use crate::command::Command;

	use super::*;

	#[test]
	fn rejects_ambiguous_prefixes() {
		let bindings = vec![
			Binding::new(KeyContext::Manager, vec![Key::char('g')], Command::Quit, "short"),
			Binding::new(KeyContext::Manager, vec![Key::char('g'), Key::char('g')], Command::Quit, "long"),
		];
		assert!(Keymap::new(bindings).is_err());
	}
}
