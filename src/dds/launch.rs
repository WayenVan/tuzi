use super::PeerId;

pub const MAX_LAUNCH_TOKEN_BYTES: usize = 256;

/// Correlates a Tuzi process with the DDS controller that launched it.
/// The controller owns the token; Tuzi retains ownership of its peer ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DdsLaunch {
	pub parent: PeerId,
	pub token:  String,
}

impl DdsLaunch {
	pub fn new(parent: PeerId, token: String) -> Result<Self, String> {
		if parent == 0 {
			return Err("DDS parent peer ID must not be 0".into());
		}
		if token.is_empty() {
			return Err("DDS launch token must not be empty".into());
		}
		if token.len() > MAX_LAUNCH_TOKEN_BYTES {
			return Err(format!("DDS launch token must not exceed {MAX_LAUNCH_TOKEN_BYTES} bytes"));
		}
		Ok(Self { parent, token })
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn validates_parent_and_token() {
		assert!(DdsLaunch::new(0, "token".into()).is_err());
		assert!(DdsLaunch::new(1, String::new()).is_err());
		assert!(DdsLaunch::new(1, "x".repeat(MAX_LAUNCH_TOKEN_BYTES + 1)).is_err());
		assert_eq!(DdsLaunch::new(7, "token".into()).unwrap(), DdsLaunch { parent: 7, token: "token".into() });
	}
}
