use crate::command::Command;

use super::Key;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyContext {
	Manager,
}

pub struct Binding {
	pub context:     KeyContext,
	pub keys:        Vec<Key>,
	pub commands:    Vec<Command>,
	pub description: String,
}

impl Binding {
	#[cfg(test)]
	pub fn new(context: KeyContext, keys: impl Into<Vec<Key>>, command: Command, description: impl Into<String>) -> Self {
		Self { context, keys: keys.into(), commands: vec![command], description: description.into() }
	}

}
