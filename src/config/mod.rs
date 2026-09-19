use std::{collections::HashMap, env, fs, path::{Path, PathBuf}};

use serde::Deserialize;

use crate::{column_mode::ColumnMode, fs::{SortBy, SortPolicy}, opener::{OpenConfig, OpenRule, Opener}};

const TUZI_PRESET: &str = include_str!("../../preset/tuzi-default.toml");
#[cfg(test)]
const KEYMAP_PRESET: &str = include_str!("../../preset/keymap-default.toml");
#[cfg(test)]
const THEME_PRESET: &str = include_str!("../../preset/theme-default.toml");

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
	pub mgr:     Manager,
	pub preview: Preview,
	pub tasks:   Tasks,
	pub opener:  OpenConfig,
	pub confirm: Confirm,
	pub fs:      FsPolicy,
	pub ui:      Ui,
	pub notify:  Notify,
	pub watcher: Watcher,
	pub dds:     Dds,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manager {
	pub sort:         SortPolicy,
	pub show_hidden:  bool,
	pub column_mode:  ColumnMode,
	pub history_size: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
	pub show:             bool,
	pub ratio:            u16,
	pub max_scan_bytes:   usize,
	pub max_line_bytes:   usize,
	pub cache_bytes:      usize,
	pub overscan_lines:   usize,
	pub syntax_highlight: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tasks {
	pub workers:              usize,
	pub copy_buffer_size:     usize,
	pub progress_interval_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Confirm { pub trash: bool, pub delete: bool }

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy { Rename, Error }

#[derive(Clone, Debug, PartialEq)]
pub struct FsPolicy {
	pub paste_conflict:  ConflictPolicy,
	pub create_conflict: ConflictPolicy,
	pub rename_conflict: ConflictPolicy,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ui { pub mouse: bool, pub popup_width: u16, pub completion_max_items: usize, pub which_key: bool, pub filename_peek: bool }

#[derive(Clone, Debug, PartialEq)]
pub struct Notify { pub info_timeout: u64, pub warn_timeout: u64, pub error_timeout: u64 }

#[derive(Clone, Debug, PartialEq)]
pub struct Watcher { pub debounce_ms: u64, pub max_wait_ms: u64, pub poll_interval_ms: u64 }

#[derive(Clone, Debug, PartialEq)]
pub struct Dds {
	pub enabled:   bool,
	pub open:      DdsOpen,
	/// Implicit built-in App events explicitly made public to all interested
	/// DDS peers. Other implicit events go only to an interested controlling
	/// parent, if present. Explicit `emit` is governed only by `enabled`.
	pub broadcast: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DdsOpen { Auto, Local, Parent }

impl Default for Config {
	fn default() -> Self {
		Self::from_preset().expect("embedded tuzi-default.toml must be valid")
	}
}

#[derive(Clone, Debug)]
pub enum RuntimeConfigSource {
	Inline(String),
	File(PathBuf),
}

#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
	pub config_dir: Option<PathBuf>,
	pub no_config:  bool,
	/// Process-local JSON documents, applied in command-line order.
	pub runtime_config: Vec<RuntimeConfigSource>,
}

impl Config {
	pub fn load(options: &LoadOptions) -> Result<Self, String> {
		let mut config = Self::from_preset().map_err(|error| format!("invalid embedded preset/tuzi-default.toml (this is a Tuzi bug): {error}"))?;
		config.validate(Path::new("preset/tuzi-default.toml")).map_err(|error| format!("{error} (this is a Tuzi bug)"))?;
		let loader = Loader::new(options)?;
		if let Some((path, source)) = loader.read("tuzi.toml")? {
			let user: UserConfig = toml::from_str(&source).map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
			user.apply(&mut config);
			config.validate(&path)?;
		}
		for (origin, document) in runtime_documents(options)? {
			if let Some(value) = document.get("config") {
				let user: UserConfig = serde_json::from_value(value.clone()).map_err(|error| format!("invalid {origin} config: {error}"))?;
				user.apply(&mut config);
				config.validate(Path::new(&origin))?;
			}
		}
		Ok(config)
	}

	fn from_preset() -> Result<Self, toml::de::Error> {
		toml::from_str::<PresetConfig>(TUZI_PRESET).map(Into::into)
	}

	fn validate(&self, path: &Path) -> Result<(), String> {
		if !(10..=90).contains(&self.preview.ratio) {
			return Err(format!("invalid {}: preview.ratio must be between 10 and 90", path.display()));
		}
		if !(64 * 1024..=1024 * 1024 * 1024).contains(&self.preview.max_scan_bytes) {
			return Err(format!("invalid {}: preview.max_scan_bytes must be between 65536 and 1073741824", path.display()));
		}
		if !(256..=1024 * 1024).contains(&self.preview.max_line_bytes) || self.preview.max_line_bytes > self.preview.max_scan_bytes {
			return Err(format!("invalid {}: preview.max_line_bytes must be between 256 and 1048576 and cannot exceed preview.max_scan_bytes", path.display()));
		}
		if self.preview.cache_bytes > 1024 * 1024 * 1024 {
			return Err(format!("invalid {}: preview.cache_bytes must not exceed 1073741824", path.display()));
		}
		if self.preview.overscan_lines > 1000 {
			return Err(format!("invalid {}: preview.overscan_lines must not exceed 1000", path.display()));
		}
		if !(1..=64).contains(&self.tasks.workers) {
			return Err(format!("invalid {}: tasks.workers must be between 1 and 64", path.display()));
		}
		if !(4 * 1024..=16 * 1024 * 1024).contains(&self.tasks.copy_buffer_size) {
			return Err(format!("invalid {}: tasks.copy_buffer_size must be between 4096 and 16777216", path.display()));
		}
		if !(10..=1000).contains(&self.tasks.progress_interval_ms) {
			return Err(format!("invalid {}: tasks.progress_interval_ms must be between 10 and 1000", path.display()));
		}
		if self.mgr.history_size == 0 {
			return Err(format!("invalid {}: mgr.history_size must be greater than zero", path.display()));
		}
		if !(20..=200).contains(&self.ui.popup_width) { return Err(format!("invalid {}: ui.popup_width must be between 20 and 200", path.display())); }
		if !(1..=50).contains(&self.ui.completion_max_items) { return Err(format!("invalid {}: ui.completion_max_items must be between 1 and 50", path.display())); }
		if !(1..=3600).contains(&self.notify.info_timeout) || !(1..=3600).contains(&self.notify.warn_timeout) || !(1..=3600).contains(&self.notify.error_timeout) {
			return Err(format!("invalid {}: notify timeouts must be between 1 and 3600 seconds", path.display()));
		}
		if !(10..=5000).contains(&self.watcher.debounce_ms) { return Err(format!("invalid {}: watcher.debounce_ms must be between 10 and 5000", path.display())); }
		if !(10..=10000).contains(&self.watcher.max_wait_ms) { return Err(format!("invalid {}: watcher.max_wait_ms must be between 10 and 10000", path.display())); }
		if self.watcher.debounce_ms > self.watcher.max_wait_ms { return Err(format!("invalid {}: watcher.debounce_ms cannot exceed watcher.max_wait_ms", path.display())); }
		if !(50..=60000).contains(&self.watcher.poll_interval_ms) { return Err(format!("invalid {}: watcher.poll_interval_ms must be between 50 and 60000", path.display())); }
		const ALLOWED: &[&str] = &["cd", "hover", "yank", "renamed", "task-done"];
		for kind in &self.dds.broadcast {
			if !ALLOWED.contains(&kind.as_str()) {
				return Err(format!("invalid {}: dds.broadcast contains unsupported kind '{kind}'", path.display()));
			}
		}
		self.opener.validate().map_err(|error| format!("invalid {}: {error}", path.display()))?;
		Ok(())
	}
}

pub(crate) fn runtime_documents(options: &LoadOptions) -> Result<Vec<(String, serde_json::Map<String, serde_json::Value>)>, String> {
	options.runtime_config.iter().map(|source| {
		let (origin, json) = match source {
			RuntimeConfigSource::Inline(json) => ("--runtime-config".to_owned(), json.clone()),
			RuntimeConfigSource::File(path) => (
				format!("--runtime-config-file {}", path.display()),
				fs::read_to_string(path).map_err(|error| format!("failed to read runtime config {}: {error}", path.display()))?,
			),
		};
		let document = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&json)
			.map_err(|error| format!("invalid {origin}: {error}"))?;
		for key in document.keys() {
			if !matches!(key.as_str(), "config" | "keymap" | "state") {
				return Err(format!("invalid {origin}: unknown top-level key '{key}'"));
			}
		}
		Ok((origin, document))
	}).collect()
}

/// Reads the last session snapshot present in the ordered runtime documents.
/// Config and keymap keep their overlay behavior; state is a whole value and
/// therefore replaces any earlier state document.
pub fn load_runtime_state(options: &LoadOptions) -> Result<Option<crate::session_state::SessionState>, String> {
	let mut state = None;
	for (origin, document) in runtime_documents(options)? {
		if let Some(value) = document.get("state") {
			state = Some(serde_json::from_value(value.clone())
				.map_err(|error| format!("invalid {origin} state: {error}"))?);
		}
	}
	Ok(state)
}

/// Shared file discovery for all configuration documents. Keymap and theme
/// use this same loader when their typed schemas are introduced, so XDG and
/// CLI override behavior stay identical across the three files.
struct Loader {
	directory: Option<PathBuf>,
}

pub(crate) fn read_user_file(options: &LoadOptions, name: &str) -> Result<Option<(PathBuf, String)>, String> {
	Loader::new(options)?.read(name)
}

impl Loader {
	fn new(options: &LoadOptions) -> Result<Self, String> {
		let directory = if options.no_config {
			None
		} else {
			Some(match &options.config_dir { Some(path) => path.clone(), None => default_config_dir()? })
		};
		Ok(Self { directory })
	}

	fn read(&self, name: &str) -> Result<Option<(PathBuf, String)>, String> {
		let Some(directory) = &self.directory else { return Ok(None) };
		let path = directory.join(name);
		match fs::read_to_string(&path) {
			Ok(source) => Ok(Some((path, source))),
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
			Err(error) => Err(format!("failed to read {}: {error}", path.display())),
		}
	}
}

fn default_config_dir() -> Result<PathBuf, String> {
	if let Some(path) = env::var_os("TUZI_CONFIG_HOME") { return Ok(PathBuf::from(path)); }
	if let Some(path) = env::var_os("XDG_CONFIG_HOME") { return Ok(PathBuf::from(path).join("tuzi")); }
	#[cfg(target_os = "windows")]
	if let Some(path) = env::var_os("APPDATA") { return Ok(PathBuf::from(path).join("tuzi")); }
	let home = env::var_os(if cfg!(target_os = "windows") { "USERPROFILE" } else { "HOME" })
		.ok_or("cannot determine the config directory: HOME is not set")?;
	Ok(PathBuf::from(home).join(".config").join("tuzi"))
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserConfig { mgr: UserManager, preview: UserPreview, tasks: UserTasks, confirm: UserConfirm, fs: UserFs, ui: UserUi, notify: UserNotify, watcher: UserWatcher, dds: UserDds, opener: HashMap<String, Vec<Opener>>, open: UserOpen }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserManager {
	sort_by:      Option<SortName>,
	sort_reverse: Option<bool>,
	show_hidden:  Option<bool>,
	column_mode:  Option<ColumnName>,
	history_size: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserPreview {
	show:             Option<bool>,
	ratio:            Option<u16>,
	max_scan_bytes:   Option<usize>,
	max_line_bytes:   Option<usize>,
	cache_bytes:      Option<usize>,
	overscan_lines:   Option<usize>,
	syntax_highlight: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserTasks { workers: Option<usize>, copy_buffer_size: Option<usize>, progress_interval_ms: Option<u64> }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserConfirm { trash: Option<bool>, delete: Option<bool> }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserFs { paste_conflict: Option<ConflictPolicy>, create_conflict: Option<ConflictPolicy>, rename_conflict: Option<ConflictPolicy> }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserUi { mouse: Option<bool>, popup_width: Option<u16>, completion_max_items: Option<usize>, which_key: Option<bool>, filename_peek: Option<bool> }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserNotify { info_timeout: Option<u64>, warn_timeout: Option<u64>, error_timeout: Option<u64> }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserWatcher { debounce_ms: Option<u64>, max_wait_ms: Option<u64>, poll_interval_ms: Option<u64> }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserDds { enabled: Option<bool>, open: Option<DdsOpen>, broadcast: Option<Vec<String>> }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserOpen { rules: Option<Vec<OpenRule>> }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetConfig { mgr: PresetManager, preview: PresetPreview, tasks: PresetTasks, confirm: PresetConfirm, fs: PresetFs, ui: PresetUi, notify: PresetNotify, watcher: PresetWatcher, dds: PresetDds, opener: HashMap<String, Vec<Opener>>, open: PresetOpen }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetManager {
	sort_by:      SortName,
	sort_reverse: bool,
	show_hidden:  bool,
	column_mode:  ColumnName,
	history_size: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetPreview {
	show:             bool,
	ratio:            u16,
	max_scan_bytes:   usize,
	max_line_bytes:   usize,
	cache_bytes:      usize,
	overscan_lines:   usize,
	syntax_highlight: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetTasks { workers: usize, copy_buffer_size: usize, progress_interval_ms: u64 }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetConfirm { trash: bool, delete: bool }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetFs { paste_conflict: ConflictPolicy, create_conflict: ConflictPolicy, rename_conflict: ConflictPolicy }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetUi { mouse: bool, popup_width: u16, completion_max_items: usize, which_key: bool, filename_peek: bool }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetNotify { info_timeout: u64, warn_timeout: u64, error_timeout: u64 }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetWatcher { debounce_ms: u64, max_wait_ms: u64, poll_interval_ms: u64 }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetDds { enabled: bool, open: DdsOpen, broadcast: Vec<String> }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetOpen { rules: Vec<OpenRule> }

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SortName { Name, Modified, Size, Extension }

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ColumnName { None, Size, Permissions, Modified }

impl From<PresetConfig> for Config {
	fn from(value: PresetConfig) -> Self {
		Self {
			mgr: Manager {
				sort: SortPolicy { by: value.mgr.sort_by.into(), reverse: value.mgr.sort_reverse },
				show_hidden: value.mgr.show_hidden,
				column_mode: value.mgr.column_mode.into(),
				history_size: value.mgr.history_size,
			},
			preview: Preview {
				show: value.preview.show,
				ratio: value.preview.ratio,
				max_scan_bytes: value.preview.max_scan_bytes,
				max_line_bytes: value.preview.max_line_bytes,
				cache_bytes: value.preview.cache_bytes,
				overscan_lines: value.preview.overscan_lines,
				syntax_highlight: value.preview.syntax_highlight,
			},
			tasks: Tasks {
				workers: value.tasks.workers,
				copy_buffer_size: value.tasks.copy_buffer_size,
				progress_interval_ms: value.tasks.progress_interval_ms,
			},
			confirm: Confirm { trash: value.confirm.trash, delete: value.confirm.delete },
			fs: FsPolicy { paste_conflict: value.fs.paste_conflict, create_conflict: value.fs.create_conflict, rename_conflict: value.fs.rename_conflict },
			ui: Ui { mouse: value.ui.mouse, popup_width: value.ui.popup_width, completion_max_items: value.ui.completion_max_items, which_key: value.ui.which_key, filename_peek: value.ui.filename_peek },
			notify: Notify { info_timeout: value.notify.info_timeout, warn_timeout: value.notify.warn_timeout, error_timeout: value.notify.error_timeout },
			watcher: Watcher { debounce_ms: value.watcher.debounce_ms, max_wait_ms: value.watcher.max_wait_ms, poll_interval_ms: value.watcher.poll_interval_ms },
			dds: Dds { enabled: value.dds.enabled, open: value.dds.open, broadcast: value.dds.broadcast },
			opener: OpenConfig { openers: value.opener, rules: value.open.rules },
		}
	}
}

impl From<SortName> for SortBy {
	fn from(value: SortName) -> Self {
		match value { SortName::Name => Self::Name, SortName::Modified => Self::Modified, SortName::Size => Self::Size, SortName::Extension => Self::Extension }
	}
}

impl From<ColumnName> for ColumnMode {
	fn from(value: ColumnName) -> Self {
		match value { ColumnName::None => Self::None, ColumnName::Size => Self::Size, ColumnName::Permissions => Self::Permissions, ColumnName::Modified => Self::Modified }
	}
}

impl UserConfig {
	fn apply(self, config: &mut Config) {
		if let Some(value) = self.mgr.sort_by {
			config.mgr.sort.by = value.into();
		}
		if let Some(value) = self.mgr.sort_reverse { config.mgr.sort.reverse = value; }
		if let Some(value) = self.mgr.show_hidden { config.mgr.show_hidden = value; }
		if let Some(value) = self.mgr.column_mode {
			config.mgr.column_mode = value.into();
		}
		if let Some(value) = self.mgr.history_size { config.mgr.history_size = value; }
		if let Some(value) = self.preview.show { config.preview.show = value; }
		if let Some(value) = self.preview.ratio { config.preview.ratio = value; }
		if let Some(value) = self.preview.max_scan_bytes { config.preview.max_scan_bytes = value; }
		if let Some(value) = self.preview.max_line_bytes { config.preview.max_line_bytes = value; }
		if let Some(value) = self.preview.cache_bytes { config.preview.cache_bytes = value; }
		if let Some(value) = self.preview.overscan_lines { config.preview.overscan_lines = value; }
		if let Some(value) = self.preview.syntax_highlight { config.preview.syntax_highlight = value; }
		if let Some(value) = self.tasks.workers { config.tasks.workers = value; }
		if let Some(value) = self.tasks.copy_buffer_size { config.tasks.copy_buffer_size = value; }
		if let Some(value) = self.tasks.progress_interval_ms { config.tasks.progress_interval_ms = value; }
		if let Some(value) = self.confirm.trash { config.confirm.trash = value; }
		if let Some(value) = self.confirm.delete { config.confirm.delete = value; }
		if let Some(value) = self.fs.paste_conflict { config.fs.paste_conflict = value; }
		if let Some(value) = self.fs.create_conflict { config.fs.create_conflict = value; }
		if let Some(value) = self.fs.rename_conflict { config.fs.rename_conflict = value; }
		if let Some(value) = self.ui.mouse { config.ui.mouse = value; }
		if let Some(value) = self.ui.popup_width { config.ui.popup_width = value; }
		if let Some(value) = self.ui.completion_max_items { config.ui.completion_max_items = value; }
		if let Some(value) = self.ui.which_key { config.ui.which_key = value; }
		if let Some(value) = self.ui.filename_peek { config.ui.filename_peek = value; }
		if let Some(value) = self.notify.info_timeout { config.notify.info_timeout = value; }
		if let Some(value) = self.notify.warn_timeout { config.notify.warn_timeout = value; }
		if let Some(value) = self.notify.error_timeout { config.notify.error_timeout = value; }
		if let Some(value) = self.watcher.debounce_ms { config.watcher.debounce_ms = value; }
		if let Some(value) = self.watcher.max_wait_ms { config.watcher.max_wait_ms = value; }
		if let Some(value) = self.watcher.poll_interval_ms { config.watcher.poll_interval_ms = value; }
		if let Some(value) = self.dds.enabled { config.dds.enabled = value; }
		if let Some(value) = self.dds.open { config.dds.open = value; }
		if let Some(value) = self.dds.broadcast { config.dds.broadcast = value; }
		for (name, variants) in self.opener { config.opener.openers.insert(name, variants); }
		if let Some(rules) = self.open.rules { config.opener.rules = rules; }
	}
}

#[cfg(test)]
mod tests {
	use std::time::{SystemTime, UNIX_EPOCH};

	use super::*;

	#[test]
	fn partial_user_config_overlays_defaults() {
		let user: UserConfig = toml::from_str("[mgr]\nshow_hidden = true\nsort_by = 'size'\n[preview]\nratio = 55").unwrap();
		let mut config = Config::default();
		user.apply(&mut config);
		assert!(config.mgr.show_hidden);
		assert_eq!(config.mgr.sort, SortPolicy::new(SortBy::Size, false));
		assert_eq!(config.preview.ratio, 55);
		assert_eq!(config.tasks.workers, 2);
		assert!(config.confirm.trash);
		assert_eq!(config.fs.paste_conflict, ConflictPolicy::Rename);
	}

	#[test]
	fn file_operation_policies_overlay_independently() {
		let user: UserConfig = toml::from_str("[confirm]\ntrash = false\n[fs]\npaste_conflict = 'error'\n[ui]\nmouse = false\npopup_width = 72\n[notify]\nwarn_timeout = 12\n[watcher]\ndebounce_ms = 120").unwrap();
		let mut config = Config::default();
		user.apply(&mut config);
		assert!(!config.confirm.trash);
		assert!(config.confirm.delete);
		assert_eq!(config.fs.paste_conflict, ConflictPolicy::Error);
		assert_eq!(config.fs.create_conflict, ConflictPolicy::Error);
		assert!(!config.ui.mouse);
		assert_eq!(config.ui.popup_width, 72);
		assert_eq!(config.notify.warn_timeout, 12);
		assert_eq!(config.watcher.debounce_ms, 120);
		assert_eq!(config.watcher.max_wait_ms, 500);
	}

	#[test]
	fn dds_is_enabled_but_implicit_broadcasts_are_private_by_default() {
		let config = Config::default();
		assert!(config.dds.enabled);
		assert_eq!(config.dds.open, DdsOpen::Auto);
		assert!(config.dds.broadcast.is_empty());

		let user: UserConfig = toml::from_str("[dds]\nenabled = false\nbroadcast = ['cd', 'task-done']").unwrap();
		let mut config = Config::default();
		user.apply(&mut config);
		assert!(!config.dds.enabled);
		assert_eq!(config.dds.broadcast, ["cd", "task-done"]);
		config.validate(Path::new("test.toml")).unwrap();
	}

	#[test]
	fn runtime_config_applies_json_documents_in_order() {
		let options = LoadOptions {
			no_config: true,
			runtime_config: vec![
				RuntimeConfigSource::Inline(r#"{"config":{"dds":{"open":"parent","broadcast":["hover"]}}}"#.into()),
				RuntimeConfigSource::Inline(r#"{"config":{"dds":{"open":"local"}}}"#.into()),
			],
			..Default::default()
		};
		let config = Config::load(&options).unwrap();
		assert_eq!(config.dds.open, DdsOpen::Local, "later documents win");
		assert_eq!(config.dds.broadcast, ["hover"]);
	}

	#[test]
	fn runtime_state_uses_the_last_document_that_contains_state() {
		let options = LoadOptions {
			no_config: true,
			runtime_config: vec![
				RuntimeConfigSource::Inline(r#"{"state":{"version":1,"active_tab":0,"tabs":[{"cwd":"/first","cursor":null,"selection":[],"expanded":[]}]}}"#.into()),
				RuntimeConfigSource::Inline(r#"{"config":{}}"#.into()),
				RuntimeConfigSource::Inline(r#"{"state":{"version":1,"active_tab":0,"tabs":[{"cwd":"/last","cursor":null,"selection":[],"expanded":[]}]}}"#.into()),
			],
			..Default::default()
		};
		let state = load_runtime_state(&options).unwrap().unwrap();
		assert_eq!(state.tabs[0].cwd, PathBuf::from("/last"));
	}

	#[test]
	fn runtime_state_rejects_unknown_snapshot_fields() {
		let options = LoadOptions {
			no_config: true,
			runtime_config: vec![RuntimeConfigSource::Inline(
				r#"{"state":{"version":1,"active_tab":0,"tabs":[],"extra":true}}"#.into(),
			)],
			..Default::default()
		};
		assert!(load_runtime_state(&options).is_err());
	}

	#[test]
	fn runtime_config_rejects_bad_json_and_unknown_keys() {
		for value in ["not-json", r#"{"unknown":{}}"#, r#"{"config":{"dds":{"unknown":true}}}"#, r#"{"config":{"dds":{"enabled":"perhaps"}}}"#] {
			let result = Config::load(&LoadOptions { no_config: true, runtime_config: vec![RuntimeConfigSource::Inline(value.into())], ..Default::default() });
			assert!(result.is_err(), "{value} should be rejected");
		}
	}

	#[test]
	fn dds_rejects_unknown_implicit_broadcast_kinds() {
		let mut config = Config::default();
		config.dds.broadcast.push("update-tab".into());
		assert!(config.validate(Path::new("test.toml")).is_err());
	}

	#[test]
	fn ui_and_notification_ranges_are_validated() {
		let mut config = Config::default();
		config.ui.popup_width = 19;
		assert!(config.validate(Path::new("test.toml")).is_err());
		config.ui.popup_width = 50;
		config.notify.error_timeout = 0;
		assert!(config.validate(Path::new("test.toml")).is_err());
	}

	#[test]
	fn watcher_ranges_and_deadline_order_are_validated() {
		let mut config = Config::default();
		config.watcher.debounce_ms = 600;
		assert!(config.validate(Path::new("test.toml")).is_err());
		config.watcher.debounce_ms = 80;
		config.watcher.poll_interval_ms = 49;
		assert!(config.validate(Path::new("test.toml")).is_err());
	}

	#[test]
	fn preview_limits_overlay_and_validate() {
		let user: UserConfig = toml::from_str("[preview]\nmax_scan_bytes = 131072\nmax_line_bytes = 4096\ncache_bytes = 0\noverscan_lines = 0\nsyntax_highlight = false").unwrap();
		let mut config = Config::default();
		user.apply(&mut config);
		assert_eq!(config.preview.max_scan_bytes, 131072);
		assert_eq!(config.preview.cache_bytes, 0);
		assert!(!config.preview.syntax_highlight);
		config.validate(Path::new("test.toml")).unwrap();
		config.preview.max_line_bytes = 131073;
		assert!(config.validate(Path::new("test.toml")).is_err());
	}

	#[test]
	fn task_copy_parameters_overlay_and_validate() {
		let user: UserConfig = toml::from_str("[tasks]\nworkers = 4\ncopy_buffer_size = 1048576\nprogress_interval_ms = 100").unwrap();
		let mut config = Config::default();
		user.apply(&mut config);
		assert_eq!(config.tasks.workers, 4);
		assert_eq!(config.tasks.copy_buffer_size, 1048576);
		assert_eq!(config.tasks.progress_interval_ms, 100);
		config.validate(Path::new("test.toml")).unwrap();
		config.tasks.copy_buffer_size = 4095;
		assert!(config.validate(Path::new("test.toml")).is_err());
	}

	#[test]
	fn every_embedded_preset_is_valid_toml() {
		Config::from_preset().unwrap().validate(Path::new("preset/tuzi-default.toml")).unwrap();
		for source in [KEYMAP_PRESET, THEME_PRESET] {
			toml::from_str::<toml::Table>(source).unwrap();
		}
	}

	#[test]
	fn unknown_fields_are_rejected() {
		assert!(toml::from_str::<UserConfig>("[mgr]\nunknown = true").is_err());
	}

	#[test]
	fn loads_tuzi_toml_from_an_explicit_directory() {
		let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
		let directory = env::temp_dir().join(format!("tuzi-config-test-{}-{nonce}", std::process::id()));
		fs::create_dir(&directory).unwrap();
		fs::write(directory.join("tuzi.toml"), "[mgr]\nshow_hidden = true\n[preview]\nratio = 61\n").unwrap();

		let config = Config::load(&LoadOptions { config_dir: Some(directory.clone()), no_config: false, ..Default::default() }).unwrap();
		assert!(config.mgr.show_hidden);
		assert_eq!(config.preview.ratio, 61);

		fs::remove_dir_all(directory).unwrap();
	}

	#[test]
	fn missing_user_file_and_no_config_both_use_the_embedded_preset() {
		let missing = env::temp_dir().join(format!("tuzi-missing-config-{}", std::process::id()));
		let from_missing = Config::load(&LoadOptions { config_dir: Some(missing), no_config: false, ..Default::default() }).unwrap();
		let disabled = Config::load(&LoadOptions { config_dir: None, no_config: true, ..Default::default() }).unwrap();
		assert_eq!(from_missing, Config::default());
		assert_eq!(disabled, Config::default());
	}
}
