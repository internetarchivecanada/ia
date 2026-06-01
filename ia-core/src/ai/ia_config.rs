//! AI Config JSON — stored in collection items, defines LLM model, prompt,
//! page selection, and response schema for AI metadata extraction.
//!
//! Format on archive.org: "AI Config JSON"
//! Compatible with the extraction system's `the config editor` web UI.

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::client::IaClient;
use crate::error::{IaError, Result};
use crate::types::ItemMetadata;

/// Top-level AI Config JSON envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IaAiConfig {
    pub result: IaAiConfigResult,
}

/// The content of an AI Config JSON file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IaAiConfigResult {
    /// The LLM model name used for extraction (e.g., "gpt-5-nano").
    pub model_name: String,
    /// The system prompt for the extraction LLM.
    pub prompt: String,
    /// Which pages to send to the LLM (cover, first N pages, etc.).
    #[serde(rename = "pageInfo")]
    pub page_info: Vec<PageInfo>,
    /// The JSON Schema defining expected response format.
    pub schema: SchemaWrapper,
}

/// Specification for which page(s) to extract from a scanned item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageInfo {
    /// Whether this is a cover page or a normal (interior) page.
    #[serde(rename = "type")]
    pub page_type: PageType,
    /// Number of pages to select (only meaningful for `Normal` type).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

/// The type of page to extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageType {
    /// The cover image (always index 0 in the JP2 zip).
    Cover,
    /// Title page.
    Title,
    /// Interior pages (sequential after cover).
    Normal,
    /// Unrecognized page type — tolerate new types from the server.
    #[serde(other)]
    Other,
}

/// Wrapper around the JSON Schema response format specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaWrapper {
    pub format: SchemaFormat,
}

/// The response format specification for the extraction LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaFormat {
    /// The format type (always "json_schema").
    #[serde(rename = "type")]
    pub format_type: String,
    /// A human-readable name for the schema.
    pub name: String,
    /// The JSON Schema object defining expected fields.
    pub schema: serde_json::Value,
}

/// Compute total number of pages needed from a pageInfo specification.
pub fn compute_page_count(page_info: &[PageInfo]) -> usize {
    page_info
        .iter()
        .map(|p| match p.page_type {
            PageType::Cover | PageType::Title => 1,
            PageType::Normal | PageType::Other => p.count.unwrap_or(1),
        })
        .sum()
}

/// Fetch the AI config from a collection item.
///
/// Looks for a file with format "AI Config JSON" in the collection's file
/// list, then downloads and parses it.
pub async fn fetch_ai_config(client: &IaClient, collection_id: &str) -> Result<IaAiConfig> {
    let item = client.get_item(collection_id).await?;
    let config_file = find_ai_config_file(&item).ok_or_else(|| {
        IaError::NotFound(format!("no AI config found in collection {collection_id}"))
    })?;

    let url = crate::download::ensure_cnt_zero(&client.url(&format!(
        "/download/{}/{}",
        collection_id,
        urlencoding::encode(&config_file),
    )));

    debug!(
        collection = collection_id,
        file = config_file,
        "fetching AI config"
    );

    let response = client
        .http()
        .get(&url)
        .send()
        .await
        .map_err(|e| IaError::Http {
            status: 0,
            message: format!("failed to fetch AI config: {e}"),
        })?;

    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status,
            message: format!("failed to fetch AI config from {collection_id}: {body}"),
        });
    }

    let body = response.text().await.map_err(|e| IaError::Http {
        status: 0,
        message: format!("failed to read AI config body: {e}"),
    })?;

    // Try wrapped format first (`{"result": {...}}`), then unwrapped (fields at top level).
    if let Ok(config) = serde_json::from_str::<IaAiConfig>(&body) {
        return Ok(config);
    }
    let result: IaAiConfigResult = serde_json::from_str(&body)?;
    Ok(IaAiConfig { result })
}

/// Load an AI config from a local file path.
///
/// Accepts both the wrapped format (`{"result": {...}}`) used on archive.org
/// and the unwrapped format (config fields directly at top level).
pub fn load_ai_config_from_file(path: &std::path::Path) -> Result<IaAiConfig> {
    let contents = std::fs::read_to_string(path).map_err(|e| {
        IaError::Config(format!(
            "failed to read AI config from {}: {e}",
            path.display()
        ))
    })?;
    // Try wrapped format first, then unwrapped
    if let Ok(config) = serde_json::from_str::<IaAiConfig>(&contents) {
        return Ok(config);
    }
    let result: IaAiConfigResult = serde_json::from_str(&contents)?;
    Ok(IaAiConfig { result })
}

/// Resolve the AI config for an item by walking its collection chain.
///
/// Returns the config and the collection ID it was found in.
/// Checks the most specific collection first, then walks up the chain.
pub async fn resolve_ai_config(
    client: &IaClient,
    item: &ItemMetadata,
) -> Result<(String, IaAiConfig)> {
    let collections = item
        .metadata
        .collection
        .as_ref()
        .map(|c| c.to_vec())
        .unwrap_or_default();

    if collections.is_empty() {
        return Err(IaError::Config(
            "item has no collections — cannot resolve AI config".to_string(),
        ));
    }

    // Try each collection in order (most specific first as listed in metadata)
    for collection in &collections {
        debug!(collection, "checking for AI config");
        match fetch_ai_config(client, collection).await {
            Ok(config) => return Ok((collection.to_string(), config)),
            Err(IaError::NotFound(_)) => continue,
            Err(e) => return Err(e),
        }
    }

    Err(IaError::NotFound(format!(
        "no AI config found in any collection: {}",
        collections.join(", ")
    )))
}

/// Find the AI config filename in an item's file list.
///
/// Looks for files with format "AI Config JSON".
fn find_ai_config_file(item: &ItemMetadata) -> Option<String> {
    item.files.iter().find_map(|f| {
        let format = f.format.as_deref().unwrap_or_default();
        if format == "AI Config JSON" {
            Some(f.name.clone())
        } else {
            None
        }
    })
}

/// Upload an AI config to a collection item via S3 PUT.
pub async fn create_ai_config(
    client: &IaClient,
    collection_id: &str,
    config: &IaAiConfig,
) -> Result<()> {
    write_ai_config(client, collection_id, config).await
}

/// Overwrite an existing AI config on a collection item.
pub async fn update_ai_config(
    client: &IaClient,
    collection_id: &str,
    config: &IaAiConfig,
) -> Result<()> {
    write_ai_config(client, collection_id, config).await
}

/// Delete the AI config from a collection item.
///
/// Uses the S3 DELETE endpoint to remove the config file.
pub async fn delete_ai_config(client: &IaClient, collection_id: &str) -> Result<()> {
    let item = client.get_item(collection_id).await?;
    let config_file = find_ai_config_file(&item).ok_or_else(|| {
        IaError::NotFound(format!("no AI config found in collection {collection_id}"))
    })?;

    let (access, secret) = client.require_auth()?;
    let url = build_s3_config_url(client, collection_id, &config_file);

    debug!(
        collection = collection_id,
        file = config_file,
        "deleting AI config"
    );

    let response = client
        .raw_http()
        .delete(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .send()
        .await
        .map_err(|e| IaError::Http {
            status: 0,
            message: format!("failed to delete AI config: {e}"),
        })?;

    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status,
            message: format!("failed to delete AI config from {collection_id}: {body}"),
        });
    }

    Ok(())
}

/// Sensible defaults matching the config editor.
pub fn default_ai_config() -> IaAiConfig {
    IaAiConfig {
        result: IaAiConfigResult {
            model_name: "gpt-5-nano".to_string(),
            prompt: DEFAULT_EXTRACTION_PROMPT.to_string(),
            page_info: vec![
                PageInfo {
                    page_type: PageType::Cover,
                    count: None,
                },
                PageInfo {
                    page_type: PageType::Normal,
                    count: Some(5),
                },
            ],
            schema: SchemaWrapper {
                format: SchemaFormat {
                    format_type: "json_schema".to_string(),
                    name: "metadata_extraction".to_string(),
                    schema: default_schema(),
                },
            },
        },
    }
}

const DEFAULT_EXTRACTION_PROMPT: &str = "\
You are a metadata extraction expert. You will be shown page images from a \
scanned book or document. Extract the following metadata fields from the \
visible text on these pages. If a field cannot be determined from the images, \
omit it from the response. Be precise and use the exact text as shown.";

fn default_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "title": {
                "type": "string",
                "description": "The title of the work"
            },
            "creator": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Author(s) or creator(s)"
            },
            "date": {
                "type": "string",
                "description": "Publication or creation date"
            },
            "language": {
                "type": "string",
                "description": "Language of the work (ISO 639 code or full name)"
            },
            "publisher": {
                "type": "string",
                "description": "Publisher name"
            },
            "description": {
                "type": "string",
                "description": "Brief description or abstract"
            }
        },
        "required": ["title"],
        "additionalProperties": true
    })
}

/// Build the S3 URL for a config file operation.
fn build_s3_config_url(client: &IaClient, collection_id: &str, filename: &str) -> String {
    let protocol = client.protocol();
    let host = client.host();
    let encoded = urlencoding::encode(filename);
    if host == "archive.org" {
        format!("{protocol}://s3.us.archive.org/{collection_id}/{encoded}")
    } else {
        format!("{protocol}://{host}/{collection_id}/{encoded}")
    }
}

/// Internal: upload config JSON to a collection item via S3 PUT.
async fn write_ai_config(
    client: &IaClient,
    collection_id: &str,
    config: &IaAiConfig,
) -> Result<()> {
    let (access, secret) = client.require_auth()?;
    let filename = format!("{collection_id}_ai_config.json");
    let body = serde_json::to_vec_pretty(config)?;

    let url = build_s3_config_url(client, collection_id, &filename);

    debug!(collection = collection_id, filename, "uploading AI config");

    let response = client
        .raw_http()
        .put(&url)
        .header("Authorization", format!("LOW {access}:{secret}"))
        .header("Content-Type", "application/json")
        .header("Content-Length", body.len())
        .header("x-archive-meta-format", "AI Config JSON")
        .body(body)
        .send()
        .await
        .map_err(|e| IaError::Http {
            status: 0,
            message: format!("failed to upload AI config: {e}"),
        })?;

    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status,
            message: format!("failed to upload AI config to {collection_id}: {body}"),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ia_ai_config_serde_roundtrip() {
        let config = default_ai_config();
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: IaAiConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.result.model_name, "gpt-5-nano");
        assert_eq!(parsed.result.page_info.len(), 2);
        assert_eq!(parsed.result.page_info[0].page_type, PageType::Cover);
        assert!(parsed.result.page_info[0].count.is_none());
        assert_eq!(parsed.result.page_info[1].page_type, PageType::Normal);
        assert_eq!(parsed.result.page_info[1].count, Some(5));
    }

    #[test]
    fn page_type_serde() {
        let cover = serde_json::to_string(&PageType::Cover).unwrap();
        assert_eq!(cover, "\"cover\"");
        let normal = serde_json::to_string(&PageType::Normal).unwrap();
        assert_eq!(normal, "\"normal\"");

        let parsed_cover: PageType = serde_json::from_str("\"cover\"").unwrap();
        assert_eq!(parsed_cover, PageType::Cover);
        let parsed_normal: PageType = serde_json::from_str("\"normal\"").unwrap();
        assert_eq!(parsed_normal, PageType::Normal);
    }

    #[test]
    fn compute_page_count_cover_and_normal() {
        let pages = vec![
            PageInfo {
                page_type: PageType::Cover,
                count: None,
            },
            PageInfo {
                page_type: PageType::Normal,
                count: Some(5),
            },
        ];
        assert_eq!(compute_page_count(&pages), 6);
    }

    #[test]
    fn compute_page_count_empty() {
        assert_eq!(compute_page_count(&[]), 0);
    }

    #[test]
    fn compute_page_count_normal_without_count_defaults_to_one() {
        let pages = vec![PageInfo {
            page_type: PageType::Normal,
            count: None,
        }];
        assert_eq!(compute_page_count(&pages), 1);
    }

    #[test]
    fn default_ai_config_has_expected_structure() {
        let config = default_ai_config();
        assert_eq!(config.result.model_name, "gpt-5-nano");
        assert!(!config.result.prompt.is_empty());
        assert_eq!(config.result.schema.format.format_type, "json_schema");
        assert_eq!(config.result.schema.format.name, "metadata_extraction");

        // Schema should have "title" as required
        let schema = &config.result.schema.format.schema;
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "title"));
    }

    #[test]
    fn schema_wrapper_serde_roundtrip() {
        let wrapper = SchemaWrapper {
            format: SchemaFormat {
                format_type: "json_schema".to_string(),
                name: "test_schema".to_string(),
                schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" }
                    }
                }),
            },
        };
        let json = serde_json::to_string(&wrapper).unwrap();
        let parsed: SchemaWrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.format.format_type, "json_schema");
        assert_eq!(parsed.format.name, "test_schema");
    }

    #[test]
    fn page_info_cover_omits_count() {
        let page = PageInfo {
            page_type: PageType::Cover,
            count: None,
        };
        let json = serde_json::to_string(&page).unwrap();
        assert!(!json.contains("count"));
    }

    #[test]
    fn page_info_normal_includes_count() {
        let page = PageInfo {
            page_type: PageType::Normal,
            count: Some(3),
        };
        let json = serde_json::to_string(&page).unwrap();
        assert!(json.contains("\"count\":3"));
    }

    #[test]
    fn real_world_config_json_parses() {
        // Simulates JSON from a real collection's AI config
        let json = r#"{
            "result": {
                "model_name": "gpt-5-nano",
                "prompt": "Extract metadata from these thesis pages.",
                "pageInfo": [
                    {"type": "cover"},
                    {"type": "normal", "count": 5}
                ],
                "schema": {
                    "format": {
                        "type": "json_schema",
                        "name": "thesis_metadata",
                        "schema": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "creator": {"type": "array", "items": {"type": "string"}},
                                "date": {"type": "string"},
                                "institution": {"type": "string"},
                                "department": {"type": "string"},
                                "major_professor": {"type": "array", "items": {"type": "string"}}
                            },
                            "required": ["title", "creator"]
                        }
                    }
                }
            }
        }"#;

        let config: IaAiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.result.model_name, "gpt-5-nano");
        assert_eq!(config.result.page_info.len(), 2);
        assert_eq!(config.result.schema.format.name, "thesis_metadata");
        let required = config.result.schema.format.schema["required"]
            .as_array()
            .unwrap();
        assert_eq!(required.len(), 2);
    }

    #[test]
    fn load_ai_config_from_file_valid() {
        let config = default_ai_config();
        let json = serde_json::to_string_pretty(&config).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai_config.json");
        std::fs::write(&path, &json).unwrap();

        let loaded = load_ai_config_from_file(&path).unwrap();
        assert_eq!(loaded.result.model_name, config.result.model_name);
        assert_eq!(loaded.result.page_info.len(), config.result.page_info.len());
    }

    #[test]
    fn load_ai_config_from_file_unwrapped() {
        // Config without the {"result": ...} envelope — just the fields directly
        let config = default_ai_config();
        let inner_json = serde_json::to_string_pretty(&config.result).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai_config_unwrapped.json");
        std::fs::write(&path, &inner_json).unwrap();

        let loaded = load_ai_config_from_file(&path).unwrap();
        assert_eq!(loaded.result.model_name, config.result.model_name);
        assert_eq!(loaded.result.page_info.len(), config.result.page_info.len());
    }

    #[test]
    fn load_ai_config_from_file_missing() {
        let err =
            load_ai_config_from_file(std::path::Path::new("/nonexistent/config.json")).unwrap_err();
        assert!(err.to_string().contains("failed to read AI config"));
    }

    #[test]
    fn load_ai_config_from_file_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not valid json{{{").unwrap();

        let err = load_ai_config_from_file(&path).unwrap_err();
        assert!(err.to_string().contains("expected")); // serde parse error
    }

    /// `fetch_ai_config` bypasses `download::fetch_response` but the AI
    /// config download must still suppress view-counting via `cnt=0`.
    #[tokio::test]
    async fn fetch_ai_config_sends_cnt_zero() {
        use crate::config::IaConfig;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Item metadata: one AI Config JSON file.
        let metadata = serde_json::json!({
            "created": 0,
            "metadata": { "identifier": "ai-coll" },
            "files": [
                { "name": "ai-coll-config.json", "format": "AI Config JSON" }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/metadata/ai-coll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata))
            .mount(&server)
            .await;

        // Download must include cnt=0; otherwise the mock returns 404.
        let config_body = serde_json::json!({
            "result": default_ai_config().result,
        });
        Mock::given(method("GET"))
            .and(path("/download/ai-coll/ai-coll-config.json"))
            .and(query_param("cnt", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(config_body))
            .mount(&server)
            .await;

        let host = server
            .uri()
            .strip_prefix("http://")
            .unwrap_or(&server.uri())
            .to_string();
        let mut config = IaConfig::default();
        config.general.host = host;
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();

        let fetched = fetch_ai_config(&client, "ai-coll")
            .await
            .expect("fetch_ai_config should succeed when cnt=0 is sent");
        assert_eq!(fetched.result.model_name, "gpt-5-nano");
    }
}
