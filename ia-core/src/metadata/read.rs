use crate::client::IaClient;
use crate::error::{IaError, Result};
use crate::retry::is_retryable_body_error;
use crate::types::ItemMetadata;

/// Max attempts for the retry wrapper around `/metadata/{id}`.
///
/// The response is tiny and idempotent, so plain retry is safe. The retry
/// middleware (`reqwest-retry`) only inspects response status; body-decode
/// errors happen *after* the middleware has released the response and so
/// bypass it entirely, and an `errcode` answer arrives with a 200. This
/// wrapper covers both.
const METADATA_MAX_ATTEMPTS: usize = 3;

/// The query parameter every `GET /metadata/{identifier}` sends.
///
/// With it, the Metadata API answers an item that cannot be located but
/// shows signs of existing with an integer `errcode` beside `error`, instead
/// of the `{}` it sends for an identifier with no item. A successful answer
/// is unchanged. See <https://archive.org/developers/md-read.html>, "Extended
/// errors", and [`check_item_body`] for how the answer is read.
pub const EXTENDED_ERR_PARAM: (&str, &str) = ("extended_err", "1");

/// Fetch full metadata for an item.
///
/// Sends S3 auth headers when credentials are configured, which is
/// required for accessing private/dark items.
///
/// The Metadata API answers a missing item with HTTP 200 and `{}`; that is
/// [`IaError::NotFound`], as is a 404. An item that exists but cannot be
/// served is [`IaError::ItemUnavailable`] (see [`check_item_body`]).
///
/// Retries up to three times, with exponential backoff, on transient
/// body-decode failures (truncated body, unexpected EOF, malformed JSON —
/// sometimes returned with a 200 status by upstream proxies) and on an
/// [`IaError::ItemUnavailable`] that [`IaError::is_retryable`] accepts
/// (`errcode` 102, data node offline).
pub async fn get(client: &IaClient, identifier: &str) -> Result<ItemMetadata> {
    get_with_params(client, identifier, &[]).await
}

/// Fetch full metadata for an item, appending extra query parameters.
///
/// Sends S3 auth headers when credentials are configured, which is
/// required for accessing private/dark items. Extra parameters are appended
/// to the query string after [`EXTENDED_ERR_PARAM`] — e.g. `dark_ok=1` is
/// required (in addition to auth) for the metadata API to return the
/// metadata of a dark item.
///
/// Errors and retries as [`get`].
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
            Err(e) if attempt < METADATA_MAX_ATTEMPTS && is_retryable_read_error(&e) => {
                let backoff = backoff_for_attempt(attempt);
                tracing::warn!(
                    identifier,
                    attempt,
                    max = METADATA_MAX_ATTEMPTS,
                    backoff_ms = backoff.as_millis() as u64,
                    error = %crate::error::format_error_chain(&e),
                    "metadata fetch failed, retrying",
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
    let mut req = client.http().get(url).query(&[EXTENDED_ERR_PARAM]);
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
        let retry_after = crate::retry::extract_retry_after(response.headers());
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status: status.as_u16(),
            message: body,
            retry_after,
        });
    }

    // Parsed as a `Value` first: `ItemMetadata` defaults every field, so
    // `{}` would otherwise become an empty item.
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(reqwest_middleware::Error::from)?;
    check_item_body(identifier, &body)?;

    Ok(serde_json::from_value(body)?)
}

/// Classify a 200 answer from `GET /metadata/{identifier}?extended_err=1`.
///
/// 1. A body with a `metadata`, `files` or `is_dark` key is the item:
///    `Ok(())`. `is_dark` alone is what a dark item read without `dark_ok=1`
///    answers; it stays an item, as before `extended_err`. An `errcode`
///    beside these keys is ignored, since a code such as 106 (a secondary
///    metadata copy was used) does not stop the item being served.
/// 2. Otherwise a body with an `error` or an `errcode` key is
///    [`IaError::ItemUnavailable`]. An `errcode` that is not an integer in
///    `u16` range counts as absent.
/// 3. Otherwise (`{}`, which the API sends for an identifier with no item,
///    or any other body with none of those keys) it is [`IaError::NotFound`].
///
/// ```
/// use ia_core::IaError;
/// use ia_core::metadata::check_item_body;
/// use serde_json::json;
///
/// assert!(check_item_body("nasa", &json!({"metadata": {"identifier": "nasa"}})).is_ok());
/// assert!(matches!(check_item_body("gone", &json!({})), Err(IaError::NotFound(_))));
/// assert!(matches!(
///     check_item_body("gone", &json!({"error": "item was deleted", "errcode": 104})),
///     Err(IaError::ItemUnavailable { code: Some(104), .. })
/// ));
/// ```
pub fn check_item_body(identifier: &str, body: &serde_json::Value) -> Result<()> {
    if ["metadata", "files", "is_dark"]
        .iter()
        .any(|key| body.get(key).is_some())
    {
        return Ok(());
    }
    let error = body.get("error");
    let errcode = body.get("errcode");
    if error.is_none() && errcode.is_none() {
        return Err(IaError::NotFound(identifier.to_string()));
    }
    let message = match error {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "no message".to_string(),
    };
    Err(IaError::ItemUnavailable {
        identifier: identifier.to_string(),
        code: errcode
            .and_then(serde_json::Value::as_u64)
            .and_then(|c| u16::try_from(c).ok()),
        message,
    })
}

fn is_retryable_read_error(err: &IaError) -> bool {
    match err {
        IaError::Network(reqwest_middleware::Error::Reqwest(e)) => is_retryable_body_error(e),
        IaError::ItemUnavailable { .. } => err.is_retryable(),
        _ => false,
    }
}

fn backoff_for_attempt(attempt: usize) -> std::time::Duration {
    // attempt=1 → 500ms, attempt=2 → 1.5s, attempt=3 → 4.5s.
    let base = std::time::Duration::from_millis(500);
    base * 3u32.saturating_pow(attempt.saturating_sub(1) as u32)
}

/// Check if an item exists.
///
/// `Ok(false)` when the API answers `{}` or 404; `Err` when it reports the
/// item as unavailable ([`IaError::ItemUnavailable`]) or the request fails,
/// so a caller can tell "gone" from "unreachable right now".
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

    // -- extended_err=1 and the 200 `{}` answer for a missing item (#118) --

    fn item_body(identifier: &str) -> serde_json::Value {
        serde_json::json!({
            "metadata": {"identifier": identifier},
            "files": []
        })
    }

    #[tokio::test]
    async fn get_sends_extended_err() {
        use wiremock::matchers::query_param;

        let mock_server = MockServer::start().await;

        // Matches only with extended_err=1 on the query string.
        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .and(query_param("extended_err", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(item_body("test-item")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let item = get(&client, "test-item").await.unwrap();
        assert_eq!(
            item.metadata.identifier.as_ref().map(|v| v.first()),
            Some("test-item")
        );
    }

    #[tokio::test]
    async fn get_with_params_sends_extended_err_beside_caller_params() {
        use wiremock::matchers::query_param;

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/dark-item"))
            .and(query_param("dark_ok", "1"))
            .and(query_param("extended_err", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(item_body("dark-item")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let params = vec![("dark_ok".to_string(), "1".to_string())];
        get_with_params(&client, "dark-item", &params)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn get_empty_body_is_not_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/missing"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get(&client, "missing").await;
        assert!(
            matches!(result, Err(IaError::NotFound(ref id)) if id == "missing"),
            "expected NotFound, got {result:?}"
        );
    }

    #[tokio::test]
    async fn exists_returns_false_for_empty_body() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/missing"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        assert!(!exists(&client, "missing").await.unwrap());
    }

    #[tokio::test]
    async fn get_errcode_102_is_retried_then_succeeds() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/offline-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": "item unavailable (data node(s) offline or not responding)",
                "errcode": 102
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/metadata/offline-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(item_body("offline-item")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let item = get(&client, "offline-item").await.unwrap();
        assert_eq!(
            item.metadata.identifier.as_ref().map(|v| v.first()),
            Some("offline-item")
        );
    }

    #[tokio::test]
    async fn get_errcode_102_gives_up_after_retry_budget() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/offline-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": "item unavailable (data node(s) offline or not responding)",
                "errcode": 102
            })))
            .expect(METADATA_MAX_ATTEMPTS as u64)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get(&client, "offline-item").await;
        match result {
            Err(IaError::ItemUnavailable {
                identifier,
                code,
                message,
            }) => {
                assert_eq!(identifier, "offline-item");
                assert_eq!(code, Some(102));
                assert!(message.contains("data node"), "message: {message}");
            }
            other => panic!("expected ItemUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_errcode_104_is_not_retried() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/deleted-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": "item was deleted",
                "errcode": 104
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get(&client, "deleted-item").await;
        assert!(
            matches!(
                result,
                Err(IaError::ItemUnavailable {
                    code: Some(104),
                    ..
                })
            ),
            "expected ItemUnavailable 104, got {result:?}"
        );
    }

    #[tokio::test]
    async fn get_error_without_errcode_is_item_unavailable() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/odd-item"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"error": "something went wrong"})),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get(&client, "odd-item").await;
        assert!(
            matches!(
                result,
                Err(IaError::ItemUnavailable { code: None, ref message, .. })
                    if message == "something went wrong"
            ),
            "expected ItemUnavailable without a code, got {result:?}"
        );
    }

    #[tokio::test]
    async fn get_item_body_with_errcode_is_still_the_item() {
        // 106 (secondary metadata copy used) is informational: when the
        // item came back, the read succeeds.
        let mock_server = MockServer::start().await;

        let mut body = item_body("test-item");
        body["errcode"] = serde_json::json!(106);
        body["error"] = serde_json::json!("unbalanced locations");
        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let item = get(&client, "test-item").await.unwrap();
        assert_eq!(
            item.metadata.identifier.as_ref().map(|v| v.first()),
            Some("test-item")
        );
    }

    #[tokio::test]
    async fn exists_errs_for_errcode_response() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/deleted-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": "item was deleted",
                "errcode": 104
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = exists(&client, "deleted-item").await;
        assert!(
            matches!(result, Err(IaError::ItemUnavailable { .. })),
            "expected Err(ItemUnavailable), got {result:?}"
        );
    }

    #[test]
    fn check_item_body_rule() {
        use serde_json::json;

        assert!(check_item_body("x", &json!({"metadata": {}})).is_ok());
        assert!(check_item_body("x", &json!({"files": []})).is_ok());
        // A dark item read without dark_ok: kept as an item, as before.
        assert!(check_item_body("x", &json!({"is_dark": true})).is_ok());
        assert!(matches!(
            check_item_body("x", &json!({})),
            Err(IaError::NotFound(_))
        ));
        assert!(matches!(
            check_item_body("x", &json!(null)),
            Err(IaError::NotFound(_))
        ));
        assert!(matches!(
            check_item_body("x", &json!({"server": "ia000000.us.archive.org"})),
            Err(IaError::NotFound(_))
        ));
        assert!(matches!(
            check_item_body("x", &json!({"errcode": 102})),
            Err(IaError::ItemUnavailable { code: Some(102), ref message, .. })
                if message == "no message"
        ));
        // An errcode that is not an integer in u16 range counts as absent.
        assert!(matches!(
            check_item_body("x", &json!({"error": "e", "errcode": "101"})),
            Err(IaError::ItemUnavailable { code: None, .. })
        ));
        assert!(matches!(
            check_item_body("x", &json!({"error": "e", "errcode": 70000})),
            Err(IaError::ItemUnavailable { code: None, .. })
        ));
        assert!(matches!(
            check_item_body("x", &json!({"error": "e", "errcode": 101})),
            Err(IaError::ItemUnavailable {
                code: Some(101),
                ..
            })
        ));
    }
}
