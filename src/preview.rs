use std::{borrow::Cow, collections::VecDeque, fs::File, io::{BufRead, BufReader, Read}, path::{Path, PathBuf}, sync::{Arc, OnceLock, atomic::{AtomicU64, Ordering}}, time::UNIX_EPOCH};

use ratatui::{style::{Color, Modifier, Style}, text::{Line, Span}};
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
pub struct PreviewData {
	pub lines:    Vec<Line<'static>>,
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
		let bytes = data.lines.iter().flat_map(|line| &line.spans).map(|span| span.content.len()).sum();
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

trait PreviewProvider {
	fn matches(&self, key: &PreviewKey) -> bool;
	fn render(&self, key: &PreviewKey, guard: &AtomicU64, generation: u64, config: &PreviewConfig) -> Result<Option<PreviewData>, String>;
}

struct MarkdownProvider;
struct JsonProvider;
struct TextProvider;
struct FallbackProvider;

pub(crate) fn render_preview(key: &PreviewKey, guard: &AtomicU64, generation: u64, config: &PreviewConfig) -> Result<PreviewData, String> {
	let providers: [&dyn PreviewProvider; 4] = [&MarkdownProvider, &JsonProvider, &TextProvider, &FallbackProvider];
	for provider in providers {
		if provider.matches(key)
			&& let Some(data) = provider.render(key, guard, generation, config)?
		{
			return Ok(data);
		}
	}
	Err("no preview provider".into())
}

impl PreviewProvider for MarkdownProvider {
	fn matches(&self, key: &PreviewKey) -> bool {
		matches!(key.path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase).as_deref(), Some("md" | "markdown" | "mdown" | "mkd" | "mkdn"))
	}

	fn render(&self, key: &PreviewKey, guard: &AtomicU64, generation: u64, config: &PreviewConfig) -> Result<Option<PreviewData>, String> {
		check_cancelled(guard, generation)?;
		let mut file = File::open(&key.path).map_err(|error| error.to_string())?;
		let mut bytes = Vec::new();
		file.by_ref().take((config.max_scan_bytes + 1) as u64).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
		if bytes.len() > config.max_scan_bytes {
			return Err(format!("preview scan exceeded {} bytes", config.max_scan_bytes));
		}
		if bytes.iter().take(1024).any(|byte| *byte == 0) {
			return Ok(None);
		}
		check_cancelled(guard, generation)?;
		let source = String::from_utf8_lossy(&bytes);
		let rendered = tui_markdown::from_str(&source);
		let total = rendered.lines.len();
		let limit = key.skip.saturating_add(key.height as usize).saturating_add(config.overscan_lines);
		let lines = rendered.lines.into_iter().skip(key.skip).take(limit.saturating_sub(key.skip)).map(owned_line).collect();
		Ok(Some(PreviewData { lines, eof: limit >= total, max_skip: total.saturating_sub(key.height as usize) }))
	}
}

impl PreviewProvider for JsonProvider {
	fn matches(&self, key: &PreviewKey) -> bool {
		key.path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
	}

	fn render(&self, key: &PreviewKey, guard: &AtomicU64, generation: u64, config: &PreviewConfig) -> Result<Option<PreviewData>, String> {
		check_cancelled(guard, generation)?;
		let mut file = File::open(&key.path).map_err(|error| error.to_string())?;
		let mut bytes = Vec::new();
		file.by_ref().take((config.max_scan_bytes + 1) as u64).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
		if bytes.len() > config.max_scan_bytes {
			return Err(format!("preview scan exceeded {} bytes", config.max_scan_bytes));
		}
		if bytes.iter().take(1024).any(|byte| *byte == 0) {
			return Ok(None);
		}
		let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
			return Ok(None);
		};
		check_cancelled(guard, generation)?;
		let pretty = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
		Ok(Some(render_text_lines(&pretty, key, config)?))
	}
}

impl PreviewProvider for TextProvider {
	fn matches(&self, _key: &PreviewKey) -> bool { true }

	fn render(&self, key: &PreviewKey, guard: &AtomicU64, generation: u64, config: &PreviewConfig) -> Result<Option<PreviewData>, String> {
		read_text(key, guard, generation, config)
	}
}

fn render_text_lines(text: &str, key: &PreviewKey, config: &PreviewConfig) -> Result<PreviewData, String> {
	let syntaxes = syntaxes();
	let syntax = syntax_for(syntaxes, &key.path);
	let themes = themes();
	let theme = themes.themes.get("base16-ocean.dark").or_else(|| themes.themes.values().next()).ok_or("no syntax theme")?;
	let mut highlighter = config.syntax_highlight.then(|| syntax.map(|syntax| HighlightLines::new(syntax, theme, HighlightOptions { ignore_errors: true }))).flatten();
	let all_lines: Vec<&str> = text.lines().collect();
	let total = all_lines.len();
	let limit = key.skip.saturating_add(key.height as usize).saturating_add(config.overscan_lines);
	let mut lines = Vec::with_capacity(limit.saturating_sub(key.skip).min(total));
	for source in all_lines.into_iter().skip(key.skip).take(limit.saturating_sub(key.skip)) {
		let line = if source.len() > config.max_line_bytes {
			highlighter = None;
			Line::from(source.to_owned())
		} else if let Some(highlighter) = &mut highlighter {
			Line::from(highlighter.highlight_line(source, syntaxes).map_err(|error| error.to_string())?.into_iter().map(|(style, text)| PreviewSpan { text: text.to_owned(), style }.into_ratatui()).collect::<Vec<_>>())
		} else {
			Line::from(source.to_owned())
		};
		lines.push(line);
	}
	Ok(PreviewData { lines, eof: limit >= total, max_skip: total.saturating_sub(key.height as usize) })
}

impl PreviewProvider for FallbackProvider {
	fn matches(&self, _key: &PreviewKey) -> bool { true }

	fn render(&self, key: &PreviewKey, _guard: &AtomicU64, _generation: u64, _config: &PreviewConfig) -> Result<Option<PreviewData>, String> {
		let kind = infer::get_from_path(&key.path).map_err(|error| error.to_string())?.map_or("binary file", |kind| kind.mime_type());
		let lines = vec![Line::from(format!("No preview available for {kind}")), Line::from(format!("Size: {} bytes", key.len))];
		Ok(Some(PreviewData { lines, eof: true, max_skip: 0 }))
	}
}

fn read_text(key: &PreviewKey, guard: &AtomicU64, generation: u64, config: &PreviewConfig) -> Result<Option<PreviewData>, String> {
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
				return Ok(None);
			}
			inspected += end;
		}

		let text = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_owned();
		let line = if text.len() > config.max_line_bytes {
			highlighter = None;
			Line::from(text)
		} else if let Some(highlighter) = &mut highlighter {
			Line::from(highlighter
				.highlight_line(&text, syntaxes)
				.map_err(|error| error.to_string())?
				.into_iter()
				.map(|(style, text)| PreviewSpan {
					text: text.to_owned(), style,
				})
				.map(|span| span.into_ratatui())
				.collect::<Vec<_>>())
		} else {
			Line::from(text)
		};
		if line_number >= key.skip {
			lines.push(line);
		}
		line_number += 1;
	}

	Ok(Some(PreviewData { lines, eof, max_skip: line_number.saturating_sub(key.height as usize) }))
}

struct PreviewSpan {
	text:  String,
	style: syntect_no_panic::highlighting::Style,
}

impl PreviewSpan {
	fn into_ratatui(self) -> Span<'static> {
		let mut style = Style::new().fg(Color::Rgb(self.style.foreground.r, self.style.foreground.g, self.style.foreground.b));
		if self.style.font_style.contains(FontStyle::BOLD) { style = style.add_modifier(Modifier::BOLD); }
		if self.style.font_style.contains(FontStyle::ITALIC) { style = style.add_modifier(Modifier::ITALIC); }
		if self.style.font_style.contains(FontStyle::UNDERLINE) { style = style.add_modifier(Modifier::UNDERLINED); }
		Span::styled(self.text, style)
	}
}

fn owned_line(line: Line<'_>) -> Line<'static> {
	Line { style: line.style, alignment: line.alignment, spans: line.spans.into_iter().map(|span| Span { style: span.style, content: Cow::Owned(span.content.into_owned()) }).collect() }
}

fn check_cancelled(guard: &AtomicU64, generation: u64) -> Result<(), String> {
	if guard.load(Ordering::Relaxed) == generation { Ok(()) } else { Err("cancelled".into()) }
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
		let data = read_text(&key, &AtomicU64::new(1), 1, &config()).unwrap().unwrap();
		let text: Vec<String> = data.lines.iter().map(ToString::to_string).collect();
		assert_eq!(&text[..2], ["two", "three"]);
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn rejects_binary_input() {
		let path = std::env::temp_dir().join("tuzi-preview-binary");
		fs::write(&path, b"hello\0world").unwrap();
		let key = PreviewKey { path: path.clone(), len: 11, modified: None, width: 40, height: 2, skip: 0 };
		assert!(read_text(&key, &AtomicU64::new(1), 1, &config()).unwrap().is_none());
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn empty_file_is_a_successful_empty_preview() {
		let path = std::env::temp_dir().join("tuzi-preview-empty");
		fs::write(&path, []).unwrap();
		let key = PreviewKey { path: path.clone(), len: 0, modified: None, width: 40, height: 2, skip: 0 };
		let data = read_text(&key, &AtomicU64::new(1), 1, &config()).unwrap().unwrap();
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
		let data = read_text(&key, &AtomicU64::new(1), 1, &limits).unwrap().unwrap();
		assert!(data.lines.iter().flat_map(|line| &line.spans).all(|span| span.style.fg.is_none()));
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn markdown_provider_renders_rich_lines() {
		let path = std::env::temp_dir().join("tuzi-preview-rich.md");
		fs::write(&path, "# Heading\n\n- **bold** item\n").unwrap();
		let key = PreviewKey { path: path.clone(), len: 27, modified: None, width: 40, height: 10, skip: 0 };
		let data = render_preview(&key, &AtomicU64::new(1), 1, &config()).unwrap();
		assert!(data.lines.iter().any(|line| line.to_string().contains("Heading")));
		assert!(data.lines.iter().flat_map(|line| &line.spans).any(|span| span.style.add_modifier.contains(Modifier::BOLD)));
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn json_provider_pretty_prints_valid_json() {
		let path = std::env::temp_dir().join("tuzi-preview-pretty.json");
		fs::write(&path, r#"{"name":"tuzi","enabled":true}"#).unwrap();
		let key = PreviewKey { path: path.clone(), len: 30, modified: None, width: 40, height: 10, skip: 0 };
		let data = render_preview(&key, &AtomicU64::new(1), 1, &config()).unwrap();
		let text: Vec<String> = data.lines.iter().map(ToString::to_string).collect();
		assert_eq!(text, ["{", "  \"enabled\": true,", "  \"name\": \"tuzi\"", "}"]);
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn invalid_json_falls_back_to_text_provider() {
		let path = std::env::temp_dir().join("tuzi-preview-invalid.json");
		fs::write(&path, "{not json}\n").unwrap();
		let key = PreviewKey { path: path.clone(), len: 11, modified: None, width: 40, height: 10, skip: 0 };
		let data = render_preview(&key, &AtomicU64::new(1), 1, &config()).unwrap();
		assert_eq!(data.lines[0].to_string(), "{not json}");
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn binary_input_uses_fallback_provider() {
		let path = std::env::temp_dir().join("tuzi-preview-fallback.bin");
		fs::write(&path, b"hello\0world").unwrap();
		let key = PreviewKey { path: path.clone(), len: 11, modified: None, width: 40, height: 10, skip: 0 };
		let data = render_preview(&key, &AtomicU64::new(1), 1, &config()).unwrap();
		assert!(data.lines[0].to_string().starts_with("No preview available for "));
		fs::remove_file(path).unwrap();
	}

	#[test]
	fn zero_cache_capacity_keeps_no_entries() {
		let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
		let mut limits = config();
		limits.cache_bytes = 0;
		let mut preview = Preview::configured(0, tx, limits);
		let key = PreviewKey { path: PathBuf::from("uncached.txt"), len: 4, modified: None, width: 40, height: 10, skip: 0 };
		preview.cache_insert(key, Arc::new(PreviewData { lines: vec![Line::from("text")], eof: true, max_skip: 0 }));
		assert!(preview.cache.is_empty());
	}

	#[test]
	fn cached_viewport_requests_a_follow_up_redraw() {
		let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
		let mut preview = Preview::new(0, tx);
		preview.visible = true;
		let path = PathBuf::from("cached.txt");
		let key = PreviewKey { path: path.clone(), len: 4, modified: None, width: 40, height: 10, skip: 0 };
		preview.cache_insert(key, Arc::new(PreviewData { lines: vec![Line::from("text")], eof: true, max_skip: 0 }));

		let target = PreviewTarget { path, len: 4, modified: None, is_dir: false };
		assert!(preview.sync(Some(target), 40, 10));
		assert!(matches!(preview.state, PreviewState::Ready(_)));
	}
}
