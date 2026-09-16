#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusMode {
	Normal,
	Select,
	Unset,
}

impl StatusMode {
	pub fn label(self) -> &'static str {
		match self {
			Self::Normal => "NORM",
			Self::Select => "SEL",
			Self::Unset => "UNS",
		}
	}
}

pub struct StatusLine {
	pub mode:        StatusMode,
	pub name:        String,
	pub size:        String,
	pub permissions: String,
	pub error:       Option<String>,
}

impl StatusLine {
	pub fn empty(mode: StatusMode) -> Self {
		Self { mode, name: String::new(), size: String::new(), permissions: String::new(), error: None }
	}
}
