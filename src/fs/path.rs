use std::{io, path::{Component, Path, PathBuf}};

/// Makes a path absolute and removes `.` / `..` components without resolving
/// symlinks. Navigation uses logical paths, like a shell's `cd -L`, while the
/// watcher separately canonicalizes paths when registering with the OS.
pub(crate) fn absolute_lexical(path: &Path) -> io::Result<PathBuf> {
	let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
	let mut normalized = PathBuf::new();
	for component in absolute.components() {
		match component {
			Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
			Component::RootDir => normalized.push(component.as_os_str()),
			Component::CurDir => {}
			Component::ParentDir => { normalized.pop(); }
			Component::Normal(part) => normalized.push(part),
		}
	}
	Ok(normalized)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn normalizes_parent_components_without_resolving_the_path() {
		let path = absolute_lexical(Path::new("/tmp/a/link/../sibling")).unwrap();
		assert_eq!(path, Path::new("/tmp/a/sibling"));
	}
}
