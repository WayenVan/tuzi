#[derive(Clone, Copy)]
pub struct Visual {
	pub start: usize,
	pub unset: bool,
}

impl Visual {
	pub fn new(start: usize, unset: bool) -> Self { Self { start, unset } }

	pub fn range(&self, cursor: usize) -> (usize, usize) { (self.start.min(cursor), self.start.max(cursor)) }
}
