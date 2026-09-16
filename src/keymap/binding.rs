use crate::action::Action;

use super::Key;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyContext {
	Manager,
}

pub struct Binding {
	pub context:     KeyContext,
	pub keys:        Vec<Key>,
	pub actions:     Vec<Action>,
	pub description: String,
}

impl Binding {
	pub fn new(context: KeyContext, keys: impl Into<Vec<Key>>, action: Action, description: impl Into<String>) -> Self {
		Self { context, keys: keys.into(), actions: vec![action], description: description.into() }
	}
}
