mod binding;
mod defaults;
mod key;
mod router;

pub use binding::{Binding, KeyContext};
pub use key::Key;
pub use router::{Route, Router};

pub struct Keymap {
	bindings: Vec<Binding>,
	hint:     String,
}

impl Default for Keymap {
	fn default() -> Self {
		let mut keymap = Self::new(defaults::bindings()).expect("built-in keymap must be valid");
		keymap.hint = defaults::status_hint().to_owned();
		keymap
	}
}

impl Keymap {
	pub fn new(bindings: Vec<Binding>) -> Result<Self, String> {
		for (i, binding) in bindings.iter().enumerate() {
			if binding.keys.is_empty() {
				return Err(format!("binding {i} has no keys"));
			}
			if binding.actions.is_empty() {
				return Err(format!("binding {i} has no actions"));
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
		let mut groups: Vec<(Vec<String>, String, Vec<crate::action::Action>)> = Vec::new();
		for binding in &bindings {
			let keys = binding.keys.iter().map(ToString::to_string).collect::<String>();
			if let Some((sequences, _, _)) = groups.iter_mut().find(|(_, _, actions)| *actions == binding.actions) {
				sequences.push(keys);
			} else {
				groups.push((vec![keys], binding.description.clone(), binding.actions.clone()));
			}
		}
		let hint = groups.into_iter().map(|(keys, description, _)| format!("{} {description}", keys.join("/"))).collect::<Vec<_>>().join("  ");
		Ok(Self { bindings, hint })
	}

	pub fn bindings(&self, context: KeyContext) -> impl Iterator<Item = &Binding> {
		self.bindings.iter().filter(move |binding| binding.context == context)
	}

	pub fn hint(&self) -> &str { &self.hint }
}

#[cfg(test)]
mod tests {
	use crate::action::Action;

	use super::*;

	#[test]
	fn rejects_ambiguous_prefixes() {
		let bindings = vec![
			Binding::new(KeyContext::Manager, vec![Key::char('g')], Action::Quit, "short"),
			Binding::new(KeyContext::Manager, vec![Key::char('g'), Key::char('g')], Action::Quit, "long"),
		];
		assert!(Keymap::new(bindings).is_err());
	}
}
