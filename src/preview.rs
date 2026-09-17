use std::{collections::VecDeque, fs::File, io::{BufRead, BufReader, Read}, path::{Path, PathBuf}, sync::{Arc, OnceLock, atomic::{AtomicU64, Ordering}}, time::UNIX_EPOCH};

use syntect_no_panic::{easy::{HighlightLines, HighlightOptions}, highlighting::{FontStyle, ThemeSet}, parsing::SyntaxSet};
use tokio::sync::mpsc::UnboundedSender;

use crate::{config::Preview as PreviewConfig, core::Node, event::Event, scheduler::PreviewScheduler};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PreviewKey {
	pub path:     PathBuf,
	pub len:      u64,
	pub modified: Option<(u64, u32)>,
	pub width:    u16,
	pub height:   u16,
	pub skip:     usize,
}

#[derive(Clone, Debug)]
pub struct PreviewTarget {
	pub path:     PathBuf,
	pub len:      u64,
	pub modified: Option<(u64, u32)>,
	pub is_dir:   bool,
}

impl PreviewTarget {
	pub fn from_node(node: &Node) -> Self {
		let modified = node.cha.modified.and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|d| (d.as_secs(), d.subsec_nanos()));
		Self { path: node.path.clone(), len: node.cha.len, modified, is_dir: node.cha.is_dir }
	}
}

#[derive(Clone, Debug)]
pub struct PreviewSpan {
	pub text:      String,
	pub foreground: Option<(u8, u8, u8)>,
	pub bold:      bool,
	pub italic:    bool,
	pub underline: bool,
}

#[derive(Clone, Debug)]
pub struct PreviewData {
	pub lines:    Vec<Vec<PreviewSpan>>,
	pub eof:      bool,
	pub max_skip: usize,
}

#[derive(Clone, Debug)]
pub enum PreviewState {
	Empty,
	Loading,
	Ready(Arc<PreviewData>),
	Error(String),
}

pub struct Preview {
	pub visible: bool,
	pub skip:    usize,
	pub width:   u16,
	pub height:  u16,
	pub state:   PreviewState,

	current:     Option<PreviewKey>,
	scheduler:   PreviewScheduler,
	cache:       VecDeque<(PreviewKey, Arc<PreviewData>, usize)>,
	cache_bytes: usize,
	config:      PreviewConfig,
}

impl Preview {
	#[cfg(test)]
	pub fn new(tab: usize, tx: UnboundedSender<Event>) -> Self {
		let mut config = crate::config::Config::default().preview;
		config.show = false;
		Self::configured(tab, tx, config)
	}

	pub fn configured(tab: usize, tx: UnboundedSender<Event>, config: PreviewConfig) -> Self {
		Self {
			visible: config.show,
			skip: 0,
			width: 0,
			height: 0,
			state: PreviewState::Empty,
			current: None,
			scheduler: PreviewScheduler::new(tab, tx, config.clone()),
			cache: VecDeque::new(),
			cache_bytes: 0,
			config,
		}
	}

	pub fn toggle(&mut self) {
		self.visible = !self.visible;
		if !self.visible {
			self.scheduler.cancel();
		}
	}

	pub fn seek(&mut self, units: i16) {
		if !self.visible {
			return;
		}
		let step = ((self.height as usize * units.unsigned_abs() as usize) / 10).max(1);
		self.skip = if units < 0 { self.skip.saturating_sub(step) } else { self.skip.saturating_add(step) };
		self.current = None;
	}

	pub fn target_changed(&mut self) {
		self.skip = 0;
		self.current = None;
		self.scheduler.cancel();
		self.state = if self.visible { PreviewState::Loading } else { PreviewState::Empty };
	}

	/// Synchronizes the desired viewport. Returns `true` when a cached result
	/// became ready synchronously after the current frame was drawn, so the
	/// caller can enqueue another redraw.
	pub fn sync(&mut self, target: Option<PreviewTarget>, width: u16, height: u16) -> bool {
		self.width = width;
		self.height = height;
		if !self.visible || width == 0 || height == 0 {
			return false;
		}
		let Some(target) = target else {
			self.state = PreviewState::Empty;
			return false;
		};
		if target.is_dir {
			self.scheduler.cancel();
			self.current = None;
			self.state = PreviewState::Empty;
			return false;
		}

		let key = PreviewKey { path: target.path, len: target.len, modified: target.modified, width, height, skip: self.skip };
		if self.current.as_ref() == Some(&key) {
			return false;
		}
		self.current = Some(key.clone());

		if let Some(data) = self.cache_get(&key) {
			self.state = PreviewState::Ready(data);
			return true;
		}

		self.state = PreviewState::Loading;
		self.scheduler.spawn(key);
		false
	}

	pub fn accept(&mut self, ticket: u64, key: PreviewKey, result: Result<PreviewData, String>) {
		if !self.scheduler.accept(ticket) || self.current.as_ref() != Some(&key) {
			return;
		}
		match result {
			Ok(data) if data.eof && key.skip > data.max_skip => {
				self.skip = data.max_skip;
				self.current = None;
			},
			Ok(data) => {
				let data = Arc::new(data);
				self.cache_insert(key, data.clone());
				self.state = PreviewState::Ready(data);
			},
			Err(error) if error == "cancelled" => {},
			Err(error) => self.state = PreviewState::Error(error),
		}
	}

	fn cache_get(&mut self, key: &PreviewKey) -> Option<Arc<PreviewData>> {
		let index = self.cache.iter().position(|(candidate, ..)| candidate == key)?;
		let entry = self.cache.remove(index)?;
		let data = entry.1.clone();
		self.cache.push_back(entry);
		Some(data)
	}

	fn cache_insert(&mut self, key: PreviewKey, data: Arc<PreviewData>) {
		let bytes = data.lines.iter().flatten().map(|span| span.text.len()).sum();
		if bytes > self.config.cache_bytes {
			return;
		}
		while self.cache_bytes + bytes > self.config.cache_bytes {
			let Some((_, _, removed)) = self.cache.pop_front() else { break };
			self.cache_bytes -= removed;
		}
		self.cache_bytes += bytes;
		self.cache.push_back((key, data, bytes));
	}
}

pub(crate) fn read_text(key: &PreviewKey, guard: &AtomicU64, generation: u64, config: &PreviewConfig) -> Result<PreviewData, String> {
	let file = File::open(&key.path).map_err(|error| error.to_string())?;
	// Bound the reader itself as well as the accounting below. `read_until`
	// may otherwise allocate an entire pathological single-line file before
	// we get a chance to reject it.
	let mut reader = BufReader::new(file.take((config.max_scan_bytes + 1) as u64));
	let syntaxes = syntaxes();
	let syntax = syntax_for(syntaxes, &key.path);
	let themes = themes();
	let theme = themes.themes.get("base16-ocean.dark").or_else(|| themes.themes.values().next()).ok_or("no syntax theme")?;
	let mut highlighter = config.syntax_highlight.then(|| syntax.map(|syntax| HighlightLines::new(syntax, theme, HighlightOptions { ignore_errors: true }))).flatten();
	let mut lines = Vec::with_capacity(key.height as usize + config.overscan_lines);
	let limit = key.skip + key.height as usize + config.overscan_lines;
	let mut line_number = 0usize;
	let mut scanned = 0usize;
	let mut inspected = 0usize;
	let mut eof = false;
	let mut buf = Vec::new();

	while line_number < limit {
		if guard.load(Ordering::Relaxed) != generation {
			return Err("cancelled".into());
		}
		buf.clear();
		let count = reader.read_until(b'\n', &mut buf).map_err(|error| error.to_string())?;
		if count == 0 {
			eof = true;
			break;
		}
		scanned += count;
		if scanned > config.max_scan_bytes {
			return Err(format!("preview scan exceeded {} bytes", config.max_scan_bytes));
		}
		if inspected < 1024 {
			let end = (1024 - inspected).min(buf.len());
			if buf[..end].contains(&0) {
				return Err("binary file".into());
			}
			inspected += end;
		}

		let text = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_owned();
		let spans = if text.len() > config.max_line_bytes {
			highlighter = None;
			vec![plain_span(text)]
		} else if let Some(highlighter) = &mut highlighter {
			highlighter
				.highlight_line(&text, syntaxes)
				.map_err(|error| error.to_string())?
				.into_iter()
				.map(|(style, text)| PreviewSpan {
					text: text.to_owned(),
					foreground: Some((style.foreground.r, style.foreground.g, style.foreground.b)),
					bold: style.font_style.contains(FontStyle::BOLD),
					italic: style.font_style.contains(FontStyle::ITALIC),
					underline: style.font_style.contains(FontStyle::UNDERLINE),
				})
				.collect()
		} else {
			vec![plain_span(text)]
		};
		if line_number >= key.skip {
			lines.push(spans);
		}
		line_number += 1;
	}

	Ok(PreviewData { lines, eof, max_skip: line_number.saturating_sub(key.height as usize) })
}

fn plain_span(text: String) -> PreviewSpan {
	PreviewSpan { text, foreground: None, bold: false, italic: false, underline: false }
}

fn syntaxes() -> &'static SyntaxSet {
	static SET: OnceLock<SyntaxSet> = OnceLock::new();
	SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
	static SET: OnceLock<ThemeSet> = OnceLock::new();
	SET.get_or_init(ThemeSet::load_defaults)
}

fn syntax_for<'a>(syntaxes: &'a SyntaxSet, path: &Path) -> Option<&'a syntect_no_panic::parsing::SyntaxReference> {
	path.extension().and_then(|ext| ext.to_str()).and_then(|ext| syntaxes.find_syntax_by_extension(ext))
}

#[cfg(test)]
mod tests {
	use std::{fs, sync::atomic::AtomicU64};

	use super::*;

	fn config() -> PreviewConfig { crate::config::Config::default().preview }

	#[test]
	fn reads_only_the_requested_text_window() {
		let path = std::env::temp_dir().join("tuzi-preview-window.txt");
		fs::write(&path, "zero\none\ntwo\nthree\nfour\n").unwrap();
		let key = PreviewKey { path: path.clone(), len: 24, modified: None, width: 40, height: 2, skip: 2 };
		let data = read_text(&key, &AtomicU64::new(1), 1, &config()).unwrap();
		let text: Vec<String> = data.lines.iter().map(|line| line.iter().map(|span| span.text.as_str()).collect()).collect();
		assert_eq!(&text[..2], ["two", "three"]);
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn rejects_binary_input() {
		let path = std::env::temp_dir().join("tuzi-preview-binary");
		fs::write(&path, b"hello\0world").unwrap();
		let key = PreviewKey { path: path.clone(), len: 11, modified: None, width: 40, height: 2, skip: 0 };
		assert_eq!(read_text(&key, &AtomicU64::new(1), 1, &config()).unwrap_err(), "binary file");
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn empty_file_is_a_successful_empty_preview() {
		let path = std::env::temp_dir().join("tuzi-preview-empty");
		fs::write(&path, []).unwrap();
		let key = PreviewKey { path: path.clone(), len: 0, modified: None, width: 40, height: 2, skip: 0 };
		let data = read_text(&key, &AtomicU64::new(1), 1, &config()).unwrap();
		assert!(data.lines.is_empty());
		assert!(data.eof);
		assert_eq!(data.max_skip, 0);
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn scan_limit_stops_pathological_input() {
		let path = std::env::temp_dir().join("tuzi-preview-scan-limit.txt");
		fs::write(&path, vec![b'x'; 70 * 1024]).unwrap();
		let key = PreviewKey { path: path.clone(), len: 70 * 1024, modified: None, width: 40, height: 2, skip: 0 };
		let mut limits = config();
		limits.max_scan_bytes = 64 * 1024;
		let error = read_text(&key, &AtomicU64::new(1), 1, &limits).unwrap_err();
		assert_eq!(error, "preview scan exceeded 65536 bytes");
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn syntax_highlight_can_be_disabled() {
		let path = std::env::temp_dir().join("tuzi-preview-plain.rs");
		fs::write(&path, "fn main() {}\n").unwrap();
		let key = PreviewKey { path: path.clone(), len: 13, modified: None, width: 40, height: 2, skip: 0 };
		let mut limits = config();
		limits.syntax_highlight = false;
		let data = read_text(&key, &AtomicU64::new(1), 1, &limits).unwrap();
		assert!(data.lines.iter().flatten().all(|span| span.foreground.is_none()));
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn zero_cache_capacity_keeps_no_entries() {
		let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
		let mut limits = config();
		limits.cache_bytes = 0;
		let mut preview = Preview::configured(0, tx, limits);
		let key = PreviewKey { path: PathBuf::from("uncached.txt"), len: 4, modified: None, width: 40, height: 10, skip: 0 };
		preview.cache_insert(key, Arc::new(PreviewData { lines: vec![vec![plain_span("text".into())]], eof: true, max_skip: 0 }));
		assert!(preview.cache.is_empty());
	}

	#[test]
	fn cached_viewport_requests_a_follow_up_redraw() {
		let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
		let mut preview = Preview::new(0, tx);
		preview.visible = true;
		let path = PathBuf::from("cached.txt");
		let key = PreviewKey { path: path.clone(), len: 4, modified: None, width: 40, height: 10, skip: 0 };
		preview.cache_insert(key, Arc::new(PreviewData { lines: vec![vec![plain_span("text".into())]], eof: true, max_skip: 0 }));

		let target = PreviewTarget { path, len: 4, modified: None, is_dir: false };
		assert!(preview.sync(Some(target), 40, 10));
		assert!(matches!(preview.state, PreviewState::Ready(_)));
	}
}
