//! Serializable snapshots of Tuzi's tab and tree state.
//!
//! This module contains only the wire model and its protocol limits. Input
//! validation and normalization live alongside the restoration executor so
//! startup and DDS restores cannot drift apart.

use std::{
	collections::HashSet,
	fs,
	path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub const SESSION_STATE_VERSION: u32 = 1;
pub const MAX_SESSION_TABS: usize = 32;
pub const MAX_EXPANDED_PATHS_PER_TAB: usize = 256;
pub const MAX_SELECTION_PATHS_PER_RESTORE: usize = 4096;
pub const MAX_SESSION_PATH_DEPTH: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionState {
	pub version:    u32,
	pub active_tab: usize,
	/// Session-wide home directory (`g=`), shared by every tab. Optional so
	/// older snapshots stay valid; absent means "leave the current home".
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub home:       Option<PathBuf>,
	pub tabs:       Vec<TabState>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TabState {
	pub cwd:       PathBuf,
	pub cursor:    Option<PathBuf>,
	pub selection: Vec<PathBuf>,
	pub expanded:  Vec<PathBuf>,
}

/// Validates an untrusted snapshot and returns its canonical, deterministic
/// representation. This is the single entry point used by every restore
/// source; callers must not start changing App state before it succeeds.
pub fn validate_and_normalize(mut state: SessionState) -> Result<SessionState, String> {
	if state.version != SESSION_STATE_VERSION {
		return Err(format!("unsupported session state version {}; expected {SESSION_STATE_VERSION}", state.version));
	}
	if state.tabs.is_empty() {
		return Err("session state must contain at least one tab".into());
	}
	if state.tabs.len() > MAX_SESSION_TABS {
		return Err(format!("session state contains too many tabs; maximum is {MAX_SESSION_TABS}"));
	}
	if state.active_tab >= state.tabs.len() {
		return Err("active_tab is outside the tabs array".into());
	}
	let selections = state.tabs.iter().try_fold(0usize, |total, tab| total.checked_add(tab.selection.len()))
		.ok_or_else(|| "session selection count overflowed".to_owned())?;
	if selections > MAX_SELECTION_PATHS_PER_RESTORE {
		return Err(format!("session state contains too many selection paths; maximum is {MAX_SELECTION_PATHS_PER_RESTORE}"));
	}

	if let Some(home) = &mut state.home {
		let canonical = canonical_path(home, "home")?;
		if !fs::metadata(&canonical).map_err(|error| format!("cannot inspect home {}: {error}", canonical.display()))?.is_dir() {
			return Err(format!("home is not a directory: {}", canonical.display()));
		}
		*home = canonical;
	}

	for (index, tab) in state.tabs.iter_mut().enumerate() {
		if tab.expanded.len() > MAX_EXPANDED_PATHS_PER_TAB {
			return Err(format!("tab {index} contains too many expanded paths; maximum is {MAX_EXPANDED_PATHS_PER_TAB}"));
		}
		normalize_tab(tab).map_err(|error| format!("invalid tab {index}: {error}"))?;
	}
	Ok(state)
}

fn normalize_tab(tab: &mut TabState) -> Result<(), String> {
	let cwd = canonical_path(&tab.cwd, "cwd")?;
	if !fs::metadata(&cwd).map_err(|error| format!("cannot inspect cwd {}: {error}", cwd.display()))?.is_dir() {
		return Err(format!("cwd is not a directory: {}", cwd.display()));
	}
	tab.cwd = cwd.clone();

	if let Some(cursor) = &mut tab.cursor {
		*cursor = canonical_descendant(&cwd, cursor, "cursor", false)?;
	}
	for selection in &mut tab.selection {
		*selection = canonical_descendant(&cwd, selection, "selection", false)?;
	}

	let mut expanded = HashSet::with_capacity(tab.expanded.len());
	for path in &tab.expanded {
		expanded.insert(canonical_descendant(&cwd, path, "expanded", true)?);
	}
	if let Some(cursor) = &tab.cursor {
		let mut ancestor = cursor.parent();
		while let Some(path) = ancestor {
			if path == cwd { break }
			if !path.starts_with(&cwd) { break }
			expanded.insert(path.to_owned());
			ancestor = path.parent();
		}
	}
	let mut expanded: Vec<_> = expanded.into_iter().collect();
	expanded.sort_by(|left, right| {
		path_depth(&cwd, left).cmp(&path_depth(&cwd, right)).then_with(|| left.cmp(right))
	});
	tab.expanded = expanded;
	Ok(())
}

fn canonical_path(path: &Path, label: &str) -> Result<PathBuf, String> {
	if !path.is_absolute() {
		return Err(format!("{label} must be an absolute path: {}", path.display()));
	}
	path.canonicalize().map_err(|error| format!("cannot resolve {label} {}: {error}", path.display()))
}

fn canonical_descendant(cwd: &Path, path: &Path, label: &str, require_directory: bool) -> Result<PathBuf, String> {
	let path = canonical_path(path, label)?;
	let relative = path.strip_prefix(cwd)
		.map_err(|_| format!("{label} is outside cwd: {}", path.display()))?;
	let depth = relative.components().count();
	if depth > MAX_SESSION_PATH_DEPTH {
		return Err(format!("{label} exceeds the maximum relative depth of {MAX_SESSION_PATH_DEPTH}: {}", path.display()));
	}
	if require_directory
		&& !fs::metadata(&path).map_err(|error| format!("cannot inspect {label} {}: {error}", path.display()))?.is_dir()
	{
		return Err(format!("{label} is not a directory: {}", path.display()));
	}
	Ok(path)
}

fn path_depth(cwd: &Path, path: &Path) -> usize {
	path.strip_prefix(cwd).expect("normalized expanded path must be within cwd").components().count()
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicUsize, Ordering};

	use super::*;

	static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

	struct TestDir(PathBuf);

	impl TestDir {
		fn new() -> Self {
			let path = std::env::temp_dir().join(format!(
				"tuzi-session-state-{}-{}",
				std::process::id(),
				NEXT_DIR.fetch_add(1, Ordering::Relaxed),
			));
			fs::create_dir(&path).unwrap();
			Self(path)
		}
	}

	impl Drop for TestDir {
		fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); }
	}

	#[test]
	fn session_state_round_trips_through_json() {
		let state = SessionState {
			version: SESSION_STATE_VERSION,
			active_tab: 0, home: None,
			tabs: vec![TabState {
				cwd: PathBuf::from("/project"),
				cursor: Some(PathBuf::from("/project/src/main.rs")),
				selection: vec![PathBuf::from("/project/README.md")],
				expanded: vec![PathBuf::from("/project/src")],
			}],
		};
		let json = serde_json::to_value(&state).unwrap();
		assert_eq!(serde_json::from_value::<SessionState>(json).unwrap(), state);
	}

	#[test]
	fn unknown_fields_are_rejected_at_both_levels() {
		let top_level = serde_json::json!({
			"version": 1,
			"active_tab": 0,
			"tabs": [],
			"extra": true
		});
		assert!(serde_json::from_value::<SessionState>(top_level).is_err());

		let tab_level = serde_json::json!({
			"version": 1,
			"active_tab": 0,
			"tabs": [{
				"cwd": "/project",
				"cursor": null,
				"selection": [],
				"expanded": [],
				"extra": true
			}]
		});
		assert!(serde_json::from_value::<SessionState>(tab_level).is_err());
	}

	#[test]
	fn normalization_canonicalizes_paths_adds_cursor_ancestors_and_sorts_expanded() {
		let root = TestDir::new();
		fs::create_dir_all(root.0.join("src/app")).unwrap();
		fs::create_dir(root.0.join("tests")).unwrap();
		fs::write(root.0.join("src/app/main.rs"), "").unwrap();
		fs::write(root.0.join("selected.txt"), "").unwrap();
		let state = SessionState {
			version: SESSION_STATE_VERSION,
			active_tab: 0, home: None,
			tabs: vec![TabState {
				cwd: root.0.clone(),
				cursor: Some(root.0.join("src/app/main.rs")),
				selection: vec![root.0.join("selected.txt")],
				expanded: vec![root.0.join("tests"), root.0.join("src/app"), root.0.join("tests")],
			}],
		};
		let state = validate_and_normalize(state).unwrap();
		let cwd = root.0.canonicalize().unwrap();
		assert_eq!(state.tabs[0].cwd, cwd);
		assert_eq!(state.tabs[0].expanded, [cwd.join("src"), cwd.join("tests"), cwd.join("src/app")]);
	}

	#[test]
	fn invalid_shape_resources_and_paths_are_rejected() {
		let root = TestDir::new();
		let tab = || TabState { cwd: root.0.clone(), cursor: None, selection: Vec::new(), expanded: Vec::new() };
		assert!(validate_and_normalize(SessionState { version: 2, active_tab: 0, home: None, tabs: vec![tab()] }).is_err());
		assert!(validate_and_normalize(SessionState { version: 1, active_tab: 0, home: None, tabs: Vec::new() }).is_err());
		assert!(validate_and_normalize(SessionState { version: 1, active_tab: 1, home: None, tabs: vec![tab()] }).is_err());
		assert!(validate_and_normalize(SessionState { version: 1, active_tab: 0, home: None, tabs: (0..=MAX_SESSION_TABS).map(|_| tab()).collect() }).is_err());
		let mut expanded_overflow = tab();
		expanded_overflow.expanded = vec![root.0.clone(); MAX_EXPANDED_PATHS_PER_TAB + 1];
		assert!(validate_and_normalize(SessionState { version: 1, active_tab: 0, home: None, tabs: vec![expanded_overflow] }).is_err());
		let mut selection_overflow = tab();
		selection_overflow.selection = vec![root.0.clone(); MAX_SELECTION_PATHS_PER_RESTORE + 1];
		assert!(validate_and_normalize(SessionState { version: 1, active_tab: 0, home: None, tabs: vec![selection_overflow] }).is_err());

		let outside = TestDir::new();
		fs::write(outside.0.join("file"), "").unwrap();
		let mut outside_tab = tab();
		outside_tab.cursor = Some(outside.0.join("file"));
		assert!(validate_and_normalize(SessionState { version: 1, active_tab: 0, home: None, tabs: vec![outside_tab] }).is_err());

		let mut missing_tab = tab();
		missing_tab.selection.push(root.0.join("missing"));
		assert!(validate_and_normalize(SessionState { version: 1, active_tab: 0, home: None, tabs: vec![missing_tab] }).is_err());
	}

	#[test]
	fn expanded_must_be_a_directory_and_relative_depth_is_limited() {
		let root = TestDir::new();
		let file = root.0.join("file");
		fs::write(&file, "").unwrap();
		let state = SessionState {
			version: 1,
			active_tab: 0, home: None,
			tabs: vec![TabState { cwd: root.0.clone(), cursor: None, selection: Vec::new(), expanded: vec![file] }],
		};
		assert!(validate_and_normalize(state).is_err());

		let mut deep = root.0.clone();
		for _ in 0..=MAX_SESSION_PATH_DEPTH { deep.push("d"); }
		fs::create_dir_all(&deep).unwrap();
		let state = SessionState {
			version: 1,
			active_tab: 0, home: None,
			tabs: vec![TabState { cwd: root.0.clone(), cursor: Some(deep), selection: Vec::new(), expanded: Vec::new() }],
		};
		assert!(validate_and_normalize(state).is_err());
	}

	#[test]
	fn home_is_optional_and_omitted_from_json_when_absent() {
		let json = r#"{"version":1,"active_tab":0,"tabs":[]}"#;
		let state: SessionState = serde_json::from_str(json).unwrap();
		assert_eq!(state.home, None, "older snapshots without home stay valid");
		assert!(!serde_json::to_string(&state).unwrap().contains("home"));
		let with_home: SessionState = serde_json::from_str(r#"{"version":1,"active_tab":0,"home":"/x","tabs":[]}"#).unwrap();
		assert!(serde_json::to_string(&with_home).unwrap().contains(r#""home":"/x""#));
	}

	#[test]
	fn home_must_be_an_existing_absolute_directory() {
		let dir = TestDir::new();
		let file = dir.0.join("file");
		fs::write(&file, "").unwrap();
		let with_home = |home: PathBuf| SessionState { version: 1, active_tab: 0, home: Some(home), tabs: vec![TabState {
			cwd: dir.0.clone(), cursor: None, selection: Vec::new(), expanded: Vec::new(),
		}] };
		assert!(validate_and_normalize(with_home(PathBuf::from("relative"))).is_err());
		assert!(validate_and_normalize(with_home(dir.0.join("missing"))).is_err());
		assert!(validate_and_normalize(with_home(file)).is_err());
		let normalized = validate_and_normalize(with_home(dir.0.join("."))).unwrap();
		assert_eq!(normalized.home, Some(dir.0.canonicalize().unwrap()), "home is canonicalized like every other path");
	}
}
