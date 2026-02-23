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

    /// Create a client without retry middleware.
    ///
    /// Useful when application-level retry handling (e.g., RateLimiter for 429)
    /// needs to see raw HTTP responses without middleware intervention.
    pub fn from_config_no_retry(config: IaConfig) -> Result<Self> {
        let user_agent = build_user_agent(&config);

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_str(&user_agent).unwrap());

        let raw_client = reqwest::Client::builder()
            .default_headers(headers)
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|e| {
                crate::error::IaError::Config(format!("failed to build HTTP client: {e}"))
            })?;

        let http = ClientBuilder::new(raw_client).build();

        Ok(Self {
            http,
            config,
            user_agent,
        })
    }

    pub async fn get_item(&self, identifier: &str) -> Result<crate::types::ItemMetadata> {
        crate::metadata::get(self, identifier).await
    }

    pub async fn item_exists(&self, identifier: &str) -> Result<bool> {
        crate::metadata::exists(self, identifier).await
    }

    pub fn search<'a>(
        &'a self,
        query: &str,
        opts: &crate::search::SearchOpts,
    ) -> std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<crate::search::SearchResult>> + Send + 'a>,
    > {
        crate::search::scrape(self, query, opts)
    }

    pub async fn search_count(&self, query: &str) -> Result<u64> {
        crate::search::num_found(self, query).await
    }

    /// Get S3 credentials, or error if not configured.
    /// Called on first write attempt — read operations stay unauthenticated.
    pub fn require_auth(&self) -> Result<(&str, &str)> {
        match (&self.config.s3_access, &self.config.s3_secret) {
            (Some(a), Some(s)) => Ok((a.as_str(), s.as_str())),
            _ => Err(crate::error::IaError::Auth(
                "S3 credentials required. Run `ia configure` or set \
                 IA_ACCESS_KEY_ID/IA_SECRET_ACCESS_KEY environment variables."
                    .into(),
            )),
        }
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

    #[test]
    fn require_auth_returns_credentials_when_present() {
        let mut config = IaConfig::default();
        config.s3_access = Some("test_access".to_string());
        config.s3_secret = Some("test_secret".to_string());
        let client = IaClient::from_config(config).unwrap();
        let (access, secret) = client.require_auth().unwrap();
        assert_eq!(access, "test_access");
        assert_eq!(secret, "test_secret");
    }

    #[test]
    fn require_auth_errors_when_no_credentials() {
        let client = IaClient::from_config(IaConfig::default()).unwrap();
        let result = client.require_auth();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::error::IaError::Auth(_)
        ));
    }

    #[test]
    fn require_auth_errors_when_partial_credentials() {
        let mut config = IaConfig::default();
        config.s3_access = Some("access_only".to_string());
        // s3_secret is None
        let client = IaClient::from_config(config).unwrap();
        assert!(client.require_auth().is_err());
    }
}
