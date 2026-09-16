mod actor;
mod app;
mod config;
mod core;
mod event;
mod fs;
mod runner;
mod scheduler;
mod tui;
mod watcher;

#[tokio::main]
async fn main() -> std::io::Result<()> { app::App::serve().await }
