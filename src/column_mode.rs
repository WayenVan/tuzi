use std::time::SystemTime;

use crate::{core::Node, fs::format_size};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColumnMode {
	#[default]
	None,
	Size,
	Permissions,
	Modified,
}

impl ColumnMode {
	pub fn text(self, node: &Node) -> Option<String> {
		match self {
			Self::None => None,
			Self::Size => Some(format_size(node.cha.len)),
			Self::Permissions => Some(node.cha.permissions()),
			Self::Modified => node.cha.modified.map(format_modified),
		}
	}
}

pub(crate) fn format_modified(modified: SystemTime) -> String {
	let seconds = SystemTime::now().duration_since(modified).unwrap_or_default().as_secs();
	match seconds {
		0..60 => format!("{seconds}s ago"),
		60..3600 => format!("{}m ago", seconds / 60),
		3600..86400 => format!("{}h ago", seconds / 3600),
		_ => format!("{}d ago", seconds / 86400),
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	#[test]
	fn default_mode_has_no_auxiliary_column() {
		let root = std::env::temp_dir().join("tuzi-column-mode-test");
		fs::create_dir_all(&root).unwrap();
		let node = Node::new(root.clone(), fs::metadata(&root).unwrap().into());

		assert_eq!(ColumnMode::default(), ColumnMode::None);
		assert_eq!(ColumnMode::None.text(&node), None);
		assert_eq!(ColumnMode::Size.text(&node), Some(format_size(node.cha.len)));

		fs::remove_dir_all(root).unwrap();
	}
}
