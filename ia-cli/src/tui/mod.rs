#[cfg(feature = "ai-analyze")]
pub mod ai;
#[cfg(feature = "ai-qa")]
#[allow(dead_code)] // Dashboard implementation — wired up via ia ai qa --dashboard
pub mod ai_qa;
mod app;
pub mod dashboard;
pub mod errors_tab;
pub mod framework;
pub mod help;
pub mod joblog_state;
pub mod log_tab;
pub mod s3_state;
pub mod search;
pub mod tab;
pub mod tasks_tab;
pub mod theme;
mod ui;
pub mod upload_app;
pub mod upload_tab;
pub mod widgets;

pub use app::run_tui;
pub use upload_app::run_upload_batch_tui;
pub use upload_app::run_upload_tui;
