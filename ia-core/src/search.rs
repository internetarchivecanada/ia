use std::pin::Pin;

use futures::Stream;
use reqwest_middleware::RequestBuilder;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::client::IaClient;
use crate::error::{IaError, Result};

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
    /// Timeout per request in seconds.
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
    let url = client.url("/services/search/v1/scrape");
    let mut req = client
        .http()
        .post(&url)
        .query(&[("q", query), ("total_only", "true")]);
    for (k, v) in params {
        req = req.query(&[(k.as_str(), v.as_str())]);
    }
    let resp = with_s3_auth(req, client).send().await?;

    if !resp.status().is_success() {
        return Err(IaError::Http {
            status: resp.status().as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
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
    let url = client.url("/advancedsearch.php");
    let mut req = client
        .http()
        .get(&url)
        .query(&[("q", query), ("output", "json"), ("rows", "0")]);
    for (k, v) in params {
        req = req.query(&[(k.as_str(), v.as_str())]);
    }
    let resp = with_s3_auth(req, client).send().await?;

    if !resp.status().is_success() {
        return Err(IaError::Http {
            status: resp.status().as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
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
    let resp = with_s3_auth(req, client).send().await?;

    if !resp.status().is_success() {
        return Err(IaError::Http {
            status: resp.status().as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }

    let body: FtsResponse = resp.json().await.map_err(reqwest_middleware::Error::from)?;
    Ok(body.hits.and_then(|h| h.total).unwrap_or(0))
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

            let resp = with_s3_auth(req, client).send().await?;

            if !resp.status().is_success() {
                Err(IaError::Http {
                    status: resp.status().as_u16(),
                    message: resp.text().await.unwrap_or_default(),
                })?;
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
                    ("rows", &rows.to_string()),
                    ("page", &page.to_string()),
                    ("output", "json"),
                ]);

            if !sorts_str.is_empty() {
                req = req.query(&[("sort[]", sorts_str.as_str())]);
            }

            for (k, v) in &extra_params {
                req = req.query(&[(k.as_str(), v.as_str())]);
            }

            let resp = with_s3_auth(req, client).send().await?;

            if !resp.status().is_success() {
                Err(IaError::Http {
                    status: resp.status().as_u16(),
                    message: resp.text().await.unwrap_or_default(),
                })?;
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

/// Response from the FTS API.
#[derive(Debug, Deserialize)]
struct FtsResponse {
    hits: Option<FtsHits>,
    #[serde(rename = "_scroll_id")]
    scroll_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FtsHits {
    total: Option<u64>,
    hits: Vec<FtsHit>,
}

#[derive(Debug, Deserialize)]
struct FtsHit {
    #[serde(rename = "_id")]
    id: Option<String>,
    #[serde(rename = "_source")]
    source: Option<serde_json::Value>,
    fields: Option<serde_json::Value>,
}

/// Search using the full-text search API (scroll-based pagination).
///
/// By default, prepends `!L` to the query for literal text matching
/// (matching Python `internetarchive` behavior). Set `opts.dsl = true`
/// to skip the prefix for raw Elasticsearch DSL queries.
pub fn fts<'a>(
    client: &'a IaClient,
    query: &str,
    opts: &SearchOpts,
) -> Pin<Box<dyn Stream<Item = Result<SearchResult>> + Send + 'a>> {
    let count = opts.count;
    // Prepend !L for literal text search unless DSL mode is active
    let query = if opts.dsl {
        query.to_string()
    } else {
        format!("!L {query}")
    };
    let extra_params = opts.params.clone();

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
                // Initial request
                let mut json_body = serde_json::json!({
                    "query": query,
                    "size": 1000,
                    "scroll": true,
                });

                for (k, v) in &extra_params {
                    json_body[k.as_str()] = serde_json::Value::String(v.clone());
                }

                let body = serde_json::to_vec(&json_body).map_err(IaError::Json)?;
                client
                    .http()
                    .post(&base_url)
                    .header("content-type", "application/json")
                    .body(body)
            };
            let resp: reqwest::Response = with_s3_auth(req, client).send().await?;

            if !resp.status().is_success() {
                Err(IaError::Http {
                    status: resp.status().as_u16(),
                    message: resp.text().await.unwrap_or_default(),
                })?;
                return;
            }

            let body: FtsResponse = resp.json().await.map_err(reqwest_middleware::Error::from)?;
            let hits = body.hits.unwrap_or(FtsHits { total: None, hits: vec![] });

            if hits.hits.is_empty() {
                break;
            }

            debug!(hits = hits.hits.len(), total = ?hits.total, "FTS page");

            for hit in hits.hits {
                let identifier = hit.id.unwrap_or_default();
                if identifier.is_empty() {
                    warn!("FTS hit missing _id, skipping");
                    continue;
                }

                // Merge _source and fields into a single map
                let mut fields = serde_json::Map::new();
                if let Some(serde_json::Value::Object(src)) = hit.source {
                    fields.extend(src);
                }
                if let Some(serde_json::Value::Object(f)) = hit.fields {
                    fields.extend(f);
                }

                yielded += 1;
                yield SearchResult { identifier, fields };

                if count > 0 && yielded >= count {
                    return;
                }
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
    use wiremock::matchers::{body_string_contains, header, method, path, query_param};
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

    #[tokio::test]
    async fn fts_prepends_literal_prefix() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .and(body_string_contains("!L test query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": {
                    "total": 1,
                    "hits": [
                        {
                            "_id": "item1|abc123",
                            "_source": {},
                            "fields": {"identifier": ["item1"]}
                        }
                    ]
                },
                "_scroll_id": ""
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let results: Vec<Result<SearchResult>> = fts(&client, "test query", &SearchOpts::default())
            .collect()
            .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].as_ref().unwrap().identifier, "item1|abc123");
    }

    #[tokio::test]
    async fn fts_dsl_mode_skips_prefix() {
        let mock_server = MockServer::start().await;

        // Mock that matches the raw query WITHOUT !L prefix.
        // body_string_contains("raw dsl") matches the query in the JSON body.
        // We also verify !L is NOT present by using a strict mock: if the
        // code incorrectly prepends !L, the query field would be "!L raw dsl"
        // which still contains "raw dsl", so we add an explicit assertion
        // via a second mock that would catch the !L prefix.
        Mock::given(method("POST"))
            .and(path("/ia-pub-fts-api"))
            .and(body_string_contains("\"query\":\"raw dsl\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": {
                    "total": 1,
                    "hits": [
                        {
                            "_id": "item1|abc123",
                            "_source": {},
                            "fields": {"identifier": ["item1"]}
                        }
                    ]
                },
                "_scroll_id": ""
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let opts = SearchOpts {
            dsl: true,
            ..Default::default()
        };
        let results: Vec<Result<SearchResult>> = fts(&client, "raw dsl", &opts).collect().await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].as_ref().unwrap().identifier, "item1|abc123");
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
}
