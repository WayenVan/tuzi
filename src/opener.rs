use std::{collections::HashMap, fs::File, io::{self, Read}, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::SystemTime};

use tokio::sync::mpsc::UnboundedSender;
use serde::Deserialize;

use crate::event::Event;

#[derive(Clone, Debug)]
pub struct OpenTarget {
	pub path: PathBuf,
	pub mime: String,
}

#[derive(Clone)]
pub struct OpenPicker {
	pub cwd:      PathBuf,
	pub targets:  Vec<OpenTarget>,
	pub choices:  Vec<OpenChoice>,
	pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct OpenChoice { pub name: String, pub description: String }

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Opener {
	pub run: String,
	#[serde(default)] pub args: Vec<String>,
	pub desc: String,
	#[serde(rename = "for")] pub platform: Option<String>,
	#[serde(default)] pub block: bool,
	#[serde(default)] pub orphan: bool,
	#[serde(default)] pub per_file: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OpenRule {
	pub mime: Option<String>,
	pub name: Option<String>,
	pub ext: Option<String>,
	pub glob: Option<String>,
	#[serde(rename = "use")] pub openers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpenConfig {
	pub openers: HashMap<String, Vec<Opener>>,
	pub rules:   Vec<OpenRule>,
}

impl OpenConfig {
	pub fn names_for(&self, target: &OpenTarget) -> Vec<&str> {
		self.rules.iter().find(|rule| rule.matches(target)).map_or_else(Vec::new, |rule| rule.openers.iter().map(String::as_str).collect())
	}

	pub fn choices(&self, targets: &[OpenTarget]) -> Vec<OpenChoice> {
		let Some(first) = targets.first() else { return Vec::new() };
		self.names_for(first).into_iter().filter(|name| targets.iter().all(|target| self.names_for(target).contains(name))).filter_map(|name| {
			let opener = self.variant(name)?;
			Some(OpenChoice { name: name.into(), description: opener.desc.clone() })
		}).collect()
	}

	pub fn variant(&self, name: &str) -> Option<&Opener> {
		self.openers.get(name)?.iter().find(|opener| platform_matches(opener.platform.as_deref()))
	}

	pub fn validate(&self) -> Result<(), String> {
		for (name, variants) in &self.openers {
			if variants.is_empty() { return Err(format!("opener '{name}' has no variants")); }
			for opener in variants {
				if opener.run.is_empty() || opener.desc.is_empty() { return Err(format!("opener '{name}' requires run and desc")); }
				if opener.block && opener.orphan { return Err(format!("opener '{name}' cannot be both block and orphan")); }
				if opener.args.iter().any(|arg| matches!(arg.as_str(), "{file}" | "{dir}")) && !opener.per_file { return Err(format!("opener '{name}' uses {{file}} or {{dir}} without per_file = true")); }
				if let Some(platform) = opener.platform.as_deref() && !matches!(platform, "unix" | "macos" | "linux" | "windows") { return Err(format!("opener '{name}' has unknown platform '{platform}'")); }
			}
		}
		for rule in &self.rules {
			if [rule.mime.is_some(), rule.name.is_some(), rule.ext.is_some(), rule.glob.is_some()].into_iter().filter(|set| *set).count() != 1 { return Err("each open rule must set exactly one of mime, name, ext, or glob".into()); }
			if rule.openers.is_empty() { return Err("open rule has no opener names".into()); }
			for name in &rule.openers { if !self.openers.contains_key(name) { return Err(format!("open rule references unknown opener '{name}'")); } }
		}
		Ok(())
	}
}

impl OpenRule {
	fn matches(&self, target: &OpenTarget) -> bool {
		self.mime.as_deref().is_some_and(|pattern| wildcard(pattern, &target.mime))
			|| self.name.as_deref().is_some_and(|name| target.path.file_name().is_some_and(|value| value.to_string_lossy().eq_ignore_ascii_case(name)))
			|| self.ext.as_deref().is_some_and(|ext| target.path.extension().is_some_and(|value| value.to_string_lossy().eq_ignore_ascii_case(ext)))
			|| self.glob.as_deref().is_some_and(|pattern| wildcard(pattern, &target.path.to_string_lossy()))
	}
}

fn platform_matches(platform: Option<&str>) -> bool {
	match platform {
		None => true,
		Some("unix") => cfg!(unix),
		Some("macos") => cfg!(target_os = "macos"),
		Some("linux") => cfg!(target_os = "linux"),
		Some("windows") => cfg!(target_os = "windows"),
		_ => false,
	}
}

fn wildcard(pattern: &str, value: &str) -> bool {
	let (pattern, value): (Vec<_>, Vec<_>) = (pattern.chars().collect(), value.chars().collect());
	let mut reachable = vec![false; value.len() + 1]; reachable[0] = true;
	for token in pattern {
		if token == '*' { for index in 1..=value.len() { reachable[index] |= reachable[index - 1]; } }
		else { for index in (1..=value.len()).rev() { reachable[index] = reachable[index - 1] && (token == '?' || token == value[index - 1]); } reachable[0] = false; }
	}
	reachable[value.len()]
}

#[derive(Clone)]
pub struct OpenScheduler {
	tx:    UnboundedSender<Event>,
	cache: Arc<Mutex<HashMap<PathBuf, CachedMime>>>,
}

#[derive(Clone)]
struct CachedMime {
	len:      u64,
	modified: Option<SystemTime>,
	mime:     String,
}

impl OpenScheduler {
	pub fn new(tx: UnboundedSender<Event>) -> Self { Self { tx, cache: Arc::new(Mutex::new(HashMap::new())) } }

	pub fn open(&self, tab: usize, cwd: PathBuf, paths: Vec<PathBuf>, interactive: bool) {
		let tx = self.tx.clone();
		let cache = self.cache.clone();
		tokio::spawn(async move {
			let result = tokio::task::spawn_blocking(move || {
				paths
					.into_iter()
					.map(|path| detect_cached(&cache, path))
					.collect::<io::Result<Vec<_>>>()
			})
			.await
			.unwrap_or_else(|error| Err(io::Error::other(error)));
			let _ = tx.send(Event::OpenResolved { tab, cwd, interactive, result });
		});
	}
}

fn detect_cached(cache: &Mutex<HashMap<PathBuf, CachedMime>>, path: PathBuf) -> io::Result<OpenTarget> {
	let metadata = std::fs::metadata(&path)?;
	let len = metadata.len();
	let modified = metadata.modified().ok();
	if let Some(cached) = cache.lock().unwrap().get(&path)
		&& cached.len == len
		&& cached.modified == modified
	{
		return Ok(OpenTarget { path, mime: cached.mime.clone() });
	}

	let mime = detect(&path, &metadata)?;
	cache.lock().unwrap().insert(path.clone(), CachedMime { len, modified, mime: mime.clone() });
	Ok(OpenTarget { path, mime })
}

fn detect(path: &Path, metadata: &std::fs::Metadata) -> io::Result<String> {
	if metadata.is_dir() {
		return Ok("inode/directory".into());
	}
	if metadata.len() == 0 {
		return Ok("inode/empty".into());
	}

	let mut file = File::open(path)?;
	let mut bytes = vec![0; metadata.len().min(8192) as usize];
	let read = file.read(&mut bytes)?;
	bytes.truncate(read);
	if let Some(kind) = infer::get(&bytes) {
		return Ok(kind.mime_type().into());
	}
	if looks_like_text(&bytes) {
		return Ok("text/plain".into());
	}
	if let Some(guessed) = mime_guess::from_path(path).first() {
		return Ok(guessed.essence_str().into());
	}
	Ok("application/octet-stream".into())
}

fn looks_like_text(bytes: &[u8]) -> bool { !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok() }

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	#[test]
	fn detects_content_then_extension_and_empty_files() {
		let root = std::env::temp_dir().join("tuzi-mime-test");
		fs::create_dir_all(&root).unwrap();
		let png = root.join("image-with-wrong-extension.txt");
		let disguised_text = root.join("notes.png");
		let rust = root.join("main.rs");
		let empty = root.join("empty");
		fs::write(&png, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").unwrap();
		fs::write(&disguised_text, b"plain text\n").unwrap();
		fs::write(&rust, b"fn main() {}\n").unwrap();
		fs::write(&empty, b"").unwrap();

		assert_eq!(detect(&png, &fs::metadata(&png).unwrap()).unwrap(), "image/png");
		assert_eq!(detect(&disguised_text, &fs::metadata(&disguised_text).unwrap()).unwrap(), "text/plain");
		assert!(detect(&rust, &fs::metadata(&rust).unwrap()).unwrap().starts_with("text/"));
		assert_eq!(detect(&empty, &fs::metadata(&empty).unwrap()).unwrap(), "inode/empty");
		assert_eq!(detect(&root, &fs::metadata(&root).unwrap()).unwrap(), "inode/directory");

		fs::remove_dir_all(root).unwrap();
	}
}
