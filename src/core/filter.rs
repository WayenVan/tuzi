use std::ops::Range;

use crate::finder::Finder;

/// Hides tree rows instead of only highlighting them — the same
/// substring/smart-case matching `Finder` uses to jump between matches,
/// wrapped so `Tab.filter`'s API doesn't carry `Finder`'s find-only concept
/// of a search direction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Filter(Finder);

impl Filter {
	pub fn new(query: String) -> Option<Self> { Finder::new(query, false).map(Self) }

	pub fn matches(&self, name: &str) -> bool { self.0.matches(name) }

	pub fn query(&self) -> &str { self.0.query() }

	pub fn ranges(&self, name: &str) -> Vec<Range<usize>> { self.0.ranges(name) }
}
