#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
	Normal,
	Insert,
	Visual,
	Replace,
}

/// A single-line, vim-modal text buffer — generic enough to back rename,
/// create, or (later) filter/search prompts; each caller just picks a
/// `title` and reads `value()` back on confirm. A real (if scoped-down)
/// subset of vim: four modes, word motions, an operator-pending `d` that
/// composes with any motion, visual-range delete, and single-char replace.
pub struct Input {
	pub title:  String,
	pub value:  Vec<char>,
	pub cursor: usize,
	pub mode:   InputMode,
	anchor:     Option<usize>,
	op_pending: bool,
}

impl Input {
	/// Starts in Insert mode with the cursor at the end — you're here to
	/// type over the old name, not read it; press Esc once to drop into
	/// Normal mode for vim-style motions, same as yazi's input does.
	pub fn new(title: impl Into<String>, value: impl Into<String>) -> Self {
		let value: Vec<char> = value.into().chars().collect();
		let cursor = value.len();
		Self { title: title.into(), value, cursor, mode: InputMode::Insert, anchor: None, op_pending: false }
	}

	pub fn value(&self) -> String { self.value.iter().collect() }

	/// The Visual-mode selection as an inclusive `(lo, hi)` character range,
	/// for the renderer to highlight.
	pub fn selection(&self) -> Option<(usize, usize)> {
		let anchor = self.anchor?;
		Some((anchor.min(self.cursor), anchor.max(self.cursor)))
	}

	// -- editing, Insert mode
	pub fn insert(&mut self, c: char) {
		self.value.insert(self.cursor, c);
		self.cursor += 1;
	}

	pub fn backspace(&mut self) {
		if self.cursor > 0 {
			self.cursor -= 1;
			self.value.remove(self.cursor);
		}
	}

	/// `x` in Normal/Visual mode, `<Delete>` in Insert.
	pub fn delete_under(&mut self) {
		self.op_pending = false;
		if self.cursor < self.value.len() {
			self.value.remove(self.cursor);
		}
		self.clamp();
	}

	/// `D`: delete from the cursor to the end of the line.
	pub fn delete_to_eol(&mut self) {
		self.op_pending = false;
		let from = self.cursor;
		self.value.truncate(from);
		self.clamp();
	}

	/// `d`: arms an operator that the *next* motion completes into a
	/// delete over the range travelled — `dw`, `db`, `de`, `d0`, `d$` all
	/// fall out of this for free, since every motion below already calls
	/// `finish_motion`. A second `d` while one's already pending is `dd`:
	/// vim's "delete line", which for a single-line buffer just means
	/// clearing it.
	pub fn op_delete(&mut self) {
		if self.op_pending {
			self.value.clear();
			self.cursor = 0;
			self.op_pending = false;
		} else {
			self.op_pending = true;
		}
	}

	/// `r`: the next character typed overwrites the one under the cursor,
	/// then mode drops back to Normal — vim's single-character replace.
	pub fn enter_replace(&mut self) {
		self.op_pending = false;
		if !self.value.is_empty() {
			self.mode = InputMode::Replace;
		}
	}

	pub fn replace_char(&mut self, c: char) {
		if self.cursor < self.value.len() {
			self.value[self.cursor] = c;
		}
		self.mode = InputMode::Normal;
	}

	/// `v`: enters Visual mode anchored at the cursor, or — pressed again
	/// while already in Visual — exits it without changing anything.
	pub fn toggle_visual(&mut self) {
		self.op_pending = false;
		if self.mode == InputMode::Visual {
			self.mode = InputMode::Normal;
			self.anchor = None;
		} else {
			self.anchor = Some(self.cursor);
			self.mode = InputMode::Visual;
		}
	}

	/// `d`/`x` while in Visual mode: deletes the selected range.
	pub fn delete_visual(&mut self) {
		if let Some((lo, hi)) = self.selection() {
			self.value.drain(lo..=hi.min(self.value.len().saturating_sub(1)));
			self.cursor = lo;
		}
		self.anchor = None;
		self.mode = InputMode::Normal;
		self.clamp();
	}

	// -- motion, Normal/Visual — the cursor stays "on" a character (max
	// index len-1); Insert instead lets it sit one past the end.
	pub fn move_left(&mut self) {
		let from = self.cursor;
		self.cursor = self.cursor.saturating_sub(1);
		self.finish_motion(from, false);
	}

	pub fn move_right(&mut self) {
		let from = self.cursor;
		self.cursor = (self.cursor + 1).min(self.insert_bound());
		self.finish_motion(from, false);
	}

	pub fn move_bol(&mut self) {
		let from = self.cursor;
		self.cursor = 0;
		self.finish_motion(from, false);
	}

	pub fn move_eol(&mut self) {
		let from = self.cursor;
		self.cursor = self.normal_bound();
		self.finish_motion(from, true);
	}

	pub fn move_word_forward(&mut self) {
		let from = self.cursor;
		self.cursor = word_forward(&self.value, self.cursor).min(self.normal_bound());
		self.finish_motion(from, false);
	}

	pub fn move_word_back(&mut self) {
		let from = self.cursor;
		self.cursor = word_back(&self.value, self.cursor);
		self.finish_motion(from, false);
	}

	pub fn move_word_end(&mut self) {
		let from = self.cursor;
		self.cursor = word_end(&self.value, self.cursor);
		self.finish_motion(from, true);
	}

	/// Completes a pending `d` operator over the range just travelled by a
	/// motion. `inclusive` motions (`e`, `$`) delete through the
	/// destination character; the rest stop just before it — matching
	/// vim's own inclusive/exclusive motion split.
	fn finish_motion(&mut self, from: usize, inclusive: bool) {
		if !std::mem::take(&mut self.op_pending) {
			return;
		}
		let lo = from.min(self.cursor);
		let mut hi = from.max(self.cursor);
		if inclusive {
			hi = (hi + 1).min(self.value.len());
		}
		self.value.drain(lo..hi);
		self.cursor = lo;
		self.clamp();
	}

	// -- mode switches
	pub fn enter_insert(&mut self) {
		self.op_pending = false;
		self.mode = InputMode::Insert;
	}

	/// `a`/`A`: nudges the cursor one further right than Normal mode
	/// otherwise allows, so typing continues *after* the current character.
	pub fn enter_insert_after(&mut self) {
		self.op_pending = false;
		self.cursor = (self.cursor + 1).min(self.value.len());
		self.mode = InputMode::Insert;
	}

	/// Esc. Insert/Visual/Replace all drop one level to Normal and return
	/// `false`; Normal has nowhere further to drop to, so returns `true` —
	/// the caller closes the prompt instead.
	pub fn escape(&mut self) -> bool {
		self.op_pending = false;
		match self.mode {
			InputMode::Insert => {
				self.mode = InputMode::Normal;
				self.cursor = self.cursor.saturating_sub(1);
				self.clamp();
				false
			}
			InputMode::Visual => {
				self.anchor = None;
				self.mode = InputMode::Normal;
				false
			}
			InputMode::Replace => {
				self.mode = InputMode::Normal;
				false
			}
			InputMode::Normal => true,
		}
	}

	fn insert_bound(&self) -> usize { if self.mode == InputMode::Insert { self.value.len() } else { self.normal_bound() } }

	fn normal_bound(&self) -> usize { self.value.len().saturating_sub(1) }

	fn clamp(&mut self) { self.cursor = self.cursor.min(self.insert_bound()); }
}

fn word_forward(chars: &[char], mut i: usize) -> usize {
	let len = chars.len();
	while i < len && !chars[i].is_whitespace() {
		i += 1;
	}
	while i < len && chars[i].is_whitespace() {
		i += 1;
	}
	i
}

fn word_back(chars: &[char], mut i: usize) -> usize {
	if i == 0 {
		return 0;
	}
	i -= 1;
	while i > 0 && chars[i].is_whitespace() {
		i -= 1;
	}
	while i > 0 && !chars[i - 1].is_whitespace() {
		i -= 1;
	}
	i
}

fn word_end(chars: &[char], mut i: usize) -> usize {
	let len = chars.len();
	if len == 0 {
		return 0;
	}
	i += 1;
	while i < len && chars[i].is_whitespace() {
		i += 1;
	}
	while i + 1 < len && !chars[i + 1].is_whitespace() {
		i += 1;
	}
	i.min(len - 1)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn starts_in_insert_with_the_cursor_at_the_end() {
		let input = Input::new("Rename", "leaf.txt");
		assert!(input.mode == InputMode::Insert);
		assert_eq!(input.cursor, 8);
		assert_eq!(input.value(), "leaf.txt");
	}

	#[test]
	fn escape_drops_through_each_mode_before_closing() {
		let mut input = Input::new("Rename", "ab");
		assert!(!input.escape(), "insert -> normal, stays open");
		assert!(input.mode == InputMode::Normal);
		assert_eq!(input.cursor, 1, "pulled back onto the last character, like vim");
		assert!(input.escape(), "already normal: caller should close");
	}

	#[test]
	fn word_motions_skip_whitespace_runs() {
		let mut input = Input::new("Rename", "foo bar baz");
		input.escape(); // Normal, cursor on 'z' at the end... actually on the last char after clamp
		input.move_bol();

		input.move_word_forward();
		assert_eq!(input.cursor, 4, "start of 'bar'");
		input.move_word_forward();
		assert_eq!(input.cursor, 8, "start of 'baz'");

		input.move_word_back();
		assert_eq!(input.cursor, 4, "back to start of 'bar'");

		input.move_bol();
		input.move_word_end();
		assert_eq!(input.cursor, 2, "end of 'foo'");
	}

	#[test]
	fn dw_deletes_exclusive_of_the_next_word() {
		let mut input = Input::new("Rename", "foo bar");
		input.escape();
		input.move_bol();

		input.op_delete();
		input.move_word_forward(); // dw
		assert_eq!(input.value(), "bar", "'foo ' is gone, 'bar' untouched");
		assert_eq!(input.cursor, 0);
	}

	#[test]
	fn d_dollar_deletes_inclusive_to_eol() {
		let mut input = Input::new("Rename", "foobar");
		input.escape();
		input.move_bol();
		input.move_right();
		input.move_right(); // cursor on the second 'o' (index 2)

		input.op_delete();
		input.move_eol(); // d$
		assert_eq!(input.value(), "fo");
	}

	#[test]
	fn dd_clears_the_whole_line() {
		let mut input = Input::new("Rename", "anything");
		input.escape();
		input.op_delete();
		input.op_delete(); // dd
		assert_eq!(input.value(), "");
		assert_eq!(input.cursor, 0);
	}

	#[test]
	fn a_non_motion_key_cancels_a_pending_operator() {
		let mut input = Input::new("Rename", "abc");
		input.escape();
		input.op_delete();
		input.delete_under(); // "dx" — not a valid vim combo, treated as a plain x

		assert_eq!(input.value(), "ab", "only the 'x' happened, not a compounded delete");
	}

	#[test]
	fn replace_overwrites_one_character_and_returns_to_normal() {
		let mut input = Input::new("Rename", "abc");
		input.escape();
		input.move_bol();

		input.enter_replace();
		assert!(input.mode == InputMode::Replace);
		input.replace_char('X');

		assert_eq!(input.value(), "Xbc");
		assert!(input.mode == InputMode::Normal);
	}

	#[test]
	fn visual_delete_removes_exactly_the_selected_range() {
		let mut input = Input::new("Rename", "abcdef");
		input.escape();
		input.move_bol();
		input.move_right(); // cursor on 'b' (index 1)

		input.toggle_visual();
		input.move_right();
		input.move_right(); // selection now b..=d (indices 1..=3)
		assert_eq!(input.selection(), Some((1, 3)));

		input.delete_visual();
		assert_eq!(input.value(), "aef");
		assert!(input.mode == InputMode::Normal);
		assert_eq!(input.cursor, 1);
	}

	#[test]
	fn append_enters_insert_one_past_the_cursor() {
		let mut input = Input::new("Rename", "abc");
		input.escape();
		input.move_bol();

		input.enter_insert_after(); // vim's `a`
		assert!(input.mode == InputMode::Insert);
		assert_eq!(input.cursor, 1, "typing continues right after 'a', not on top of it");
	}
}
