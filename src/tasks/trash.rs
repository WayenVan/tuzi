use std::path::Path;

pub(super) trait TrashBackend: Send + Sync {
	fn delete(&self, path: &Path) -> Result<(), String>;
}

pub(super) struct SystemTrash;

impl TrashBackend for SystemTrash {
	fn delete(&self, path: &Path) -> Result<(), String> { trash_delete(path).map_err(|error| error.to_string()) }
}

/// Avoid Finder/osascript automation prompts for background tasks.
#[cfg(target_os = "macos")]
fn trash_delete(path: &Path) -> Result<(), trash::Error> {
	use trash::{TrashContext, macos::{DeleteMethod, TrashContextExtMacos}};
	let mut context = TrashContext::new();
	context.set_delete_method(DeleteMethod::NsFileManager);
	context.delete(path)
}

#[cfg(not(target_os = "macos"))]
fn trash_delete(path: &Path) -> Result<(), trash::Error> { trash::delete(path) }
