pub mod client;
pub mod config;
pub mod error;
pub mod user_agent;

pub use client::IaClient;
pub use config::IaConfig;
pub use error::{IaError, Result};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert_eq!(super::version(), "0.1.0");
    }
}
