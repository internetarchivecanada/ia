use std::pin::Pin;
use std::time::Duration;

use futures::Stream;
use reqwest_middleware::RequestBuilder;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::client::IaClient;
use crate::error::{IaError, Result};

/// Retries of one search request after a 429, before it fails as
/// [`IaError::RateLimited`]. The transport's own budget for a 5xx is the
/// same three.
const THROTTLE_RETRIES: u32 = 3;

/// Send one search request, retrying a 429.
///
/// The transport middleware retries a 5xx and leaves a 429 to the
/// application layer. For a search there is no shared `RateLimiter` to
/// pause (one stream, one worker), so the page request itself is retried:
/// up to [`THROTTLE_RETRIES`] times, waiting the server's `Retry-After`
/// when the response carried one, otherwise the standard backoff. Past the
/// budget the error is [`IaError::RateLimited`] with the last header's
/// value, 0 when there was none (as the tasks API reports a 429; metadata
/// reads use 30 there, a pause length rather than a report).
async fn send_page(
    req: RequestBuilder,
    client: &IaClient,
    timeout: Option<Duration>,
) -> Result<reqwest::Response> {
    let req = with_s3_auth(req, client);
    let req = match timeout {
        Some(t) => req.timeout(t),
        None => req,
    };
    let backoff = crate::retry::backoff_policy(
        crate::retry::STANDARD_MIN_DELAY,
        crate::retry::STANDARD_MAX_DELAY,
        THROTTLE_RETRIES,
    );
    let mut n_past_retries: u32 = 0;
    loop {
        // Every search request has a cloneable body (query parameters, or
        // a JSON document), so this cannot fail in practice. `Config` is the
        // variant the crate uses for internal invariants (see `upload::single`).
        let attempt = req.try_clone().ok_or_else(|| {
            IaError::Config("internal error: search request body is not cloneable".into())
        })?;
        let resp = attempt.send().await?;
        if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Ok(resp);
        }
        let retry_after = crate::retry::retry_after_wait(resp.headers());
        if n_past_retries >= THROTTLE_RETRIES {
            return Err(IaError::RateLimited {
                retry_after: retry_after.map(|d| d.as_secs()).unwrap_or(0),
            });
        }
        let wait = crate::retry::wait_before_retry(retry_after, &backoff, n_past_retries);
        warn!(
            retry = n_past_retries + 1,
            wait_ms = wait.as_millis() as u64,
            retry_after = retry_after.is_some(),
            "search rate limited (429), retrying"
        );
        tokio::time::sleep(wait).await;
        n_past_retries += 1;
    }
}

/// Turn a non-success search response into [`IaError::Http`], keeping its
/// `Retry-After` for the caller.
async fn http_error(resp: reqwest::Response) -> IaError {
    let status = resp.status().as_u16();
    let retry_after = crate::retry::extract_retry_after(resp.headers());
    IaError::Http {
        status,
        message: resp.text().await.unwrap_or_default(),
        retry_after,
    }
}

/// Attach S3 auth header if credentials are configured.
fn with_s3_auth(req: RequestBuilder, client: &IaClient) -> RequestBuilder {
    if let Some(auth) = client
        .config()
        .s3_access
        .as_deref()
        .zip(client.config().s3_secret.as_deref())
        .map(|(a, s)| format!("LOW {a}:{s}"))
    {
        req.header("Authorization", auth)
    } else {
        req
    }
}

/// Default page size for advanced search queries.
pub const DEFAULT_ADVANCED_ROWS: usize = 50;

/// A single search result from any backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Item identifier.
    pub identifier: String,

    /// All returned fields (depends on the query's `fl` parameter).
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// Search options shared across backends.
#[derive(Debug, Clone, Default)]
pub struct SearchOpts {
    /// Fields to return (default: all via `*`).
    pub fields: Vec<String>,
    /// Sort clauses (e.g., "downloads desc").
    pub sorts: Vec<String>,
    /// Maximum number of results (0 = unlimited).
    pub count: usize,
    /// Page size for advanced search (0 = use [`DEFAULT_ADVANCED_ROWS`]).
    pub rows: usize,
    /// Timeout per request in seconds, covering the whole request from
    /// connect to the last byte of the response body. The client's own
    /// connect and read timeouts stay in force, so this can only shorten
    /// them; each retry of a request gets the same limit. `None` sets no
    /// per-request limit.
    pub timeout: Option<u64>,
    /// Extra query parameters.
    pub params: Vec<(String, String)>,
    /// FTS: skip `!L` prefix for raw Elasticsearch DSL queries.
    pub dsl: bool,
}

// ─── Scrape API ───────────────────────────────────────────────────────────────

/// Response from the scrape API.
#[derive(Debug, Deserialize)]
struct ScrapeResponse {
    items: Option<Vec<serde_json::Value>>,
    cursor: Option<String>,
    total: Option<u64>,
}

/// Count total results matching a scrape query without fetching items.
///
/// Uses POST (matching Python `internetarchive` behavior — GET returns wrong totals).
pub async fn num_found(client: &IaClient, query: &str, params: &[(String, String)]) -> Result<u64> {
    num_found_with_timeout(client, query, params, None).await
}

/// [`num_found`] with a per-request timeout.
pub async fn num_found_with_timeout(
    client: &IaClient,
    query: &str,
    params: &[(String, String)],
    timeout: Option<Duration>,
) -> Result<u64> {
    let url = client.url("/services/search/v1/scrape");
    let mut req = client
        .http()
        .post(&url)
        .query(&[("q", query), ("total_only", "true")]);
    for (k, v) in params {
        req = req.query(&[(k.as_str(), v.as_str())]);
    }
    let resp = send_page(req, client, timeout).await?;

    if !resp.status().is_success() {
        return Err(http_error(resp).await);
    }

    let body: ScrapeResponse = resp.json().await.map_err(reqwest_middleware::Error::from)?;
    Ok(body.total.unwrap_or(0))
}

/// Count total results matching an advanced search query without fetching items.
///
/// Uses GET to `/advancedsearch.php` and reads `response.numFound`.
pub async fn advanced_num_found(
    client: &IaClient,
    query: &str,
    params: &[(String, String)],
) -> Result<u64> {
    advanced_num_found_with_timeout(client, query, params, None).await
}

/// [`advanced_num_found`] with a per-request timeout.
pub async fn advanced_num_found_with_timeout(
    client: &IaClient,
    query: &str,
    params: &[(String, String)],
    timeout: Option<Duration>,
) -> Result<u64> {
    let url = client.url("/advancedsearch.php");
    let mut req = client
        .http()
        .get(&url)
        .query(&[("q", query), ("output", "json"), ("rows", "0")]);
    for (k, v) in params {
        req = req.query(&[(k.as_str(), v.as_str())]);
    }
    let resp = send_page(req, client, timeout).await?;

    if !resp.status().is_success() {
        return Err(http_error(resp).await);
    }

    let body: AdvancedSearchResponse =
        resp.json().await.map_err(reqwest_middleware::Error::from)?;
    Ok(body.response.num_found)
}

/// Count total FTS results matching a query without fetching items.
///
/// Uses GET to the FTS endpoint (matching Python `internetarchive` behavior).
/// Reads `hits.total` from the response.
pub async fn fts_num_found(
    client: &IaClient,
    query: &str,
    dsl: bool,
    params: &[(String, String)],
) -> Result<u64> {
    fts_num_found_with_timeout(client, query, dsl, params, None).await
}

/// [`fts_num_found`] with a per-request timeout.
pub async fn fts_num_found_with_timeout(
    client: &IaClient,
    query: &str,
    dsl: bool,
    params: &[(String, String)],
    timeout: Option<Duration>,
) -> Result<u64> {
    let base_url = client.fts_base_url();
    let q = if dsl {
        query.to_string()
    } else {
        format!("!L {query}")
    };

    let mut req = client.http().get(&base_url).query(&[("q", &q)]);
    for (k, v) in params {
        req = req.query(&[(k.as_str(), v.as_str())]);
    }
    let resp = send_page(req, client, timeout).await?;
    let body = read_fts_response(resp).await?;
    Ok(body.hits.and_then(|h| h.total).map_or(0, |t| t.value()))
}

/// Search using the scrape API (cursor-based pagination).
/// Returns a stream of search results.
pub fn scrape<'a>(
    client: &'a IaClient,
    query: &str,
    opts: &SearchOpts,
) -> Pin<Box<dyn Stream<Item = Result<SearchResult>> + Send + 'a>> {
    let url = client.url("/services/search/v1/scrape");
    let fields = if opts.fields.is_empty() {
        "*".to_string()
    } else {
        opts.fields.join(",")
    };
    let sorts = opts.sorts.join(",");
    let count = opts.count;
    let query = query.to_string();
    let extra_params = opts.params.clone();
    let timeout = opts.timeout.map(Duration::from_secs);

    Box::pin(async_stream::try_stream! {
        let mut cursor: Option<String> = None;
        let mut yielded = 0usize;

        loop {
            let mut req = client
                .http()
                .post(&url)
                .query(&[("q", &query), ("fields", &fields)]);

            if !sorts.is_empty() {
                req = req.query(&[("sorts", &sorts)]);
            }

            if let Some(ref c) = cursor {
                req = req.query(&[("cursor", c)]);
            }

            for (k, v) in &extra_params {
                req = req.query(&[(k.as_str(), v.as_str())]);
            }

            let resp = send_page(req, client, timeout).await?;

            if !resp.status().is_success() {
                Err(http_error(resp).await)?;
                return; // unreachable but needed for type inference
            }

            let body: ScrapeResponse = resp.json().await.map_err(reqwest_middleware::Error::from)?;
            let items = body.items.unwrap_or_default();

            if items.is_empty() {
                break;
            }

            for item in items {
                let result = parse_search_result(item)?;
                yielded += 1;
                yield result;

                if count > 0 && yielded >= count {
                    return;
                }
            }

            match body.cursor {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => break,
            }
        }
    })
}

// ─── Advanced Search ──────────────────────────────────────────────────────────

/// Response from advanced search.
#[derive(Debug, Deserialize)]
struct AdvancedSearchResponse {
    response: AdvancedSearchInner,
}

#[derive(Debug, Deserialize)]
struct AdvancedSearchInner {
    #[serde(rename = "numFound")]
    num_found: u64,
    docs: Vec<serde_json::Value>,
}

/// Search using the advanced search API (page-based pagination).
pub fn advanced<'a>(
    client: &'a IaClient,
    query: &str,
    opts: &SearchOpts,
) -> Pin<Box<dyn Stream<Item = Result<SearchResult>> + Send + 'a>> {
    let url = client.url("/advancedsearch.php");
    let fields = if opts.fields.is_empty() {
        "*".to_string()
    } else {
        opts.fields.join(",")
    };
    let sorts_str = if opts.sorts.is_empty() {
        String::new()
    } else {
        opts.sorts.join(",")
    };
    let count = opts.count;
    let query = query.to_string();
    let rows = if opts.rows > 0 {
        opts.rows
    } else {
        DEFAULT_ADVANCED_ROWS
    };
    let extra_params = opts.params.clone();
    let timeout = opts.timeout.map(Duration::from_secs);
    // If extra_params already contains "rows", let it take precedence over the
    // default so that `--parameters 'rows=3'` works as expected.
    let has_extra_rows = extra_params.iter().any(|(k, _)| k == "rows");

    Box::pin(async_stream::try_stream! {
        let mut page = 1usize;
        let mut yielded = 0usize;

        loop {
            let mut req = client
                .http()
                .get(&url)
                .query(&[
                    ("q", query.as_str()),
                    ("fl[]", fields.as_str()),
                    ("page", &page.to_string()),
                    ("output", "json"),
                ]);

            if !has_extra_rows {
                req = req.query(&[("rows", &rows.to_string())]);
            }

            if !sorts_str.is_empty() {
                req = req.query(&[("sort[]", sorts_str.as_str())]);
            }

            for (k, v) in &extra_params {
                req = req.query(&[(k.as_str(), v.as_str())]);
            }

            let resp = send_page(req, client, timeout).await?;

            if !resp.status().is_success() {
                Err(http_error(resp).await)?;
                return;
            }

            let body: AdvancedSearchResponse = resp.json().await.map_err(reqwest_middleware::Error::from)?;
            let docs = body.response.docs;

            if docs.is_empty() {
                break;
            }

            debug!(
                page,
                docs = docs.len(),
                total = body.response.num_found,
                "advanced search page"
            );

            for doc in docs {
                let result = parse_search_result(doc)?;
                yielded += 1;
                yield result;

                if count > 0 && yielded >= count {
                    return;
                }
            }

            // If we've yielded all results, we're done
            if yielded >= body.response.num_found as usize {
                break;
            }

            page += 1;
        }
    })
}

// ─── Full-Text Search ─────────────────────────────────────────────────────────

/// Results per FTS request when the caller sets no `size`.
const FTS_DEFAULT_SIZE: u64 = 1000;

/// Longest part of an unexpected response body quoted in an error, in
/// characters.
const BODY_SNIPPET_CHARS: usize = 200;

/// Response from the FTS API: a page of hits, or one of the wrapper's error
/// envelopes.
#[derive(Debug, Deserialize)]
struct FtsResponse {
    hits: Option<FtsHits>,
    #[serde(rename = "_scroll_id")]
    scroll_id: Option<String>,
    /// `{"msg_type": "ERROR", "text": "the parameter q is required"}` when
    /// the API rejects the request (served with HTTP 200).
    user_message: Option<FtsUserMessage>,
    /// `"UPSTREAM_ERROR"` and the like, with a human-readable `message`.
    error: Option<serde_json::Value>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FtsUserMessage {
    msg_type: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FtsHits {
    total: Option<FtsTotal>,
    #[serde(default)]
    hits: Vec<FtsHit>,
}

/// `hits.total`: a number, or `{"value": N, "relation": ...}` as
/// Elasticsearch 7 and later return it unless told otherwise.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FtsTotal {
    Count(u64),
    Object { value: u64 },
}

impl FtsTotal {
    fn value(&self) -> u64 {
        match self {
            FtsTotal::Count(n) | FtsTotal::Object { value: n } => *n,
        }
    }
}

#[derive(Debug, Deserialize)]
struct FtsHit {
    /// `identifier|sha1` of the matching file.
    #[serde(rename = "_id")]
    id: Option<String>,
    #[serde(rename = "_source")]
    source: Option<serde_json::Value>,
    fields: Option<serde_json::Value>,
    /// The matching text, with matches marked.
    highlight: Option<serde_json::Value>,
}

/// The start of `text`, at most [`BODY_SNIPPET_CHARS`] characters.
fn body_snippet(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(BODY_SNIPPET_CHARS) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_string(),
    }
}

/// Read one FTS response.
///
/// A non-success status is [`IaError::Http`] with the body text, as for the
/// other search backends. A success status whose body is not JSON, or that
/// carries one of the wrapper's error envelopes (`user_message`, or a string
/// or object `error` with `message`), is [`IaError::SearchFailed`]: the API answers a request
/// it rejects with HTTP 200, so the status alone does not tell a rejection
/// from an empty result.
async fn read_fts_response(resp: reqwest::Response) -> Result<FtsResponse> {
    if !resp.status().is_success() {
        return Err(http_error(resp).await);
    }
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(reqwest_middleware::Error::from)?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| IaError::SearchFailed {
            status,
            message: format!("response is not JSON ({e}): {}", body_snippet(&text)),
        })?;
    let body: FtsResponse = serde_json::from_value(value).map_err(|e| IaError::SearchFailed {
        status,
        message: format!("unexpected response ({e}): {}", body_snippet(&text)),
    })?;

    if let Some(um) = &body.user_message {
        let message = um
            .text
            .clone()
            .filter(|t| !t.is_empty())
            .or_else(|| um.msg_type.clone())
            .unwrap_or_else(|| body_snippet(&text));
        return Err(IaError::SearchFailed { status, message });
    }
    // `"error": "UPSTREAM_ERROR"`, or an Elasticsearch-style error object.
    if let Some(error) = body
        .error
        .as_ref()
        .filter(|e| e.is_string() || e.is_object())
    {
        let message = body.message.clone().unwrap_or_else(|| match error {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        });
        return Err(IaError::SearchFailed { status, message });
    }
    Ok(body)
}

/// A `--parameters`-style value as the JSON type the FTS API expects:
/// `size` and `from` as numbers and `scroll` as a boolean when they parse,
/// everything else as a string.
fn fts_param_value(key: &str, value: &str) -> serde_json::Value {
    match key {
        "size" | "from" => value
            .parse::<u64>()
            .map_or_else(|_| value.into(), serde_json::Value::from),
        "scroll" => match value {
            "true" => true.into(),
            "false" => false.into(),
            _ => value.into(),
        },
        _ => value.into(),
    }
}

/// The body of the first FTS request: `q`, `size` and `scroll`, then the
/// extra parameters. `from` turns scroll off: the API rejects `from` with
/// `scroll: true` (a 502) and honours it without.
fn fts_first_body(query: &str, params: &[(String, String)]) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert("q".into(), query.into());
    body.insert("size".into(), FTS_DEFAULT_SIZE.into());
    body.insert("scroll".into(), true.into());
    for (k, v) in params {
        body.insert(k.clone(), fts_param_value(k, v));
    }
    if body.contains_key("from") {
        body.insert("scroll".into(), false.into());
    }
    serde_json::Value::Object(body)
}

/// Turn one hit into a [`SearchResult`], or `None` when it names no item.
///
/// The identifier is `fields.identifier` (the first element of the list the
/// API sends), else the part of `_id` before `|`. `_source` and `fields`
/// become the result's fields, without `identifier`, plus the hit's `_id`
/// (`identifier|sha1` of the matching file, unique per hit) and
/// `highlight`.
fn fts_hit_to_result(hit: FtsHit) -> Option<SearchResult> {
    let mut fields = serde_json::Map::new();
    if let Some(serde_json::Value::Object(src)) = hit.source {
        fields.extend(src);
    }
    if let Some(serde_json::Value::Object(f)) = hit.fields {
        fields.extend(f);
    }
    let from_fields = match fields.remove("identifier") {
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Array(values)) => values.into_iter().find_map(|v| match v {
            serde_json::Value::String(s) => Some(s),
            _ => None,
        }),
        _ => None,
    };
    let identifier = from_fields
        .filter(|s| !s.is_empty())
        .or_else(|| {
            hit.id.as_deref().map(|id| {
                id.split_once('|')
                    .map_or(id, |(ident, _)| ident)
                    .to_string()
            })
        })
        .filter(|s| !s.is_empty())?;
    if let Some(id) = hit.id {
        fields.insert("_id".into(), id.into());
    }
    if let Some(highlight) = hit.highlight {
        fields.insert("highlight".into(), highlight);
    }
    Some(SearchResult { identifier, fields })
}

/// Search using the full-text search API (scroll-based pagination).
///
/// By default, prepends `!L` to the query for literal text matching
/// (matching Python `internetarchive` behavior). Set `opts.dsl = true`
/// to skip the prefix for raw Elasticsearch DSL queries.
///
/// The first request is a POST with the JSON body
/// `{"q": ..., "size": 1000, "scroll": true}` plus `opts.params` (`size` and
/// `from` sent as numbers). Each `_scroll_id` is followed with
/// `POST ?scroll=true` and `{"scroll_id": ...}` until a page has no hits or
/// no `_scroll_id`. A `from` parameter turns scroll off: one page is fetched.
///
/// Each hit is one matching file or page, so an item can appear more than
/// once. [`SearchResult::identifier`] is the item identifier.
///
/// # Errors
///
/// The stream yields [`IaError::SearchFailed`] when the API reports an error
/// in a success response (it answers a rejected request with HTTP 200) or
/// sends a body that is not JSON, and [`IaError::Http`] for a non-success
/// status.
pub fn fts<'a>(
    client: &'a IaClient,
    query: &str,
    opts: &SearchOpts,
) -> Pin<Box<dyn Stream<Item = Result<SearchResult>> + Send + 'a>> {
    let count = opts.count;
    let timeout = opts.timeout.map(Duration::from_secs);
    // Prepend !L for literal text search unless DSL mode is active
    let query = if opts.dsl {
        query.to_string()
    } else {
        format!("!L {query}")
    };
    let first_body = fts_first_body(&query, &opts.params);
    let scroll = first_body["scroll"] == serde_json::Value::Bool(true);

    // FTS uses a different host — configurable for testability
    let base_url = client.fts_base_url();

    Box::pin(async_stream::try_stream! {
        let mut scroll_id: Option<String> = None;
        let mut yielded = 0usize;

        loop {
            let req = if let Some(ref sid) = scroll_id {
                // Scroll request
                let body = serde_json::to_vec(&serde_json::json!({ "scroll_id": sid }))
                    .map_err(IaError::Json)?;
                client
                    .http()
                    .post(format!("{base_url}?scroll=true"))
                    .header("content-type", "application/json")
                    .body(body)
            } else {
                let body = serde_json::to_vec(&first_body).map_err(IaError::Json)?;
                client
                    .http()
                    .post(&base_url)
                    .header("content-type", "application/json")
                    .body(body)
            };
            let resp: reqwest::Response = send_page(req, client, timeout).await?;
            let body = read_fts_response(resp).await?;
            let FtsHits { total, hits } = body.hits.unwrap_or(FtsHits { total: None, hits: vec![] });

            if hits.is_empty() {
                break;
            }

            debug!(hits = hits.len(), total = ?total.map(|t| t.value()), "FTS page");

            for hit in hits {
                let Some(result) = fts_hit_to_result(hit) else {
                    warn!("FTS hit names no identifier, skipping");
                    continue;
                };

                yielded += 1;
                yield result;

                if count > 0 && yielded >= count {
                    return;
                }
            }

            if !scroll {
                break;
            }
            match body.scroll_id {
                Some(sid) if !sid.is_empty() => scroll_id = Some(sid),
                _ => break,
            }
        }
    })
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Parse a JSON value into a SearchResult.
fn parse_search_result(value: serde_json::Value) -> Result<SearchResult> {
    let obj = value
        .as_object()
        .ok_or_else(|| IaError::Config("search result is not an object".to_string()))?;

    let identifier = obj
        .get("identifier")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut fields = obj.clone();
    fields.remove("identifier");

    Ok(SearchResult { identifier, fields })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TEST_ACCESS: &str = "test_access";
    const TEST_SECRET: &str = "test_secret";
    const TEST_AUTH: &str = "LOW test_access:test_secret";

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.fts_host = Some(host.to_string());
        config.general.secure = false;
        config
    }

    fn mock_config_with_auth(server_uri: &str) -> crate::config::IaConfig {
        let mut config = mock_config(server_uri);
        config.s3_access = Some(TEST_ACCESS.to_string());
        config.s3_secret = Some(TEST_SECRET.to_string());
        config
    }

    // ─── --timeout ────────────────────────────────────────────────────────

    const SLOW: Duration = Duration::from_millis(1500);

    async fn slow_server() -> MockServer {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "items": [{"identifier": "a"}],
            "cursor": "",
            "total": 1,
            "response": {"numFound": 1, "docs": [{"identifier": "a"}]},
            "hits": {"total": 1, "hits": [{"fields": {"identifier": "a"}}]}
        });
        Mock::given(wiremock::matchers::any())
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(body)
                    .set_delay(SLOW),
            )
            .mount(&server)
            .await;
        server
    }

    fn timed_out(e: &IaError) -> bool {
        matches!(e, IaError::Network(reqwest_middleware::Error::Reqwest(e)) if e.is_timeout())
    }

    fn short() -> Option<u64> {
        Some(1)
    }

    async fn first_error(
        mut s: Pin<Box<dyn Stream<Item = Result<SearchResult>> + Send + '_>>,
    ) -> Option<IaError> {
        match s.next().await {
            Some(Err(e)) => Some(e),
            _ => None,
        }
    }

    #[tokio::test]
    async fn scrape_timeout_applies() {
        let server = slow_server().await;
        let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
        let opts = SearchOpts {
            timeout: short(),
            ..Default::default()
        };
        let e = first_error(scrape(&client, "q", &opts))
            .await
            .expect("timeout error");
        assert!(timed_out(&e), "{e:?}");
        let ok: Vec<_> = scrape(&client, "q", &SearchOpts::default()).collect().await;
        assert!(ok.iter().all(Result::is_ok) && ok.len() == 1);
    }

    #[tokio::test]
    async fn advanced_timeout_applies() {
        let server = slow_server().await;
        let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
        let opts = SearchOpts {
            timeout: short(),
            count: 1,
            ..Default::default()
        };
        let e = first_error(advanced(&client, "q", &opts))
            .await
            .expect("timeout error");
        assert!(timed_out(&e), "{e:?}");
        let opts = SearchOpts {
            count: 1,
            ..Default::default()
        };
        let ok: Vec<_> = advanced(&client, "q", &opts).collect().await;
        assert!(ok.iter().all(Result::is_ok) && ok.len() == 1);
    }

    #[tokio::test]
    async fn fts_timeout_applies() {
        let server = slow_server().await;
        let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
        let opts = SearchOpts {
            timeout: short(),
            count: 1,
            ..Default::default()
        };
        let e = first_error(fts(&client, "q", &opts))
            .await
            .expect("timeout error");
        assert!(timed_out(&e), "{e:?}");
        let opts = SearchOpts {
            count: 1,
            ..Default::default()
        };
        let ok: Vec<_> = fts(&client, "q", &opts).collect().await;
        assert!(ok.iter().all(Result::is_ok) && ok.len() == 1);
    }

    #[tokio::test]
    async fn num_found_timeout_applies() {
        let server = slow_server().await;
        let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
        let t = Some(Duration::from_millis(200));
        let e = num_found_with_timeout(&client, "q", &[], t)
            .await
            .unwrap_err();
        assert!(timed_out(&e), "{e:?}");
        assert_eq!(num_found(&client, "q", &[]).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn advanced_num_found_timeout_applies() {
        let server = slow_server().await;
        let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
        let t = Some(Duration::from_millis(200));
        let e = advanced_num_found_with_timeout(&client, "q", &[], t)
            .await
            .unwrap_err();
        assert!(timed_out(&e), "{e:?}");
        assert_eq!(advanced_num_found(&client, "q", &[]).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn fts_num_found_timeout_applies() {
        let server = slow_server().await;
        let client = IaClient::from_config(mock_config(&server.uri())).unwrap();
        let t = Some(Duration::from_millis(200));
        let e = fts_num_found_with_timeout(&client, "q", false, &[], t)
            .await
            .unwrap_err();
        assert!(timed_out(&e), "{e:?}");
        assert_eq!(fts_num_found(&client, "q", false, &[]).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn scrape_returns_results() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/search/v1/scrape"))
            .and(query_param("q", "collection:test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [
                    {"identifier": "item1"},
                    {"identifier": "item2"},
                    {"identifier": "item3"}
                ],
                "cursor": "",
                "total": 3
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results: Vec<Result<SearchResult>> =
            scrape(&client, "collection:test", &SearchOpts::default())
                .collect()
                .await;

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].as_ref().unwrap().identifier, "item1");
        assert_eq!(results[2].as_ref().unwrap().identifier, "item3");
    }

    #[tokio::test]
    async fn scrape_with_count_limit() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/search/v1/scrape"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [
                    {"identifier": "item1"},
                    {"identifier": "item2"},
                    {"identifier": "item3"},
                    {"identifier": "item4"},
                    {"identifier": "item5"}
                ],
                "cursor": "next",
                "total": 100
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let opts = SearchOpts {
            count: 2,
            ..Default::default()
        };
        let results: Vec<Result<SearchResult>> =
            scrape(&client, "collection:test", &opts).collect().await;

        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn num_found_returns_total() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/search/v1/scrape"))
            .and(query_param("total_only", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 42
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let count = num_found(&client, "collection:test", &[]).await.unwrap();
        assert_eq!(count, 42);
    }

    #[tokio::test]
    async fn advanced_search_returns_results() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/advancedsearch.php"))
            .and(query_param("output", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "response": {
                    "numFound": 2,
                    "docs": [
                        {"identifier": "a1", "title": "Item A"},
                        {"identifier": "b2", "title": "Item B"}
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results: Vec<Result<SearchResult>> =
            advanced(&client, "test query", &SearchOpts::default())
                .collect()
                .await;

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].as_ref().unwrap().identifier, "a1");
        let title = results[0]
            .as_ref()
            .unwrap()
            .fields
            .get("title")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(title, "Item A");
    }

    #[tokio::test]
    async fn advanced_search_uses_rows_param() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/advancedsearch.php"))
            .and(query_param("rows", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "response": {
                    "numFound": 1,
                    "docs": [{"identifier": "item1", "title": "Test"}]
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let opts = SearchOpts {
            rows: 50,
            ..Default::default()
        };
        let results: Vec<Result<SearchResult>> = advanced(&client, "test", &opts).collect().await;

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn advanced_search_extra_params_rows_overrides_default() {
        let mock_server = MockServer::start().await;

        // The mock expects rows=3 (from extra params), NOT rows=50 (default).
        Mock::given(method("GET"))
            .and(path("/advancedsearch.php"))
            .and(query_param("rows", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "response": {
                    "numFound": 3,
                    "docs": [
                        {"identifier": "a"},
                        {"identifier": "b"},
                        {"identifier": "c"},
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let opts = SearchOpts {
            params: vec![("rows".to_string(), "3".to_string())],
            count: 3,
            ..Default::default()
        };
        let results: Vec<Result<SearchResult>> = advanced(&client, "test", &opts).collect().await;

        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn parse_search_result_extracts_identifier() {
        let value = serde_json::json!({
            "identifier": "nasa",
            "title": "NASA",
            "downloads": 1000
        });
        let result = parse_search_result(value).unwrap();
        assert_eq!(result.identifier, "nasa");
        assert!(result.fields.contains_key("title"));
        assert!(!result.fields.contains_key("identifier"));
    }

    #[tokio::test]
    async fn scrape_handles_empty_response() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/search/v1/scrape"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [],
                "total": 0
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results: Vec<Result<SearchResult>> = scrape(&client, "nothing", &SearchOpts::default())
            .collect()
            .await;

        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn fts_num_found_returns_total() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/ia-pub-fts-api"))
            .and(query_param("q", "!L test query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": {
                    "total": 1234,
                    "hits": []
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let count = fts_num_found(&client, "test query", false, &[])
            .await
            .unwrap();
        assert_eq!(count, 1234);
    }

    #[tokio::test]
    async fn fts_num_found_dsl_skips_prefix() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/ia-pub-fts-api"))
            .and(query_param("q", "raw dsl query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": {
                    "total": 42,
                    "hits": []
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let count = fts_num_found(&client, "raw dsl query", true, &[])
            .await
            .unwrap();
        assert_eq!(count, 42);
    }

    /// One FTS page with the given `_id`s (each hit's `fields.identifier` is
    /// the part before `|`) and `_scroll_id`.
    fn fts_page(ids: &[&str], scroll_id: &str) -> serde_json::Value {
        let hits: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| {
                let ident = id.split('|').next().unwrap();
                serde_json::json!({
                    "_index": "fts_main_v2",
                    "_id": id,
                    "fields": {"identifier": [ident]}
                })
            })
            .collect();
        serde_json::json!({
            "hits": {"total": ids.len(), "hits": hits},
            "_scroll_id": scroll_id
        })
    }

    /// The API's answer to a request it rejects, as served live on
    /// 2026-11-05 for a body without `q`.
    fn fts_user_message_envelope() -> serde_json::Value {
        serde_json::json!({
            "user_message": {"msg_type": "ERROR", "text": "the parameter q is required"},
            "ia-pub-fts-api-wrapper": true
        })
    }

    /// The JSON body of the `n`th request the mock server received.
    async fn request_body(mock_server: &MockServer, n: usize) -> serde_json::Value {
        let requests = mock_server.received_requests().await.unwrap();
        serde_json::from_slice(&requests[n].body).unwrap()
    }

    async fn collect_fts(
        client: &IaClient,
        query: &str,
        opts: &SearchOpts,
    ) -> Vec<Result<SearchResult>> {
        fts(client, query, opts).collect().await
    }

    /// The first request carries the query under `q` (the API answers
    /// `query` with "the parameter q is required"), `size` as a number and
    /// `scroll` as a boolean.
    #[tokio::test]
    async fn fts_first_request_body_has_q_numeric_size_boolean_scroll() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_page(&["item1|abc123"], "")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "test query", &SearchOpts::default()).await;
        assert_eq!(results.len(), 1);

        let body = request_body(&mock_server, 0).await;
        assert_eq!(
            body,
            serde_json::json!({"q": "!L test query", "size": 1000, "scroll": true})
        );
    }

    #[tokio::test]
    async fn fts_dsl_mode_skips_prefix() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_page(&["item1|abc123"], "")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let opts = SearchOpts {
            dsl: true,
            ..Default::default()
        };
        let results = collect_fts(&client, "raw dsl", &opts).await;
        assert_eq!(results.len(), 1);

        let body = request_body(&mock_server, 0).await;
        assert_eq!(body["q"], "raw dsl");
        assert!(body.get("query").is_none(), "{body}");
    }

    /// A 200 carrying `user_message` and no hits is the server rejecting the
    /// request, not an empty result.
    #[tokio::test]
    async fn fts_user_message_on_200_is_an_error_with_the_server_text() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_user_message_envelope()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        assert_eq!(results.len(), 1);
        match &results[0] {
            Err(e @ IaError::SearchFailed { status: 200, .. }) => {
                assert!(e.to_string().contains("the parameter q is required"), "{e}");
            }
            other => panic!("expected SearchFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fts_user_message_on_400_is_an_http_error_with_the_server_text() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(400).set_body_json(fts_user_message_envelope()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        assert_eq!(results.len(), 1);
        match &results[0] {
            Err(IaError::Http {
                status: 400,
                message,
                ..
            }) => assert!(message.contains("the parameter q is required"), "{message}"),
            other => panic!("expected Http 400, got {other:?}"),
        }
    }

    /// The wrapper's other error shape (served live with a 502 on
    /// 2026-11-05) is an error on a 2xx too.
    #[tokio::test]
    async fn fts_error_key_on_200_is_an_error_with_the_server_message() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": "UPSTREAM_ERROR",
                "message": "The search service is temporarily unavailable, please try again later.",
                "ia-pub-fts-api-wrapper": true
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        assert_eq!(results.len(), 1);
        let err = results[0].as_ref().unwrap_err();
        assert!(
            matches!(err, IaError::SearchFailed { status: 200, .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("temporarily unavailable"), "{err}");
    }

    #[tokio::test]
    async fn fts_non_json_body_is_an_error_naming_the_body() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>maintenance</html>"))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        assert_eq!(results.len(), 1);
        let err = results[0].as_ref().unwrap_err();
        assert!(
            matches!(err, IaError::SearchFailed { status: 200, .. }),
            "{err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("not JSON"), "{msg}");
        assert!(msg.contains("<html>maintenance</html>"), "{msg}");
    }

    #[tokio::test]
    async fn fts_connection_error_is_one_error() {
        let mut config = crate::IaConfig::default();
        config.general.fts_host = Some("127.0.0.1:1".to_string()); // nothing listening
        config.general.secure = false;
        let client = IaClient::from_config_no_retry(config).unwrap();

        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
    }

    /// No hits and no `user_message` is a genuine empty result.
    #[tokio::test]
    async fn fts_no_hits_is_a_clean_empty_stream() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_page(&[], "sid-unused")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "zzzznomatch", &SearchOpts::default()).await;
        assert!(results.is_empty(), "{results:?}");
        mock_server.verify().await;
    }

    #[tokio::test]
    async fn fts_empty_scroll_id_yields_hits_and_ends() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_page(&["a|1", "b|2"], "")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        let ids: Vec<&str> = results
            .iter()
            .map(|r| r.as_ref().unwrap().identifier.as_str())
            .collect();
        assert_eq!(ids, ["a", "b"]);
        mock_server.verify().await;
    }

    #[tokio::test]
    async fn fts_missing_scroll_id_yields_hits_and_ends() {
        let mock_server = MockServer::start().await;
        let mut page = fts_page(&["a|1"], "");
        page.as_object_mut().unwrap().remove("_scroll_id");
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].as_ref().unwrap().identifier, "a");
        mock_server.verify().await;
    }

    /// A `_scroll_id` continues at `?scroll=true` with `{"scroll_id": ...}`,
    /// and both pages come out in order.
    #[tokio::test]
    async fn fts_scroll_continuation_yields_both_pages_in_order() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .and(query_param("scroll", "true"))
            .and(wiremock::matchers::body_json(
                serde_json::json!({"scroll_id": "sid-1"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_page(&["c|3", "d|4"], "")))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"q": "!L computer"}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(fts_page(&["a|1", "b|2"], "sid-1")),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        let ids: Vec<&str> = results
            .iter()
            .map(|r| r.as_ref().unwrap().identifier.as_str())
            .collect();
        assert_eq!(ids, ["a", "b", "c", "d"]);
        mock_server.verify().await;
    }

    /// `size` and `from` go out as numbers and `scroll` as a boolean even
    /// though they arrive as strings; other parameters stay strings.
    #[tokio::test]
    async fn fts_params_are_sent_with_json_types() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_page(&["a|1"], "")))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let opts = SearchOpts {
            params: vec![("size".into(), "5".into()), ("scope".into(), "all".into())],
            ..Default::default()
        };
        collect_fts(&client, "computer", &opts).await;

        let body = request_body(&mock_server, 0).await;
        assert_eq!(
            body,
            serde_json::json!({"q": "!L computer", "size": 5, "scroll": true, "scope": "all"})
        );
    }

    /// The API rejects `from` together with `scroll: true` (a 502 live) and
    /// honours it without, so `from` fetches one page and stops, even if the
    /// answer carries a `_scroll_id`.
    #[tokio::test]
    async fn fts_from_turns_scroll_off_and_fetches_one_page() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(fts_page(&["a|1", "b|2"], "sid-1")),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let opts = SearchOpts {
            params: vec![("size".into(), "2".into()), ("from".into(), "10".into())],
            ..Default::default()
        };
        let results = collect_fts(&client, "computer", &opts).await;
        assert_eq!(results.len(), 2);

        let body = request_body(&mock_server, 0).await;
        assert_eq!(
            body,
            serde_json::json!({"q": "!L computer", "size": 2, "from": 10, "scroll": false})
        );
        mock_server.verify().await;
    }

    /// The identifier is `fields.identifier`, not `_id` (which is
    /// `identifier|sha1` of the matching file); without `fields.identifier`
    /// it is the part of `_id` before `|`. A plain string `identifier` (as
    /// scroll continuation pages send) works too. `_id` and `highlight` are
    /// kept in the fields.
    #[tokio::test]
    async fn fts_identifier_comes_from_fields_and_highlight_is_kept() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": {"total": 2, "hits": [
                    {
                        "_id": "comingofageinlan0000your|1319f8442aca0dbe3c25322fea16539a99ce3f94",
                        "highlight": {"text": ["{{{computer}}} literacy"]},
                        "fields": {"identifier": ["comingofageinlan0000your"], "page_num": [12]}
                    },
                    {
                        "_id": "otheritem|ec3f975d49670f9c1751f0da47f7e57ba2b486ca",
                        "fields": {"page_num": [3]}
                    },
                    {
                        "_id": "thirditem|90fb7780f187ad5239e10f999eb31e12679b15f4",
                        "fields": {"identifier": "thirditem", "page_num": 7}
                    }
                ]},
                "_scroll_id": ""
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        let first = results[0].as_ref().unwrap();
        assert_eq!(first.identifier, "comingofageinlan0000your");
        assert!(
            !first.fields.contains_key("identifier"),
            "{:?}",
            first.fields
        );
        assert_eq!(
            first.fields["highlight"],
            serde_json::json!({"text": ["{{{computer}}} literacy"]})
        );
        assert_eq!(first.fields["page_num"], serde_json::json!([12]));
        assert_eq!(
            first.fields["_id"],
            "comingofageinlan0000your|1319f8442aca0dbe3c25322fea16539a99ce3f94"
        );
        assert_eq!(results[1].as_ref().unwrap().identifier, "otheritem");
        let third = results[2].as_ref().unwrap();
        assert_eq!(third.identifier, "thirditem");
        assert!(
            !third.fields.contains_key("identifier"),
            "{:?}",
            third.fields
        );
    }

    /// A rejection on a continuation page comes after the first page's
    /// results and ends the stream.
    #[tokio::test]
    async fn fts_error_on_a_continuation_page_ends_the_stream_after_the_results() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .and(query_param("scroll", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_user_message_envelope()))
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"q": "!L computer"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_page(&["a|1"], "sid-1")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        assert_eq!(results.len(), 2, "{results:?}");
        assert_eq!(results[0].as_ref().unwrap().identifier, "a");
        assert!(
            matches!(results[1], Err(IaError::SearchFailed { status: 200, .. })),
            "{:?}",
            results[1]
        );
        mock_server.verify().await;
    }

    /// `scroll=false` alone fetches one page; a `size` that is not a number
    /// is passed through as a string for the API to judge.
    #[tokio::test]
    async fn fts_scroll_false_fetches_one_page_and_bad_size_stays_a_string() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_page(&["a|1"], "sid-1")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let opts = SearchOpts {
            params: vec![
                ("scroll".into(), "false".into()),
                ("size".into(), "abc".into()),
            ],
            ..Default::default()
        };
        let results = collect_fts(&client, "computer", &opts).await;
        assert_eq!(results.len(), 1);

        let body = request_body(&mock_server, 0).await;
        assert_eq!(
            body,
            serde_json::json!({"q": "!L computer", "size": "abc", "scroll": false})
        );
        mock_server.verify().await;
    }

    /// Only a string or object `error` is an error envelope; a `false` or
    /// `null` beside hits is not.
    #[tokio::test]
    async fn fts_error_false_beside_hits_is_not_an_error() {
        let mock_server = MockServer::start().await;
        let mut page = fts_page(&["a|1"], "");
        page["error"] = serde_json::json!(false);
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results = collect_fts(&client, "computer", &SearchOpts::default()).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].as_ref().unwrap().identifier, "a");
    }

    /// Elasticsearch 7 and later report `hits.total` as `{"value": N}`
    /// unless told otherwise; the API sends a number today.
    #[tokio::test]
    async fn fts_num_found_reads_total_as_an_object() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ia-pub-fts-api"))
            .and(query_param("q", "!L test query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": {"total": {"value": 6524253, "relation": "eq"}, "hits": []}
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let count = fts_num_found(&client, "test query", false, &[])
            .await
            .unwrap();
        assert_eq!(count, 6524253);
    }

    #[tokio::test]
    async fn fts_num_found_user_message_is_an_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ia-pub-fts-api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fts_user_message_envelope()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let err = fts_num_found(&client, "test", false, &[])
            .await
            .unwrap_err();
        assert!(
            matches!(err, IaError::SearchFailed { status: 200, .. }),
            "{err:?}"
        );
        assert!(
            err.to_string().contains("the parameter q is required"),
            "{err}"
        );
    }

    // ─── advanced_num_found ──────────────────────────────────────────────────

    #[tokio::test]
    async fn advanced_num_found_returns_total() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/advancedsearch.php"))
            .and(query_param("q", "collection:test"))
            .and(query_param("output", "json"))
            .and(query_param("rows", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "response": {
                    "numFound": 11234,
                    "docs": []
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let count = advanced_num_found(&client, "collection:test", &[])
            .await
            .unwrap();
        assert_eq!(count, 11234);
    }

    // ─── S3 auth header tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn num_found_sends_auth_header() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/search/v1/scrape"))
            .and(query_param("total_only", "true"))
            .and(header("Authorization", TEST_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 42
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
        let count = num_found(&client, "test", &[]).await.unwrap();
        assert_eq!(count, 42);
    }

    #[tokio::test]
    async fn advanced_num_found_sends_auth_header() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/advancedsearch.php"))
            .and(query_param("rows", "0"))
            .and(header("Authorization", TEST_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "response": {
                    "numFound": 99,
                    "docs": []
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
        let count = advanced_num_found(&client, "test", &[]).await.unwrap();
        assert_eq!(count, 99);
    }

    #[tokio::test]
    async fn fts_num_found_sends_auth_header() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/ia-pub-fts-api"))
            .and(header("Authorization", TEST_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": {
                    "total": 500,
                    "hits": []
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
        let count = fts_num_found(&client, "test", false, &[]).await.unwrap();
        assert_eq!(count, 500);
    }

    #[tokio::test]
    async fn scrape_sends_auth_header() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/search/v1/scrape"))
            .and(header("Authorization", TEST_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"identifier": "item1"}],
                "cursor": "",
                "total": 1
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
        let results: Vec<Result<SearchResult>> = scrape(&client, "test", &SearchOpts::default())
            .collect()
            .await;

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn advanced_sends_auth_header() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/advancedsearch.php"))
            .and(header("Authorization", TEST_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "response": {
                    "numFound": 1,
                    "docs": [{"identifier": "item1"}]
                }
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
        let results: Vec<Result<SearchResult>> = advanced(&client, "test", &SearchOpts::default())
            .collect()
            .await;

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn fts_sends_auth_header() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .and(header("Authorization", TEST_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": {
                    "total": 1,
                    "hits": [
                        {
                            "_id": "item1|abc",
                            "_source": {},
                            "fields": {}
                        }
                    ]
                },
                "_scroll_id": ""
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config_with_auth(&mock_server.uri())).unwrap();
        let results: Vec<Result<SearchResult>> =
            fts(&client, "test", &SearchOpts::default()).collect().await;

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn no_auth_header_without_credentials() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/services/search/v1/scrape"))
            .and(query_param("total_only", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 10
            })))
            .mount(&mock_server)
            .await;

        // mock_config does NOT set s3_access/s3_secret
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let count = num_found(&client, "test", &[]).await.unwrap();
        assert_eq!(count, 10);

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].headers.get("Authorization").is_none(),
            "Authorization header should not be sent without S3 credentials"
        );
    }

    // -- 429 handling: retried with Retry-After, not a terminal error --

    /// Serve `status` (with `Retry-After: 1`) for the first `times`
    /// requests to `path_`, then the given JSON body.
    async fn mount_throttled_then(
        mock_server: &MockServer,
        http_method: &str,
        path_: &str,
        times: u64,
        body: serde_json::Value,
    ) {
        Mock::given(method(http_method))
            .and(path(path_))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "1")
                    .set_body_string("slow down"),
            )
            .up_to_n_times(times)
            .expect(times)
            .mount(mock_server)
            .await;
        Mock::given(method(http_method))
            .and(path(path_))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(mock_server)
            .await;
    }

    fn scrape_body(ids: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "items": ids.iter().map(|id| serde_json::json!({"identifier": id})).collect::<Vec<_>>(),
            "cursor": "",
            "total": ids.len()
        })
    }

    /// A 429 on a scrape page is retried after the server's Retry-After and
    /// the stream completes.
    #[tokio::test]
    async fn scrape_429_with_retry_after_is_retried() {
        let mock_server = MockServer::start().await;
        mount_throttled_then(
            &mock_server,
            "POST",
            "/services/search/v1/scrape",
            1,
            scrape_body(&["item1", "item2"]),
        )
        .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();

        let started = std::time::Instant::now();
        let results: Vec<Result<SearchResult>> =
            scrape(&client, "collection:test", &SearchOpts::default())
                .collect()
                .await;
        let ids: Vec<&str> = results
            .iter()
            .map(|r| r.as_ref().unwrap().identifier.as_str())
            .collect();
        assert_eq!(ids, ["item1", "item2"]);
        assert!(
            started.elapsed() >= std::time::Duration::from_secs(1),
            "Retry-After: 1 was not waited for ({:?})",
            started.elapsed()
        );
        mock_server.verify().await;
    }

    /// Three retries, then the stream yields `RateLimited` with the header's
    /// value. `Retry-After: 0` keeps the test fast; the budget is the point.
    #[tokio::test]
    async fn scrape_429_past_the_budget_is_rate_limited() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services/search/v1/scrape"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "0")
                    .set_body_string("slow down"),
            )
            .expect(4)
            .mount(&mock_server)
            .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();

        let results: Vec<Result<SearchResult>> =
            scrape(&client, "collection:test", &SearchOpts::default())
                .collect()
                .await;
        assert_eq!(results.len(), 1);
        assert!(
            matches!(results[0], Err(IaError::RateLimited { retry_after: 0 })),
            "got {:?}",
            results[0]
        );
        mock_server.verify().await;
    }

    #[tokio::test]
    async fn num_found_429_with_retry_after_is_retried() {
        let mock_server = MockServer::start().await;
        mount_throttled_then(
            &mock_server,
            "POST",
            "/services/search/v1/scrape",
            1,
            serde_json::json!({"items": [], "total": 42}),
        )
        .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            num_found(&client, "collection:test", &[]).await.unwrap(),
            42
        );
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));
        mock_server.verify().await;
    }

    #[tokio::test]
    async fn advanced_429_with_retry_after_is_retried() {
        let mock_server = MockServer::start().await;
        mount_throttled_then(
            &mock_server,
            "GET",
            "/advancedsearch.php",
            1,
            serde_json::json!({"response": {"numFound": 1, "docs": [{"identifier": "a1"}]}}),
        )
        .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let started = std::time::Instant::now();
        let results: Vec<Result<SearchResult>> =
            advanced(&client, "test query", &SearchOpts::default())
                .collect()
                .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].as_ref().unwrap().identifier, "a1");
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));
        mock_server.verify().await;
    }

    #[tokio::test]
    async fn fts_429_with_retry_after_is_retried() {
        let mock_server = MockServer::start().await;
        mount_throttled_then(
            &mock_server,
            "POST",
            "/ia-pub-fts-api",
            1,
            serde_json::json!({
                "hits": {"total": 1, "hits": [{"_id": "item1|abc", "_source": {}, "fields": {"identifier": ["item1"]}}]},
                "_scroll_id": ""
            }),
        )
        .await;
        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let started = std::time::Instant::now();
        let results: Vec<Result<SearchResult>> = fts(&client, "test query", &SearchOpts::default())
            .collect()
            .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].as_ref().unwrap().identifier, "item1");
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));
        mock_server.verify().await;
    }
}
