mod action;
mod actor;
mod app;
mod column_mode;
mod config;
mod core;
mod event;
mod finder;
mod fs;
mod icon;
mod keymap;
mod preview;
mod runner;
mod scheduler;
mod status;
mod tui;
mod watcher;

#[tokio::main]
async fn main() -> std::io::Result<()> { app::App::serve().await }
