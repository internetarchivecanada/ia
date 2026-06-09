use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};

use crate::config::IaConfig;
use crate::error::Result;
use crate::retry::{LoggingRetryStrategy, RetryStats, TimingMiddleware};
use crate::user_agent::build_user_agent;

/// Maximum time to establish a TCP connection (incl. TLS handshake).
pub(crate) const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Maximum idle time between response reads on an established connection.
///
/// Applies per read operation and resets after each successful read, so
/// long streaming *downloads* are fine as long as bytes keep flowing.
///
/// CAUTION: the clock is NOT reset by request-body writes — the server is
/// legitimately silent while a large body uploads, so this must never be
/// set on a transport that sends streaming/multipart upload bodies (they
/// would abort once the send exceeds the timeout). Pass `None` for those.
pub(crate) const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Apply connect/read timeouts to a client builder.
///
/// Shared by all transport clients so a stalled connection aborts with a
/// timeout error instead of hanging its tokio task indefinitely. `read`
/// must be `None` for transports that send large request bodies (see
/// [`READ_TIMEOUT`]).
pub(crate) fn configure_transport(
    builder: reqwest::ClientBuilder,
    connect: std::time::Duration,
    read: Option<std::time::Duration>,
) -> reqwest::ClientBuilder {
    let builder = builder.connect_timeout(connect);
    match read {
        Some(read) => builder.read_timeout(read),
        None => builder,
    }
}

/// The underlying reqwest transports, split by timeout requirements.
struct Transports {
    /// General API transport: archive.org-only redirects, read timeout.
    api: reqwest::Client,
    /// Upload transport: archive.org-only redirects, NO read timeout —
    /// safe for requests that spend a long time sending a body.
    upload: reqwest::Client,
    /// Download-auth transport: redirects disabled, read timeout.
    no_redirect: reqwest::Client,
    user_agent: String,
}

/// Error returned when a redirect targets a non-archive.org domain.
#[derive(Debug)]
struct RedirectBlockedError(reqwest::Url);

impl std::fmt::Display for RedirectBlockedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "redirect to non-archive.org domain blocked: {}", self.0)
    }
}

impl std::error::Error for RedirectBlockedError {}

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
    /// Retry-middleware client over the upload transport (no read
    /// timeout). For upload-path requests with cloneable bodies:
    /// multipart part PUTs and S3 control calls (initiate/complete/
    /// abort/list), whose body sends or server-side processing can
    /// legitimately exceed the API read timeout.
    upload_http: ClientWithMiddleware,
    /// Raw reqwest client without retry middleware (upload transport,
    /// no read timeout).
    ///
    /// Used by operations that manage their own retry loops (e.g., upload)
    /// and need to send streaming (non-cloneable) request bodies.
    /// The retry middleware requires cloneable requests, which conflicts
    /// with `Body::wrap_stream()` and `Body::from(tokio::fs::File)`.
    raw_http: reqwest::Client,
    /// Client with redirects disabled, for requests that need to preserve
    /// the `Authorization` header across redirects (equivalent to curl's
    /// `--location-trusted`). archive.org redirects `/download/` requests
    /// to data-node hosts like `ia600XXX.us.archive.org`, and reqwest
    /// strips `Authorization` on redirects by default.
    no_redirect_http: reqwest::Client,
    config: IaConfig,
    user_agent: String,
    retry_stats: Arc<RetryStats>,
}

impl IaClient {
    /// Create a new client with auto-discovered config.
    pub fn new() -> Result<Self> {
        let config = IaConfig::load()?;
        Self::from_config(config)
    }

    /// Only follow redirects to *.archive.org domains.
    /// Blocks SSRF and prevents credential leakage if auth is added later.
    fn archive_redirect_policy() -> reqwest::redirect::Policy {
        reqwest::redirect::Policy::custom(|attempt| {
            if let Some(host) = attempt.url().host_str() {
                if host == "archive.org" || host.ends_with(".archive.org") {
                    return attempt.follow();
                }
            }
            let target_url = attempt.url().clone();
            attempt.error(RedirectBlockedError(target_url))
        })
    }

    /// Shared builder settings for all transports.
    ///
    /// HTTP/2 flow-control windows default to 65,535 bytes in hyper (RFC
    /// minimum). At ~40 ms RTT that caps per-stream throughput at ~1.6
    /// MB/s — the actual cause of "slow downloads on fiber." Advertise
    /// large receive windows (16 MB stream, 64 MB connection) and enable
    /// BDP-adaptive windowing so the window grows with real throughput.
    /// TCP_NODELAY eliminates Nagle delays for small control frames.
    fn base_builder(headers: HeaderMap) -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            .default_headers(headers)
            .pool_max_idle_per_host(10)
            .http2_adaptive_window(true)
            .http2_initial_stream_window_size(16 * 1024 * 1024)
            .http2_initial_connection_window_size(64 * 1024 * 1024)
            .tcp_nodelay(true)
    }

    /// Build the underlying reqwest transports with shared settings.
    ///
    /// `read_timeout` is applied only to the `api` and `no_redirect`
    /// transports (request bodies are small; response bodies reset the
    /// timer per chunk). The `upload` transport gets the connect timeout
    /// only: reqwest's read-timeout clock is not reset by request-body
    /// writes, so it would abort any upload whose body takes longer than
    /// the timeout to send.
    fn build_transports(
        config: &IaConfig,
        connect_timeout: std::time::Duration,
        read_timeout: std::time::Duration,
    ) -> Result<Transports> {
        let user_agent = build_user_agent(config);

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_str(&user_agent).unwrap());

        let api = configure_transport(
            Self::base_builder(headers.clone()),
            connect_timeout,
            Some(read_timeout),
        )
        .redirect(Self::archive_redirect_policy())
        .build()
        .map_err(|e| crate::error::IaError::Config(format!("failed to build HTTP client: {e}")))?;

        // Upload transport: NO read timeout (see above).
        let upload =
            configure_transport(Self::base_builder(headers.clone()), connect_timeout, None)
                .redirect(Self::archive_redirect_policy())
                .build()
                .map_err(|e| {
                    crate::error::IaError::Config(format!(
                        "failed to build upload HTTP client: {e}"
                    ))
                })?;

        // No-redirect client for download auth: archive.org redirects
        // /download/ to data nodes, and reqwest strips Authorization on
        // redirect. We handle redirects manually to preserve auth headers
        // (equivalent to curl --location-trusted).
        let no_redirect = configure_transport(
            Self::base_builder(headers),
            connect_timeout,
            Some(read_timeout),
        )
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| {
            crate::error::IaError::Config(format!("failed to build no-redirect client: {e}"))
        })?;

        Ok(Transports {
            api,
            upload,
            no_redirect,
            user_agent,
        })
    }

    /// Create a new client with the provided config.
    pub fn from_config(config: IaConfig) -> Result<Self> {
        Self::from_config_with_verbosity(config, 0)
    }

    /// Create a new client with the provided config and verbosity level.
    ///
    /// `verbosity` controls diagnostic output detail (0 = dedup warnings,
    /// 1+ = individual events). See [`RetryStats`] for details.
    pub fn from_config_with_verbosity(config: IaConfig, verbosity: u8) -> Result<Self> {
        let transports = Self::build_transports(&config, CONNECT_TIMEOUT, READ_TIMEOUT)?;

        let retry_policy = ExponentialBackoff::builder()
            .retry_bounds(
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(60),
            )
            .build_with_max_retries(3);

        let stats = Arc::new(RetryStats::new(verbosity));

        // Clone before moving into middleware — reqwest::Client is Arc-based, cheap to clone.
        let raw_http = transports.upload.clone();

        let http = ClientBuilder::new(transports.api)
            .with(TimingMiddleware::new(stats.clone()))
            .with(RetryTransientMiddleware::new_with_policy_and_strategy(
                retry_policy,
                LoggingRetryStrategy::new(stats.clone()),
            ))
            .build();

        // Same middleware stack over the upload transport, for upload
        // requests with cloneable bodies (multipart parts, S3 control
        // calls) that want retry but must not have a read timeout.
        let upload_http = ClientBuilder::new(transports.upload)
            .with(TimingMiddleware::new(stats.clone()))
            .with(RetryTransientMiddleware::new_with_policy_and_strategy(
                retry_policy,
                LoggingRetryStrategy::new(stats.clone()),
            ))
            .build();

        Ok(Self {
            http,
            upload_http,
            raw_http,
            no_redirect_http: transports.no_redirect,
            config,
            user_agent: transports.user_agent,
            retry_stats: stats,
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

    /// The base URL for the Full-Text Search API.
    ///
    /// Uses `fts_host` from config if set, otherwise defaults to
    /// `be-api.us.archive.org`. This makes FTS testable with wiremock.
    pub fn fts_base_url(&self) -> String {
        let host = self
            .config
            .general
            .fts_host
            .as_deref()
            .unwrap_or("be-api.us.archive.org");
        format!("{}://{host}/ia-pub-fts-api", self.protocol())
    }

    /// The User-Agent string being sent with requests.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// The underlying HTTP client (for operation modules).
    pub fn http(&self) -> &ClientWithMiddleware {
        &self.http
    }

    /// Retry-middleware HTTP client over the upload transport.
    ///
    /// Like [`Self::http`] but without a read timeout. Use for upload-path
    /// requests with cloneable bodies — multipart part PUTs and S3 control
    /// calls — where the body send or server-side processing (e.g.
    /// assembling a completed multipart upload) can stay silent longer
    /// than the API read timeout.
    pub(crate) fn upload_http(&self) -> &ClientWithMiddleware {
        &self.upload_http
    }

    /// Raw HTTP client without retry middleware (upload transport).
    ///
    /// Use this for requests with streaming (non-cloneable) bodies, such as
    /// file uploads. The retry middleware requires `Request::try_clone()` to
    /// succeed, which fails for `Body::wrap_stream()` and
    /// `Body::from(tokio::fs::File)`. Operations using this client must
    /// implement their own retry logic.
    pub(crate) fn raw_http(&self) -> &reqwest::Client {
        &self.raw_http
    }

    /// HTTP client with redirects disabled, for manual redirect handling.
    ///
    /// Use this for requests that need to preserve the `Authorization`
    /// header across redirects (archive.org redirects `/download/` to
    /// data-node hosts, and reqwest strips auth headers on redirect).
    /// Equivalent to curl's `--location-trusted`.
    pub fn no_redirect_http(&self) -> &reqwest::Client {
        &self.no_redirect_http
    }

    /// The current configuration.
    pub fn config(&self) -> &IaConfig {
        &self.config
    }

    /// Shared HTTP diagnostic counters.
    pub fn retry_stats(&self) -> &RetryStats {
        &self.retry_stats
    }

    /// Create a client without retry middleware.
    ///
    /// Useful when application-level retry handling (e.g., RateLimiter for 429)
    /// needs to see raw HTTP responses without middleware intervention.
    #[doc(hidden)]
    pub fn from_config_no_retry(config: IaConfig) -> Result<Self> {
        let transports = Self::build_transports(&config, CONNECT_TIMEOUT, READ_TIMEOUT)?;
        // Stats are required by the struct but will always be zeros — no
        // TimingMiddleware or LoggingRetryStrategy is wired in this path.
        let stats = Arc::new(RetryStats::new(0));
        let raw_http = transports.upload.clone();
        let http = ClientBuilder::new(transports.api).build();
        let upload_http = ClientBuilder::new(transports.upload).build();

        Ok(Self {
            http,
            upload_http,
            raw_http,
            no_redirect_http: transports.no_redirect,
            config,
            user_agent: transports.user_agent,
            retry_stats: stats,
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
        crate::search::num_found(self, query, &[]).await
    }

    /// Get S3 credentials, or error if not configured.
    /// Called on first write attempt — read operations stay unauthenticated.
    pub fn require_auth(&self) -> Result<(&str, &str)> {
        match (&self.config.s3_access, &self.config.s3_secret) {
            (Some(a), Some(s)) => Ok((a.as_str(), s.as_str())),
            _ => Err(crate::error::IaError::Auth(
                "S3 credentials required. Run `ia config login` or set \
                 IA_ACCESS_KEY_ID/IA_SECRET_ACCESS_KEY environment variables."
                    .into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_timeout_aborts_stalled_response() {
        // A server that accepts connections but never responds. Without a
        // read timeout the request would hang forever.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                // Hold the connection open without writing a response.
                tokio::spawn(async move {
                    let _stream = stream;
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                });
            }
        });

        let client = configure_transport(
            reqwest::Client::builder(),
            std::time::Duration::from_secs(5),
            Some(std::time::Duration::from_millis(200)),
        )
        .build()
        .unwrap();

        let start = std::time::Instant::now();
        let result = client.get(format!("http://{addr}/")).send().await;
        let err = result.expect_err("stalled response must time out");
        assert!(err.is_timeout(), "expected timeout error, got: {err}");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "request should abort at the read timeout, not hang"
        );
    }

    /// Spawn a local server that reads a full 50-byte request body and only
    /// then responds 200. Returns its address.
    async fn slow_body_sink() -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let mut total = Vec::new();
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                total.extend_from_slice(&buf[..n]);
                // Headers + full 50-byte body received (5 chunks x 10 bytes).
                let body_start = total
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4);
                if body_start.is_some_and(|s| total.len() - s >= 50) {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        addr
    }

    /// A request body of 5 x 10-byte chunks, 150ms apart (~750ms total).
    fn slow_body_stream() -> impl futures::Stream<Item = std::io::Result<Vec<u8>>> {
        futures::stream::unfold(0u32, |i| async move {
            if i >= 5 {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            Some((Ok(vec![b'x'; 10]), i + 1))
        })
    }

    /// reqwest's read_timeout clock is NOT reset by request-body writes —
    /// the server is legitimately silent while a body uploads, so a
    /// transport used for uploads must not have a read timeout at all.
    /// The upload transport must allow a body that takes longer to send
    /// than the API transport's read timeout.
    #[tokio::test]
    async fn upload_transport_allows_body_send_longer_than_read_timeout() {
        let addr = slow_body_sink().await;

        // Build transports with a read timeout far shorter than the
        // ~750ms body send. Only the API/download transports get it.
        let transports = IaClient::build_transports(
            &IaConfig::default(),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(300),
        )
        .unwrap();

        let start = std::time::Instant::now();
        let result = transports
            .upload
            .put(format!("http://{addr}/"))
            .header("content-length", "50")
            .body(reqwest::Body::wrap_stream(slow_body_stream()))
            .send()
            .await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= std::time::Duration::from_millis(700),
            "body should have streamed slowly, took {elapsed:?}"
        );
        let resp = result.unwrap_or_else(|e| {
            panic!("upload taking longer than the API read timeout must succeed, got: {e}")
        });
        assert_eq!(resp.status(), 200);
    }

    /// The API transport, by contrast, must abort a slow body send at its
    /// read timeout rather than hang — documents why upload requests must
    /// never go through it.
    #[tokio::test]
    async fn api_transport_read_timeout_fires_during_slow_body_send() {
        let addr = slow_body_sink().await;

        let transports = IaClient::build_transports(
            &IaConfig::default(),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(300),
        )
        .unwrap();

        let result = transports
            .api
            .put(format!("http://{addr}/"))
            .header("content-length", "50")
            .body(reqwest::Body::wrap_stream(slow_body_stream()))
            .send()
            .await;

        let err = result.expect_err("slow body send through API transport should time out");
        assert!(err.is_timeout(), "expected timeout error, got: {err}");
    }

    #[test]
    fn timeout_constants_are_sane() {
        // Connect should fail fast; read timeout is idle-based so it must
        // be generous enough for slow servers but bounded.
        assert!(CONNECT_TIMEOUT <= std::time::Duration::from_secs(60));
        assert!(READ_TIMEOUT >= std::time::Duration::from_secs(30));
    }

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
    fn client_exposes_retry_stats() {
        let client = IaClient::from_config(IaConfig::default()).unwrap();
        let stats = client.retry_stats();
        assert_eq!(stats.summary().requests_total, 0);
        assert!(!stats.had_retries());
    }

    #[test]
    fn client_with_verbosity() {
        let client = IaClient::from_config_with_verbosity(IaConfig::default(), 2).unwrap();
        assert_eq!(client.retry_stats().summary().requests_total, 0);
    }

    #[test]
    fn require_auth_errors_when_partial_credentials() {
        let mut config = IaConfig::default();
        config.s3_access = Some("access_only".to_string());
        // s3_secret is None
        let client = IaClient::from_config(config).unwrap();
        assert!(client.require_auth().is_err());
    }

    // -- Redirect policy tests --

    fn mock_config(server_uri: &str) -> IaConfig {
        let mut config = IaConfig::default();
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config
    }

    #[tokio::test]
    async fn redirect_to_non_archive_org_is_blocked() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Return a 302 redirect to a localhost URL.
        // The wiremock host is 127.0.0.1 — NOT archive.org.
        // So the redirect policy should BLOCK this redirect.
        Mock::given(method("GET"))
            .and(path("/step1"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/step2", mock_server.uri())),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/step2"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();

        let result = client
            .http()
            .get(format!("{}/step1", mock_server.uri()))
            .send()
            .await;
        // Should fail because 127.0.0.1 is not *.archive.org
        assert!(
            result.is_err(),
            "redirect to non-archive.org should be blocked"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("redirect") || err_msg.contains("non-archive.org"),
            "error should mention redirect blocking: {err_msg}"
        );
    }

    #[tokio::test]
    async fn non_redirect_request_succeeds() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Verify that normal (non-redirect) requests still work fine.
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();

        let resp = client
            .http()
            .get(format!("{}/metadata/test", mock_server.uri()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }
}
