pub mod ai;
mod app;
pub mod framework;
mod ui;
#[allow(dead_code)] // Consumed by upload_ui (Task 7) and CLI wiring (Task 8)
pub mod upload_app;
#[allow(dead_code)] // Stub — Task 7 implements the real rendering
mod upload_ui;
pub mod widgets;

pub use app::run_tui;
