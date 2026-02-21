use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};

use crate::config::IaConfig;
use crate::error::Result;
use crate::user_agent::build_user_agent;

/// The central HTTP client for interacting with the Internet Archive.
///
/// Wraps a `reqwest::Client` with IA-specific configuration:
/// - Connection pooling and keep-alive (unlike Python's Connection: close)
/// - Automatic retry with exponential backoff for 429/5xx
/// - Retry-After header respect
/// - Proper User-Agent identification
#[derive(Clone)]
pub struct IaClient {
    http: ClientWithMiddleware,
    config: IaConfig,
    user_agent: String,
}

impl IaClient {
    /// Create a new client with auto-discovered config.
    pub fn new() -> Result<Self> {
        let config = IaConfig::load()?;
        Self::from_config(config)
    }

    /// Create a new client with the provided config.
    pub fn from_config(config: IaConfig) -> Result<Self> {
        let user_agent = build_user_agent(&config);

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_str(&user_agent).unwrap());

        let raw_client = reqwest::Client::builder()
            .default_headers(headers)
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|e| crate::error::IaError::Config(format!("failed to build HTTP client: {e}")))?;

        let retry_policy = ExponentialBackoff::builder()
            .retry_bounds(
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(60),
            )
            .build_with_max_retries(3);

        let http = ClientBuilder::new(raw_client)
            .with(RetryTransientMiddleware::new_with_policy(retry_policy))
            .build();

        Ok(Self {
            http,
            config,
            user_agent,
        })
    }

    /// The configured host (default: "archive.org").
    pub fn host(&self) -> &str {
        &self.config.general.host
    }

    /// The protocol ("https" or "http").
    pub fn protocol(&self) -> &str {
        self.config.protocol()
    }

    /// Build a full URL from a path on the main archive.org host.
    pub fn url(&self, path: &str) -> String {
        format!("{}://{}{}", self.protocol(), self.host(), path)
    }

    /// The User-Agent string being sent with requests.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// The underlying HTTP client (for operation modules).
    pub fn http(&self) -> &ClientWithMiddleware {
        &self.http
    }

    /// The current configuration.
    pub fn config(&self) -> &IaConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_from_default_config() {
        let client = IaClient::from_config(IaConfig::default()).unwrap();
        assert_eq!(client.host(), "archive.org");
        assert_eq!(client.protocol(), "https");
        assert!(client.user_agent().starts_with("ia/"));
    }

    #[test]
    fn client_url_construction() {
        let client = IaClient::from_config(IaConfig::default()).unwrap();
        assert_eq!(
            client.url("/metadata/nasa"),
            "https://archive.org/metadata/nasa"
        );
    }

    #[test]
    fn client_custom_host() {
        let mut config = IaConfig::default();
        config.general.host = "test.archive.org".to_string();
        let client = IaClient::from_config(config).unwrap();
        assert_eq!(client.host(), "test.archive.org");
        assert_eq!(
            client.url("/metadata/test"),
            "https://test.archive.org/metadata/test"
        );
    }

    #[test]
    fn client_insecure_mode() {
        let mut config = IaConfig::default();
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();
        assert_eq!(client.protocol(), "http");
        assert!(client.url("/test").starts_with("http://"));
    }

    #[test]
    fn client_is_clone() {
        let client = IaClient::from_config(IaConfig::default()).unwrap();
        let _clone = client.clone(); // Should compile — reqwest::Client is Arc-based
    }
}
