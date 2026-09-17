use std::time::{Duration, Instant};

/// How urgent a one-off toast is — controls its color and how long it stays
/// on screen before disappearing on its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoticeLevel {
	Info,
	Warn,
	Error,
}

/// A transient toast: an operational error or warning that isn't tied to any
/// one place in the tree (unlike a directory load failure, which stays
/// pinned to its node instead of expiring). No slide animation — it's just
/// visible until `expires_at`, then gone.
pub struct Notice {
	pub level:   NoticeLevel,
	pub message: String,
	expires_at:  Instant,
}

impl Notice {
	pub fn new(level: NoticeLevel, message: impl Into<String>, timeout: Duration) -> Self {
		Self { level, message: message.into(), expires_at: Instant::now() + timeout }
	}

	pub fn expired(&self) -> bool { Instant::now() >= self.expires_at }

	/// How long until this notice should be pruned — used to schedule the
	/// redraw that makes it disappear on time even if nothing else happens.
	pub fn remaining(&self) -> Duration { self.expires_at.saturating_duration_since(Instant::now()) }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_fresh_notice_is_not_expired() {
		let notice = Notice::new(NoticeLevel::Warn, "test", Duration::from_secs(5));
		assert!(!notice.expired());
		assert!(notice.remaining() > Duration::ZERO);
	}

	#[test]
	fn a_more_severe_level_stays_up_longer() {
		let info = Notice::new(NoticeLevel::Info, "x", Duration::from_secs(3));
		let warn = Notice::new(NoticeLevel::Warn, "x", Duration::from_secs(5));
		let error = Notice::new(NoticeLevel::Error, "x", Duration::from_secs(8));
		assert!(info.remaining() < warn.remaining());
		assert!(warn.remaining() < error.remaining());
	}
}
