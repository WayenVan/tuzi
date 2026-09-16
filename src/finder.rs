use std::ops::Range;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Finder {
	query:          String,
	query_chars:    Vec<char>,
	case_sensitive: bool,
	previous:       bool,
}

impl Finder {
	pub fn new(query: String, previous: bool) -> Option<Self> {
		if query.is_empty() {
			return None;
		}
		let case_sensitive = query.chars().any(char::is_uppercase);
		let query_chars = query.chars().collect();
		Some(Self { query, query_chars, case_sensitive, previous })
	}

	pub fn previous(&self) -> bool { self.previous }

	pub fn matches(&self, name: &str) -> bool { !self.ranges(name).is_empty() }

	/// Non-overlapping character ranges, so callers can style Unicode names
	/// without converting byte offsets back into terminal cells.
	pub fn ranges(&self, name: &str) -> Vec<Range<usize>> {
		let name: Vec<char> = name.chars().collect();
		let width = self.query_chars.len();
		if width == 0 || width > name.len() {
			return Vec::new();
		}

		let mut ranges = Vec::new();
		let mut start = 0;
		while start + width <= name.len() {
			let matched = name[start..start + width]
				.iter()
				.zip(&self.query_chars)
				.all(|(left, right)| self.char_eq(*left, *right));
			if matched {
				ranges.push(start..start + width);
				start += width;
			} else {
				start += 1;
			}
		}
		ranges
	}

	fn char_eq(&self, left: char, right: char) -> bool {
		if self.case_sensitive { left == right } else { left.to_lowercase().eq(right.to_lowercase()) }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn smart_case_and_ranges() {
		let finder = Finder::new("rs".into(), false).unwrap();
		assert!(finder.matches("MAIN.RS"));
		assert_eq!(finder.ranges("rs-rs"), vec![0..2, 3..5]);

		let finder = Finder::new("Rs".into(), false).unwrap();
		assert!(finder.matches("main.Rs"));
		assert!(!finder.matches("main.rs"));
	}
}
