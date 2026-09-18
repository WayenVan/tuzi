use std::path::Path;

use serde::Deserialize;

use crate::{command::Command, config::{LoadOptions, read_user_file, runtime_documents}};

use super::{Binding, Key, Keymap};

const PRESET: &str = include_str!("../../preset/keymap-default.toml");

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Document { mgr: Section }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Section {
	keymap:         Option<Vec<Entry>>,
	prepend_keymap: Vec<Entry>,
	append_keymap:  Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
	on:   OneOrMany,
	run:  OneOrMany,
	desc: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany { One(String), Many(Vec<String>) }

impl OneOrMany {
	fn values(self) -> Vec<String> { match self { Self::One(value) => vec![value], Self::Many(values) => values } }
}

pub(super) fn load(options: &LoadOptions) -> Result<Keymap, String> {
	let preset: Document = toml::from_str(PRESET)
		.map_err(|error| format!("invalid embedded preset/keymap-default.toml (this is a Tuzi bug): {error}"))?;
	let mut core = entries(preset.mgr.keymap.unwrap_or_default(), Path::new("preset/keymap-default.toml"))?;
	if core.is_empty() { return Err("embedded preset/keymap-default.toml has no manager bindings (this is a Tuzi bug)".into()); }

	if let Some((path, source)) = read_user_file(options, "keymap.toml")? {
		let user: Document = toml::from_str(&source).map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
		apply_document(&mut core, user, &path)?;
	}
	for (origin, document) in runtime_documents(options)? {
		if let Some(value) = document.get("keymap") {
			let runtime: Document = serde_json::from_value(value.clone()).map_err(|error| format!("invalid {origin} keymap: {error}"))?;
			apply_document(&mut core, runtime, Path::new(&origin))?;
		}
	}
	Keymap::new(core).map_err(|error| format!("invalid keymap: {error}"))
}

fn apply_document(core: &mut Vec<Binding>, document: Document, path: &Path) -> Result<(), String> {
	if let Some(replacement) = document.mgr.keymap { *core = entries(replacement, path)?; }
	let prepend = entries(document.mgr.prepend_keymap, path)?;
	let append = entries(document.mgr.append_keymap, path)?;
	validate_section(&prepend, path, "prepend_keymap")?;
	validate_section(&append, path, "append_keymap")?;
	for binding in &prepend { core.retain(|old| old.context != binding.context || old.keys != binding.keys); }
	let mut merged = prepend;
	merged.append(core);
	for binding in append {
		if !merged.iter().any(|old| old.context == binding.context && old.keys == binding.keys) { merged.push(binding); }
	}
	*core = merged;
	Ok(())
}

fn validate_section(bindings: &[Binding], path: &Path, name: &str) -> Result<(), String> {
	for (index, binding) in bindings.iter().enumerate() {
		if bindings[..index].iter().any(|previous| previous.context == binding.context && (previous.keys.starts_with(&binding.keys) || binding.keys.starts_with(&previous.keys))) {
			return Err(format!("{}: mgr.{name} entry {} conflicts with an earlier key sequence", path.display(), index + 1));
		}
	}
	Ok(())
}

fn entries(values: Vec<Entry>, path: &Path) -> Result<Vec<Binding>, String> {
	values.into_iter().enumerate().map(|(index, entry)| {
		let keys = entry.on.values().into_iter().map(|value| value.parse()).collect::<Result<Vec<Key>, _>>()
			.map_err(|error| format!("{}: mgr keymap entry {}: {error}", path.display(), index + 1))?;
		let commands = entry.run.values().into_iter().map(|value| value.parse::<Command>()).collect::<Result<Vec<_>, _>>()
			.map_err(|error| format!("{}: mgr keymap entry {}: {error}", path.display(), index + 1))?;
		Ok(Binding { context: super::KeyContext::Manager, keys, commands, description: entry.desc })
	}).collect()
}


#[cfg(test)]
mod tests {
	use std::{fs, time::{SystemTime, UNIX_EPOCH}};
	use crossterm::event::{KeyCode, KeyModifiers};
	use crate::command::{Command, CursorTarget};

	use super::*;

	#[test]
	fn parses_keys_and_multiple_commands() {
		let document: Document = toml::from_str("[mgr]\nkeymap = [{ on = ['g', '<C-p>'], run = ['cursor top', 'preview toggle'], desc = 'test' }]").unwrap();
		let bindings = entries(document.mgr.keymap.unwrap(), Path::new("test.toml")).unwrap();
		assert_eq!(bindings[0].keys, vec![Key::char('g'), Key::new(KeyCode::Char('p'), KeyModifiers::CONTROL)]);
		assert_eq!(bindings[0].commands, vec![Command::Cursor(CursorTarget::Top), Command::TogglePreview]);
	}

	#[test]
	fn prepend_overrides_a_default_and_append_adds_a_binding() {
		let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
		let directory = std::env::temp_dir().join(format!("tuzi-keymap-test-{}-{nonce}", std::process::id()));
		fs::create_dir(&directory).unwrap();
		fs::write(directory.join("keymap.toml"), "[mgr]\nprepend_keymap = [{ on = 'q', run = 'escape', desc = 'Do not quit' }]\nappend_keymap = [{ on = '<F2>', run = 'preview toggle', desc = 'Preview' }]\n").unwrap();
		let keymap = load(&LoadOptions { config_dir: Some(directory.clone()), no_config: false, ..Default::default() }).unwrap();
		let q = keymap.bindings(super::super::KeyContext::Manager).find(|binding| binding.keys == [Key::char('q')]).unwrap();
		assert_eq!(q.commands, [Command::Escape]);
		assert!(keymap.bindings(super::super::KeyContext::Manager).any(|binding| binding.keys == [Key::plain(KeyCode::F(2))]));
		fs::remove_dir_all(directory).unwrap();
	}

	#[test]
	fn runtime_config_can_override_keymap() {
		let options = LoadOptions {
			no_config: true,
			runtime_config: vec![crate::config::RuntimeConfigSource::Inline(
				r#"{"keymap":{"mgr":{"prepend_keymap":[{"on":"q","run":"escape","desc":"Stay open"}]}}}"#.into(),
			)],
			..Default::default()
		};
		let keymap = load(&options).unwrap();
		let q = keymap.bindings(super::super::KeyContext::Manager).find(|binding| binding.keys == [Key::char('q')]).unwrap();
		assert_eq!(q.commands, [Command::Escape]);
	}

	#[test]
	fn rejects_unknown_commands_and_ambiguous_prefixes() {
		let bad: Document = toml::from_str("[mgr]\nkeymap = [{ on = 'x', run = 'unknown', desc = 'bad' }]").unwrap();
		assert!(entries(bad.mgr.keymap.unwrap(), Path::new("bad.toml")).is_err());
		let bindings = entries(toml::from_str::<Document>("[mgr]\nkeymap = [{ on = 'g', run = 'quit', desc = 'short' }, { on = ['g', 'g'], run = 'quit', desc = 'long' }]").unwrap().mgr.keymap.unwrap(), Path::new("bad.toml")).unwrap();
		assert!(Keymap::new(bindings).is_err());
	}
}
