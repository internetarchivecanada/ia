pub mod ai;
pub mod auth;
pub mod client;
pub mod config;
pub mod disk_pool;
pub mod download;
pub mod error;
pub mod files;
pub mod joblog;
pub mod metadata;
pub mod rate_limit;
pub mod search;
pub mod spreadsheet;
pub mod types;
pub mod update;
pub mod upload;
pub mod user_agent;

pub use client::IaClient;
pub use config::IaConfig;
pub use error::{IaError, JsonError, JsonErrorBody, Result, write_json_error};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
    }
}
