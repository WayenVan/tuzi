use std::{collections::{HashMap, HashSet}, path::Path};

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use crate::config::{LoadOptions, read_user_file};

const PRESET: &str = include_str!("../preset/theme-default.toml");
const REQUIRED_STYLES: &[&str] = &[
	"mgr.cursor_unfocused", "mgr.error", "mgr.loading", "mgr.symlink", "mgr.find_match", "mgr.marker_visual", "mgr.marker_selected", "mgr.marker_cut", "mgr.marker_copy",
	"win.cwd", "win.badge_copy", "win.badge_cut", "tabs.active", "tabs.inactive", "tabs.separator", "tabs.outer",
	"status.normal", "status.select", "status.unset", "status.normal_alt", "status.select_alt", "status.unset_alt", "status.perm_none", "status.perm_read", "status.perm_write", "status.perm_exec", "status.perm_type", "status.task",
	"popup.border", "popup.selected", "popup.muted", "popup.warning", "popup.danger", "popup.cancel", "which.key", "which.separator", "which.description",
	"preview.border", "preview.error", "prompt.insert", "prompt.normal", "prompt.visual", "prompt.search", "tasks.normal", "tasks.selected", "tasks.failed", "tasks.progress", "notify.info", "notify.warn", "notify.error",
];

#[derive(Clone, Debug)]
pub struct Theme {
	pub icon: IconConfig,
	styles:   HashMap<String, Style>,
}

#[derive(Clone, Debug)]
pub struct IconConfig {
	pub enabled:        bool,
	pub directory:      IconStyle,
	pub directory_open: IconStyle,
	pub symlink:        IconStyle,
	pub broken_symlink: IconStyle,
	pub fallback:       IconStyle,
	pub globs:           Vec<GlobRule>,
	pub dirs:            Vec<NameRule>,
	pub files:           Vec<NameRule>,
	pub exts:            Vec<NameRule>,
}

#[derive(Clone, Debug)]
pub struct IconStyle {
	pub text:  String,
	pub color: Color,
}

#[derive(Clone, Debug)]
pub struct NameRule {
	pub name:  String,
	pub icon:  IconStyle,
}

#[derive(Clone, Debug)]
pub struct GlobRule {
	pub pattern: String,
	pub icon:    IconStyle,
}

impl Theme {
	pub fn load(options: &LoadOptions) -> Result<Self, String> {
		let preset: ThemeDocument = toml::from_str(PRESET)
			.map_err(|error| format!("invalid embedded preset/theme-default.toml (this is a Tuzi bug): {error}"))?;
		let mut icon = IconConfig::from_document(preset.icon, Path::new("preset/theme-default.toml"))?;
		let mut styles = preset.style.into_iter().map(|(name, value)| style(value, Path::new("preset/theme-default.toml")).map(|style| (name, style))).collect::<Result<HashMap<_, _>, _>>()?;
		for name in REQUIRED_STYLES {
			if !styles.contains_key(*name) { return Err(format!("embedded preset/theme-default.toml is missing style '{name}' (this is a Tuzi bug)")); }
		}
		if let Some((path, source)) = read_user_file(options, "theme.toml")? {
			let user: UserTheme = toml::from_str(&source).map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
			icon.overlay(user.icon, &path)?;
			for (name, patch) in user.style {
				let target = styles.get_mut(&name).ok_or_else(|| format!("{}: unknown style '{name}'", path.display()))?;
				patch_style(target, patch, &path)?;
			}
		}
		Ok(Self { icon, styles })
	}

	pub fn style(&self, name: &str) -> Style { *self.styles.get(name).unwrap_or_else(|| panic!("missing embedded theme style: {name}")) }
}

impl Default for Theme {
	fn default() -> Self { Self::load(&LoadOptions { config_dir: None, no_config: true }).expect("built-in theme must be valid") }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeDocument { icon: IconDocument, style: HashMap<String, RawStyle> }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IconDocument {
	enabled:        bool,
	directory:      RawIcon,
	directory_open: RawIcon,
	symlink:        RawIcon,
	broken_symlink: RawIcon,
	fallback:       RawIcon,
	#[serde(default)] globs: Vec<RawGlob>,
	#[serde(default)] dirs:  Vec<RawName>,
	#[serde(default)] files: Vec<RawName>,
	#[serde(default)] exts:  Vec<RawName>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserTheme { icon: UserIcon, style: HashMap<String, RawStyle> }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UserIcon {
	enabled:        Option<bool>,
	directory:      Option<RawIcon>,
	directory_open: Option<RawIcon>,
	symlink:        Option<RawIcon>,
	broken_symlink: Option<RawIcon>,
	fallback:       Option<RawIcon>,
	globs:          Option<Vec<RawGlob>>,
	dirs:           Option<Vec<RawName>>,
	files:          Option<Vec<RawName>>,
	exts:           Option<Vec<RawName>>,
	prepend_globs:  Vec<RawGlob>,
	append_globs:   Vec<RawGlob>,
	prepend_dirs:   Vec<RawName>,
	append_dirs:    Vec<RawName>,
	prepend_files:  Vec<RawName>,
	append_files:   Vec<RawName>,
	prepend_exts:   Vec<RawName>,
	append_exts:    Vec<RawName>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIcon { text: String, fg: String }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawName { name: String, text: String, fg: String }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGlob { url: String, text: String, fg: String }

#[derive(Clone, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawStyle {
	fg:        Option<String>,
	bg:        Option<String>,
	bold:      Option<bool>,
	italic:    Option<bool>,
	underline: Option<bool>,
	reverse:   Option<bool>,
}

impl IconConfig {
	fn from_document(value: IconDocument, path: &Path) -> Result<Self, String> {
		Ok(Self {
			enabled: value.enabled,
			directory: icon(value.directory, path)?,
			directory_open: icon(value.directory_open, path)?,
			symlink: icon(value.symlink, path)?,
			broken_symlink: icon(value.broken_symlink, path)?,
			fallback: icon(value.fallback, path)?,
			globs: glob_rules(value.globs, path)?,
			dirs: name_rules(value.dirs, path)?,
			files: name_rules(value.files, path)?,
			exts: name_rules(value.exts, path)?,
		})
	}

	fn overlay(&mut self, value: UserIcon, path: &Path) -> Result<(), String> {
		if let Some(enabled) = value.enabled { self.enabled = enabled; }
		if let Some(value) = value.directory { self.directory = icon(value, path)?; }
		if let Some(value) = value.directory_open { self.directory_open = icon(value, path)?; }
		if let Some(value) = value.symlink { self.symlink = icon(value, path)?; }
		if let Some(value) = value.broken_symlink { self.broken_symlink = icon(value, path)?; }
		if let Some(value) = value.fallback { self.fallback = icon(value, path)?; }
		merge(&mut self.globs, value.globs.map(|v| glob_rules(v, path)).transpose()?, glob_rules(value.prepend_globs, path)?, glob_rules(value.append_globs, path)?);
		merge_names(&mut self.dirs, value.dirs.map(|v| name_rules(v, path)).transpose()?, name_rules(value.prepend_dirs, path)?, name_rules(value.append_dirs, path)?, path, "dirs")?;
		merge_names(&mut self.files, value.files.map(|v| name_rules(v, path)).transpose()?, name_rules(value.prepend_files, path)?, name_rules(value.append_files, path)?, path, "files")?;
		merge_names(&mut self.exts, value.exts.map(|v| name_rules(v, path)).transpose()?, name_rules(value.prepend_exts, path)?, name_rules(value.append_exts, path)?, path, "exts")?;
		validate_unique(&self.dirs, path, "dirs")?;
		validate_unique(&self.files, path, "files")?;
		validate_unique(&self.exts, path, "exts")?;
		Ok(())
	}
}

fn merge_names(base: &mut Vec<NameRule>, replacement: Option<Vec<NameRule>>, prepend: Vec<NameRule>, append: Vec<NameRule>, path: &Path, name: &str) -> Result<(), String> {
	validate_unique(&prepend, path, &format!("prepend_{name}"))?;
	validate_unique(&append, path, &format!("append_{name}"))?;
	let mut core = replacement.unwrap_or_else(|| std::mem::take(base));
	for rule in &prepend { core.retain(|old| old.name != rule.name); }
	let mut merged = prepend;
	merged.append(&mut core);
	for rule in append {
		if !merged.iter().any(|old| old.name == rule.name) { merged.push(rule); }
	}
	*base = merged;
	Ok(())
}

fn merge<T>(base: &mut Vec<T>, replacement: Option<Vec<T>>, mut prepend: Vec<T>, mut append: Vec<T>) {
	let mut core = replacement.unwrap_or_else(|| std::mem::take(base));
	prepend.append(&mut core);
	prepend.append(&mut append);
	*base = prepend;
}

fn icon(value: RawIcon, path: &Path) -> Result<IconStyle, String> {
	if value.text.is_empty() { return Err(format!("{}: icon text cannot be empty", path.display())); }
	Ok(IconStyle { text: value.text, color: parse_color(&value.fg).map_err(|error| format!("{}: {error}", path.display()))? })
}

fn name_rules(values: Vec<RawName>, path: &Path) -> Result<Vec<NameRule>, String> {
	values.into_iter().map(|value| {
		let RawName { name, text, fg } = value;
		if name.is_empty() { return Err(format!("{}: icon rule name cannot be empty", path.display())); }
		Ok(NameRule { name: name.to_ascii_lowercase(), icon: icon(RawIcon { text, fg }, path)? })
	}).collect()
}

fn glob_rules(values: Vec<RawGlob>, path: &Path) -> Result<Vec<GlobRule>, String> {
	values.into_iter().map(|value| {
		let RawGlob { url, text, fg } = value;
		if url.is_empty() { return Err(format!("{}: icon glob cannot be empty", path.display())); }
		Ok(GlobRule { pattern: url, icon: icon(RawIcon { text, fg }, path)? })
	}).collect()
}

fn validate_unique(values: &[NameRule], path: &Path, name: &str) -> Result<(), String> {
	let mut seen = HashSet::new();
	for value in values {
		if !seen.insert(&value.name) { return Err(format!("{}: duplicate icon name '{}' in {name}", path.display(), value.name)); }
	}
	Ok(())
}

fn style(value: RawStyle, path: &Path) -> Result<Style, String> {
	let mut target = Style::new();
	patch_style(&mut target, value, path)?;
	Ok(target)
}

fn patch_style(target: &mut Style, value: RawStyle, path: &Path) -> Result<(), String> {
	if let Some(value) = value.fg { target.fg = Some(parse_color(&value).map_err(|error| format!("{}: {error}", path.display()))?); }
	if let Some(value) = value.bg { target.bg = Some(parse_color(&value).map_err(|error| format!("{}: {error}", path.display()))?); }
	for (value, modifier) in [(value.bold, Modifier::BOLD), (value.italic, Modifier::ITALIC), (value.underline, Modifier::UNDERLINED), (value.reverse, Modifier::REVERSED)] {
		match value { Some(true) => { target.add_modifier.insert(modifier); target.sub_modifier.remove(modifier); }, Some(false) => { target.add_modifier.remove(modifier); target.sub_modifier.insert(modifier); }, None => {} }
	}
	Ok(())
}

pub fn parse_color(value: &str) -> Result<Color, String> {
	if let Some(hex) = value.strip_prefix('#') {
		if hex.len() != 6 { return Err(format!("invalid color '{value}', expected #RRGGBB")); }
		let number = u32::from_str_radix(hex, 16).map_err(|_| format!("invalid color '{value}'"))?;
		return Ok(Color::Rgb((number >> 16) as u8, (number >> 8) as u8, number as u8));
	}
	match value.to_ascii_lowercase().as_str() {
		"reset" => Ok(Color::Reset), "black" => Ok(Color::Black), "red" => Ok(Color::Red), "green" => Ok(Color::Green),
		"yellow" => Ok(Color::Yellow), "blue" => Ok(Color::Blue), "magenta" => Ok(Color::Magenta), "cyan" => Ok(Color::Cyan),
		"gray" | "grey" => Ok(Color::Gray), "dark-gray" | "dark-grey" => Ok(Color::DarkGray),
		"light-red" => Ok(Color::LightRed), "light-green" => Ok(Color::LightGreen), "light-yellow" => Ok(Color::LightYellow),
		"light-blue" => Ok(Color::LightBlue), "light-magenta" => Ok(Color::LightMagenta), "light-cyan" => Ok(Color::LightCyan),
		"white" => Ok(Color::White), _ => Err(format!("unknown color '{value}'")),
	}
}

#[cfg(test)]
mod tests {
	use std::{fs, time::{SystemTime, UNIX_EPOCH}};

	use super::*;

	#[test]
	fn parses_named_and_rgb_colors() {
		assert_eq!(parse_color("cyan").unwrap(), Color::Cyan);
		assert_eq!(parse_color("#89b4fa").unwrap(), Color::Rgb(0x89, 0xb4, 0xfa));
		assert!(parse_color("#bad").is_err());
	}

	#[test]
	fn user_icon_rules_overlay_the_embedded_theme() {
		let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
		let directory = std::env::temp_dir().join(format!("tuzi-theme-test-{}-{nonce}", std::process::id()));
		fs::create_dir(&directory).unwrap();
		fs::write(directory.join("theme.toml"), "[icon]\ndirectory = { text = 'D', fg = '#112233' }\nprepend_exts = [{ name = 'rs', text = 'R', fg = 'red' }]\n[style]\n'mgr.error' = { fg = '#abcdef', bold = true }\n").unwrap();
		let theme = Theme::load(&LoadOptions { config_dir: Some(directory.clone()), no_config: false }).unwrap();
		assert_eq!(theme.icon.directory.text, "D");
		assert_eq!(theme.icon.directory.color, Color::Rgb(0x11, 0x22, 0x33));
		assert_eq!(theme.icon.exts[0].name, "rs");
		assert_eq!(theme.style("mgr.error").fg, Some(Color::Rgb(0xab, 0xcd, 0xef)));
		assert!(theme.style("mgr.error").add_modifier.contains(Modifier::BOLD));
		fs::remove_dir_all(directory).unwrap();
	}
}
