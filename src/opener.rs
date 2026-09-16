use std::{collections::HashMap, fs::File, io::{self, Read}, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::SystemTime};

use tokio::sync::mpsc::UnboundedSender;

use crate::event::Event;

#[derive(Clone, Debug)]
pub struct OpenTarget {
	pub path: PathBuf,
	pub mime: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenMode {
	Open,
	Reveal,
}

impl OpenMode {
	pub const ALL: [Self; 2] = [Self::Open, Self::Reveal];

	pub const fn label(self) -> &'static str {
		match self {
			Self::Open => "Open with the default application",
			Self::Reveal => "Reveal in the file manager",
		}
	}
}

#[derive(Clone)]
pub struct OpenPicker {
	pub cwd:      PathBuf,
	pub targets:  Vec<OpenTarget>,
	pub selected: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenKind {
	Folder,
	Text,
	Image,
	Audio,
	Video,
	Archive,
	Other,
}

impl OpenTarget {
	pub fn kind(&self) -> OpenKind {
		if self.mime == "inode/directory" {
			OpenKind::Folder
		} else if is_text(&self.mime) || self.mime == "inode/empty" {
			OpenKind::Text
		} else if self.mime.starts_with("image/") {
			OpenKind::Image
		} else if self.mime.starts_with("audio/") {
			OpenKind::Audio
		} else if self.mime.starts_with("video/") {
			OpenKind::Video
		} else if is_archive(&self.mime) {
			OpenKind::Archive
		} else {
			OpenKind::Other
		}
	}
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

fn is_text(mime: &str) -> bool {
	mime.starts_with("text/")
		|| matches!(
			mime,
			"application/json"
				| "application/ld+json"
				| "application/javascript"
				| "application/xml"
				| "application/toml"
				| "application/yaml"
				| "application/x-sh"
		)
}

fn is_archive(mime: &str) -> bool {
	matches!(
		mime,
		"application/zip"
			| "application/gzip"
			| "application/x-7z-compressed"
			| "application/x-rar-compressed"
			| "application/x-tar"
			| "application/x-bzip2"
			| "application/x-xz"
			| "application/zstd"
	)
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	#[test]
	fn classifies_mime_groups() {
		let target = |mime: &str| OpenTarget { path: PathBuf::new(), mime: mime.into() };
		assert_eq!(target("inode/directory").kind(), OpenKind::Folder);
		assert_eq!(target("text/plain").kind(), OpenKind::Text);
		assert_eq!(target("image/png").kind(), OpenKind::Image);
		assert_eq!(target("application/zip").kind(), OpenKind::Archive);
		assert_eq!(target("application/octet-stream").kind(), OpenKind::Other);
	}

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
