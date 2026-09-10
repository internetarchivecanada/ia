use crate::client::IaClient;
use crate::error::{IaError, Result};
use crate::retry::is_retryable_body_error;
use crate::types::ItemMetadata;

/// Max attempts for the body-decode retry wrapper around `/metadata/{id}`.
///
/// The response is tiny and idempotent, so plain retry is safe. The retry
/// middleware (`reqwest-retry`) only inspects response status; body-decode
/// errors happen *after* the middleware has released the response and so
/// bypass it entirely. This wrapper covers that gap.
const METADATA_MAX_ATTEMPTS: usize = 3;

/// Fetch full metadata for an item.
///
/// Sends S3 auth headers when credentials are configured, which is
/// required for accessing private/dark items.
///
/// Retries up to three times on transient body-decode failures (truncated
/// body, unexpected EOF, malformed JSON — sometimes returned with a 200
/// status by upstream proxies) with exponential backoff.
pub async fn get(client: &IaClient, identifier: &str) -> Result<ItemMetadata> {
    get_with_params(client, identifier, &[]).await
}

/// Fetch full metadata for an item, appending extra query parameters.
///
/// Sends S3 auth headers when credentials are configured, which is
/// required for accessing private/dark items. Extra parameters are appended
/// to the query string — e.g. `dark_ok=1` is required (in addition to auth)
/// for the metadata API to return the metadata of a dark item.
///
/// Retries up to three times on transient body-decode failures (truncated
/// body, unexpected EOF, malformed JSON — sometimes returned with a 200
/// status by upstream proxies) with exponential backoff.
pub async fn get_with_params(
    client: &IaClient,
    identifier: &str,
    params: &[(String, String)],
) -> Result<ItemMetadata> {
    let url = client.url(&format!("/metadata/{identifier}"));

    let mut attempt: usize = 0;
    loop {
        attempt += 1;
        match get_once(client, identifier, &url, params).await {
            Ok(item) => return Ok(item),
            Err(e) if attempt < METADATA_MAX_ATTEMPTS && is_retryable_network_error(&e) => {
                let backoff = backoff_for_attempt(attempt);
                tracing::warn!(
                    identifier,
                    attempt,
                    max = METADATA_MAX_ATTEMPTS,
                    backoff_ms = backoff.as_millis() as u64,
                    error = %crate::error::format_error_chain(&e),
                    "metadata fetch body-decode error, retrying",
                );
                tokio::time::sleep(backoff).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn get_once(
    client: &IaClient,
    identifier: &str,
    url: &str,
    params: &[(String, String)],
) -> Result<ItemMetadata> {
    let mut req = client.http().get(url);
    if !params.is_empty() {
        req = req.query(params);
    }
    if let Some(auth) = crate::auth::s3_auth_value(client.config()) {
        req = req.header("Authorization", auth);
    }
    let response = req.send().await?;

    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(IaError::NotFound(identifier.to_string()));
    }
    if status.as_u16() == 429 {
        let retry_after = crate::retry::extract_retry_after(response.headers());
        return Err(IaError::RateLimited {
            retry_after: retry_after.unwrap_or(30),
        });
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status: status.as_u16(),
            message: body,
        });
    }

    let item: ItemMetadata = response
        .json()
        .await
        .map_err(reqwest_middleware::Error::from)?;

    Ok(item)
}

fn is_retryable_network_error(err: &IaError) -> bool {
    match err {
        IaError::Network(reqwest_middleware::Error::Reqwest(e)) => is_retryable_body_error(e),
        _ => false,
    }
}

fn backoff_for_attempt(attempt: usize) -> std::time::Duration {
    // attempt=1 → 500ms, attempt=2 → 1.5s, attempt=3 → 4.5s.
    let base = std::time::Duration::from_millis(500);
    base * 3u32.saturating_pow(attempt.saturating_sub(1) as u32)
}

/// Check if an item exists.
pub async fn exists(client: &IaClient, identifier: &str) -> Result<bool> {
    match get(client, identifier).await {
        Ok(_) => Ok(true),
        Err(IaError::NotFound(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        // Strip protocol prefix for host
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config
    }

    #[tokio::test]
    async fn get_item_metadata() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {
                    "identifier": "test-item",
                    "title": "Test Item",
                    "mediatype": "texts"
                },
                "files": [
                    {"name": "test.pdf", "size": "1000", "source": "original", "md5": "abc123"}
                ],
                "server": "ia000000.us.archive.org"
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let item = get(&client, "test-item").await.unwrap();

        assert_eq!(
            item.metadata.identifier.as_ref().map(|v| v.first()),
            Some("test-item")
        );
        assert_eq!(item.files.len(), 1);
        assert_eq!(item.files[0].name, "test.pdf");
        assert_eq!(item.files[0].size, Some(1000));
    }

    #[tokio::test]
    async fn get_with_params_sends_query_params() {
        use wiremock::matchers::query_param;

        let mock_server = MockServer::start().await;

        // The mock only matches when dark_ok=1 is present on the query string,
        // so a passing test proves the param was actually sent.
        Mock::given(method("GET"))
            .and(path("/metadata/dark-item"))
            .and(query_param("dark_ok", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {
                    "identifier": "dark-item",
                    "title": "Dark Item",
                    "mediatype": "audio"
                },
                "is_dark": true
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let params = vec![("dark_ok".to_string(), "1".to_string())];
        let item = get_with_params(&client, "dark-item", &params)
            .await
            .unwrap();

        assert_eq!(
            item.metadata.identifier.as_ref().map(|v| v.first()),
            Some("dark-item")
        );
        assert!(item.is_dark);
    }

    #[tokio::test]
    async fn get_item_not_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/nonexistent"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get(&client, "nonexistent").await;

        assert!(matches!(result, Err(IaError::NotFound(_))));
    }

    #[tokio::test]
    async fn get_item_rate_limited() {
        let mock_server = MockServer::start().await;

        // 429 passes through middleware without retry.
        Mock::given(method("GET"))
            .and(path("/metadata/busy-item"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get(&client, "busy-item").await;

        match result {
            Err(IaError::RateLimited { retry_after }) => {
                assert_eq!(retry_after, 60);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_item_rate_limited_no_header() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/busy-item-2"))
            .respond_with(ResponseTemplate::new(429))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get(&client, "busy-item-2").await;

        match result {
            Err(IaError::RateLimited { retry_after }) => {
                assert_eq!(retry_after, 30); // default
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_retries_on_body_decode_error_and_succeeds() {
        // archive.org occasionally returns 200 with a truncated / non-JSON
        // body. Prior to adding retries here, a single bad response caused
        // the whole item to be dropped — bypassing the middleware retry
        // (which only inspects response status).
        let mock_server = MockServer::start().await;

        // First call: 200 OK, but body is not valid JSON.
        Mock::given(method("GET"))
            .and(path("/metadata/flaky-item"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string("<!doctype html><html>whoops</html>"),
            )
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        // Subsequent calls: valid JSON.
        Mock::given(method("GET"))
            .and(path("/metadata/flaky-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "flaky-item", "title": "Flaky"},
                "files": []
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let item = get(&client, "flaky-item").await.unwrap();
        assert_eq!(
            item.metadata.identifier.as_ref().map(|v| v.first()),
            Some("flaky-item"),
        );
    }

    #[tokio::test]
    async fn get_gives_up_after_retry_budget() {
        // Sustained decode failures — retry should bound the attempts and
        // return the error instead of hanging forever.
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/broken-item"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string("not json"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get(&client, "broken-item").await;
        assert!(
            matches!(result, Err(IaError::Network(_))),
            "expected Network error after retry budget, got {result:?}",
        );
    }

    #[tokio::test]
    async fn exists_returns_true_for_existing_item() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/nasa"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "nasa"},
                "files": []
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        assert!(exists(&client, "nasa").await.unwrap());
    }

    #[tokio::test]
    async fn exists_returns_false_for_missing_item() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/nope"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        assert!(!exists(&client, "nope").await.unwrap());
    }
}
