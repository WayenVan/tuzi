use std::str::FromStr;

use crate::{column_mode::ColumnMode, dds::BUILTIN_KINDS, fs::{SortBy, SortPolicy}};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CursorTarget {
	Relative(isize),
	Top,
	Bottom,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CdTarget {
	Interactive,
	Path(String),
	Trash,
	Config,
	Selected,
}

/// Whether an armed delete confirmation sends its targets to the system
/// trash (recoverable) or removes them outright.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteMode {
	Trash,
	Permanent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyKind {
	Path,
	Url,
	DirectoryPath,
	DirectoryUrl,
	Filename,
	Stem,
}

/// `serde_json::Value` can hold an `f64`, so `Command` can't derive `Eq`
/// (only `PartialEq`) once `Emit` carries one.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
	Quit,
	Escape,
	Cursor(CursorTarget),
	MovePage(i8),
	Cd(CdTarget),
	HistoryBack,
	HistoryForward,
	Expand,
	ToggleExpand,
	Collapse,
	CollapseSubtree,
	CollapseAll,
	CenterCursor,
	ToggleSelect,
	VisualSelect { unset: bool },
	Delete,
	DeletePermanently,
	Yank { cut: bool },
	Paste,
	PasteLink { absolute: bool },
	Copy(CopyKind),
	Rename(Option<String>),
	Create(Option<String>),
	Find { previous: bool },
	Filter,
	CommandPrompt,
	NewTab,
	CloseTab,
	SwitchTab(isize),
	SetColumnMode(ColumnMode),
	SetSort(SortPolicy),
	ToggleHidden,
	TogglePreview,
	SeekPreview(i16),
	RepeatFind { opposite: bool },
	Fzf,
	Zoxide,
	Open { interactive: bool },
	ToggleTasks,
	/// Publishes a custom event on the internal DDS bus (`.ai/dds-plan.md`
	/// P2). `data` defaults to `Value::Null` when the command carries no
	/// JSON argument.
	Emit { kind: String, data: serde_json::Value },
}

impl FromStr for Command {
	type Err = String;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let tokens = tokenize(value)?;
		let words: Vec<_> = tokens.iter().map(String::as_str).collect();
		let invalid = || format!("invalid command '{value}'");
		match words.as_slice() {
			["quit"] => Ok(Self::Quit),
			["escape"] => Ok(Self::Escape),
			["cursor", "top"] => Ok(Self::Cursor(CursorTarget::Top)),
			["cursor", "bottom"] => Ok(Self::Cursor(CursorTarget::Bottom)),
			["cursor", amount] => amount.parse().map(|amount| Self::Cursor(CursorTarget::Relative(amount))).map_err(|_| invalid()),
			["page", amount] => amount.parse().map(Self::MovePage).map_err(|_| invalid()),
			["cd"] => Ok(Self::Cd(CdTarget::Interactive)),
			["cd", "@trash"] => Ok(Self::Cd(CdTarget::Trash)),
			["cd", "@config"] => Ok(Self::Cd(CdTarget::Config)),
			["cd", "@selected"] => Ok(Self::Cd(CdTarget::Selected)),
			["cd", target] if target.starts_with('@') => Err(format!("invalid cd target '{target}'")),
			["cd", path] if !path.is_empty() => Ok(Self::Cd(CdTarget::Path((*path).into()))),
			["rename"] => Ok(Self::Rename(None)),
			["rename", name] if !name.is_empty() => Ok(Self::Rename(Some((*name).into()))),
			["create"] => Ok(Self::Create(None)),
			["create", path] if !path.is_empty() => Ok(Self::Create(Some((*path).into()))),
			["history", "back"] => Ok(Self::HistoryBack),
			["history", "forward"] => Ok(Self::HistoryForward),
			["expand"] => Ok(Self::Expand),
			["expand", "toggle"] => Ok(Self::ToggleExpand),
			["collapse"] => Ok(Self::Collapse),
			["collapse", "subtree"] => Ok(Self::CollapseSubtree),
			["collapse", "all"] => Ok(Self::CollapseAll),
			["center"] => Ok(Self::CenterCursor),
			["select", "toggle"] => Ok(Self::ToggleSelect),
			["visual"] => Ok(Self::VisualSelect { unset: false }),
			["visual", "--unset"] => Ok(Self::VisualSelect { unset: true }),
			["remove"] => Ok(Self::Delete),
			["remove", "--permanently"] => Ok(Self::DeletePermanently),
			["yank"] => Ok(Self::Yank { cut: false }),
			["yank", "--cut"] => Ok(Self::Yank { cut: true }),
			["paste"] => Ok(Self::Paste),
			["link", "--relative"] => Ok(Self::PasteLink { absolute: false }),
			["link", "--absolute"] => Ok(Self::PasteLink { absolute: true }),
			["copy", kind] => Ok(Self::Copy(match *kind {
				"path" => CopyKind::Path, "url" => CopyKind::Url, "dirpath" => CopyKind::DirectoryPath,
				"dirurl" => CopyKind::DirectoryUrl, "filename" => CopyKind::Filename, "stem" => CopyKind::Stem,
				_ => return Err(invalid()),
			})),
			["find"] => Ok(Self::Find { previous: false }),
			["find", "--previous"] => Ok(Self::Find { previous: true }),
			["find", "repeat"] => Ok(Self::RepeatFind { opposite: false }),
			["find", "repeat", "--opposite"] => Ok(Self::RepeatFind { opposite: true }),
			["filter"] => Ok(Self::Filter),
			["command"] => Ok(Self::CommandPrompt),
			["hidden", "toggle"] => Ok(Self::ToggleHidden),
			["tab", "create"] => Ok(Self::NewTab),
			["tab", "close"] => Ok(Self::CloseTab),
			["tab", "switch", amount] => amount.parse().map(Self::SwitchTab).map_err(|_| invalid()),
			["column", mode] => Ok(Self::SetColumnMode(match *mode {
				"none" => ColumnMode::None, "size" => ColumnMode::Size, "permissions" => ColumnMode::Permissions,
				"modified" => ColumnMode::Modified, _ => return Err(invalid()),
			})),
			["sort", by] | ["sort", by, "--reverse=no"] => Ok(Self::SetSort(SortPolicy::new(parse_sort(by).ok_or_else(invalid)?, false))),
			["sort", by, "--reverse"] | ["sort", by, "--reverse=yes"] => Ok(Self::SetSort(SortPolicy::new(parse_sort(by).ok_or_else(invalid)?, true))),
			["preview", "toggle"] => Ok(Self::TogglePreview),
			["preview", "seek", amount] => amount.parse().map(Self::SeekPreview).map_err(|_| invalid()),
			["fzf"] => Ok(Self::Fzf),
			["zoxide"] => Ok(Self::Zoxide),
			["open"] => Ok(Self::Open { interactive: false }),
			["open", "--interactive"] => Ok(Self::Open { interactive: true }),
			["tasks", "toggle"] => Ok(Self::ToggleTasks),
			["emit", kind] => Ok(Self::Emit { kind: emit_kind(kind).map_err(|_| invalid())?, data: serde_json::Value::Null }),
			["emit", kind, json] => Ok(Self::Emit {
				kind: emit_kind(kind).map_err(|_| invalid())?,
				data: serde_json::from_str(json).map_err(|_| format!("invalid json for emit: '{json}'"))?,
			}),
			_ => Err(invalid()),
		}
	}
}

/// Rejects kinds that would let `emit` spoof a built-in DDS event.
fn emit_kind(kind: &str) -> Result<String, ()> {
	if kind.is_empty() || BUILTIN_KINDS.contains(&kind) { return Err(()); }
	Ok(kind.to_string())
}

fn parse_sort(value: &str) -> Option<SortBy> {
	match value { "name" => Some(SortBy::Name), "modified" => Some(SortBy::Modified), "size" => Some(SortBy::Size), "extension" => Some(SortBy::Extension), _ => None }
}

pub struct CommandSpec {
	pub name:        &'static str,
	pub description: &'static str,
	pub usages:      &'static [&'static str],
}

const COMMAND_SPECS: &[CommandSpec] = &[
	CommandSpec { name: "cursor", description: "Move the cursor", usages: &["cursor 1", "cursor -1", "cursor top", "cursor bottom"] },
	CommandSpec { name: "cd", description: "Change directory", usages: &["cd", "cd @selected", "cd @trash", "cd @config", "cd ..", "cd ~", "cd ~/Downloads", "cd ~/Desktop"] },
	CommandSpec { name: "center", description: "Center the cursor", usages: &["center"] },
	CommandSpec { name: "collapse", description: "Collapse directories", usages: &["collapse", "collapse subtree", "collapse all"] },
	CommandSpec { name: "column", description: "Set the metadata column", usages: &["column none", "column size", "column permissions", "column modified"] },
	CommandSpec { name: "command", description: "Open the command prompt", usages: &["command"] },
	CommandSpec { name: "copy", description: "Copy path information", usages: &["copy path", "copy url", "copy dirpath", "copy dirurl", "copy filename", "copy stem"] },
	CommandSpec { name: "create", description: "Create a file or directory", usages: &["create"] },
	CommandSpec { name: "emit", description: "Publish a custom DDS event", usages: &["emit my-kind", r#"emit my-kind '{"a":1}'"#] },
	CommandSpec { name: "escape", description: "Cancel the current mode", usages: &["escape"] },
	CommandSpec { name: "expand", description: "Expand directories", usages: &["expand", "expand toggle"] },
	CommandSpec { name: "filter", description: "Filter visible files", usages: &["filter"] },
	CommandSpec { name: "find", description: "Find visible files", usages: &["find", "find --previous", "find repeat", "find repeat --opposite"] },
	CommandSpec { name: "fzf", description: "Jump with fzf", usages: &["fzf"] },
	CommandSpec { name: "hidden", description: "Toggle hidden files", usages: &["hidden toggle"] },
	CommandSpec { name: "history", description: "Navigate directory history", usages: &["history back", "history forward"] },
	CommandSpec { name: "link", description: "Paste a symbolic link", usages: &["link --relative", "link --absolute"] },
	CommandSpec { name: "open", description: "Open selected files", usages: &["open", "open --interactive"] },
	CommandSpec { name: "page", description: "Move by a percentage of the viewport", usages: &["page -50", "page 50"] },
	CommandSpec { name: "paste", description: "Paste yanked files", usages: &["paste"] },
	CommandSpec { name: "preview", description: "Control the preview", usages: &["preview toggle", "preview seek 1", "preview seek -1"] },
	CommandSpec { name: "quit", description: "Quit Tuzi", usages: &["quit"] },
	CommandSpec { name: "remove", description: "Remove selected files", usages: &["remove", "remove --permanently"] },
	CommandSpec { name: "rename", description: "Rename the selected file", usages: &["rename"] },
	CommandSpec { name: "select", description: "Toggle selection", usages: &["select toggle"] },
	CommandSpec { name: "sort", description: "Set the sort order", usages: &["sort name", "sort name --reverse", "sort modified", "sort modified --reverse", "sort size", "sort size --reverse", "sort extension", "sort extension --reverse"] },
	CommandSpec { name: "tab", description: "Manage tabs", usages: &["tab create", "tab close", "tab switch 1", "tab switch -1"] },
	CommandSpec { name: "tasks", description: "Toggle the task manager", usages: &["tasks toggle"] },
	CommandSpec { name: "visual", description: "Control visual selection", usages: &["visual", "visual --unset"] },
	CommandSpec { name: "yank", description: "Yank selected files", usages: &["yank", "yank --cut"] },
	CommandSpec { name: "zoxide", description: "Jump with zoxide", usages: &["zoxide"] },
];

pub fn specs() -> &'static [CommandSpec] { COMMAND_SPECS }

pub fn completions(prefix: &str) -> Vec<String> {
	let prefix = prefix.trim_start();
	specs().iter().flat_map(|spec| {
		debug_assert!(!spec.description.is_empty());
		debug_assert!(spec.usages.iter().all(|usage| usage == &spec.name || usage.strip_prefix(spec.name).is_some_and(|rest| rest.starts_with(' '))));
		spec.usages
	}).filter(|candidate| candidate.starts_with(prefix)).map(|candidate| (*candidate).into()).collect()
}

fn tokenize(value: &str) -> Result<Vec<String>, String> {
	let mut words = Vec::new();
	let mut word = String::new();
	let mut quote = None;
	let mut escaped = false;
	let mut started = false;
	for ch in value.chars() {
		if escaped {
			if ch.is_whitespace() || matches!(ch, '\\' | '\'' | '"') { word.push(ch); }
			else { word.push('\\'); word.push(ch); }
			escaped = false;
			started = true;
			continue;
		}
		if quote != Some('\'') && ch == '\\' {
			escaped = true;
			started = true;
			continue;
		}
		if let Some(mark) = quote {
			if ch == mark { quote = None; } else { word.push(ch); }
			started = true;
			continue;
		}
		match ch {
			'\'' | '"' => { quote = Some(ch); started = true; }
			ch if ch.is_whitespace() => if started { words.push(std::mem::take(&mut word)); started = false; },
			_ => { word.push(ch); started = true; },
		}
	}
	if escaped { word.push('\\'); }
	if quote.is_some() { return Err(format!("invalid command '{value}': unterminated quote")); }
	if started { words.push(word); }
	Ok(words)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_public_command_language() {
		assert_eq!("cursor -2".parse(), Ok(Command::Cursor(CursorTarget::Relative(-2))));
		assert_eq!("open --interactive".parse(), Ok(Command::Open { interactive: true }));
		assert!("unknown".parse::<Command>().is_err());
		assert!("arrow -2".parse::<Command>().is_err());
		assert!("input cd".parse::<Command>().is_err());
	}

	#[test]
	fn emit_publishes_an_arbitrary_kind_with_optional_json() {
		assert_eq!(
			"emit my-kind".parse(),
			Ok(Command::Emit { kind: "my-kind".into(), data: serde_json::Value::Null })
		);
		assert_eq!(
			r#"emit my-kind '{"a":1}'"#.parse(),
			Ok(Command::Emit { kind: "my-kind".into(), data: serde_json::json!({"a": 1}) })
		);
	}

	#[test]
	fn emit_rejects_reserved_kinds_and_malformed_json() {
		assert!("emit cd".parse::<Command>().is_err(), "cd is a built-in DDS kind");
		assert!("emit ''".parse::<Command>().is_err(), "empty kind");
		assert!(r#"emit my-kind 'not json'"#.parse::<Command>().is_err());
	}

	#[test]
	fn parses_quoted_and_escaped_arguments_without_a_shell() {
		assert_eq!(r#"cd "/tmp/path with spaces""#.parse(), Ok(Command::Cd(CdTarget::Path("/tmp/path with spaces".into()))));
		assert_eq!(r#"rename old\ name.txt"#.parse(), Ok(Command::Rename(Some("old name.txt".into()))));
		assert_eq!("create 'a b.txt'".parse(), Ok(Command::Create(Some("a b.txt".into()))));
		assert_eq!(r#"cd "C:\Users\Tuzi""#.parse(), Ok(Command::Cd(CdTarget::Path(r#"C:\Users\Tuzi"#.into()))));
		assert!(r#"cd "unfinished"#.parse::<Command>().is_err());
		assert!(r#"cd """#.parse::<Command>().is_err());
		assert!(r#"rename """#.parse::<Command>().is_err());
		assert!(r#"create """#.parse::<Command>().is_err());
	}

	#[test]
	fn builtin_cd_targets_are_explicitly_namespaced() {
		assert_eq!("cd @trash".parse(), Ok(Command::Cd(CdTarget::Trash)));
		assert_eq!("cd trash".parse(), Ok(Command::Cd(CdTarget::Path("trash".into()))));
		assert_eq!("cd ./@trash".parse(), Ok(Command::Cd(CdTarget::Path("./@trash".into()))));
		assert!("cd @unknown".parse::<Command>().is_err());
	}

	#[test]
	fn completes_command_prefixes() {
		assert_eq!(completions("open --i"), ["open --interactive"]);
		assert_eq!(completions("cursor "), ["cursor 1", "cursor -1", "cursor top", "cursor bottom"]);
		assert!(completions("sort m").contains(&"sort modified".into()));
		assert!(completions("not-a-command").is_empty());
	}

	#[test]
	fn command_specs_have_unique_names_and_valid_concrete_usages() {
		let mut names = std::collections::HashSet::new();
		for spec in specs() {
			assert!(names.insert(spec.name), "duplicate command spec: {}", spec.name);
			assert!(!spec.description.is_empty());
			for usage in spec.usages {
				assert!(usage.parse::<Command>().is_ok(), "invalid command usage: {usage}");
			}
		}
	}
}
