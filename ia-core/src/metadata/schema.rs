use serde::{Deserialize, Serialize};

use crate::client::IaClient;
use crate::error::IaError;

/// A single field definition from the IA metadata schema.
///
/// The Internet Archive stores metadata field definitions in the
/// `ia-metadata` item. Each field has properties like whether it's
/// required, repeatable, internal-only, and who can edit it.
///
/// The JSON keys from the API use spaces (e.g. "internal use only"),
/// which are renamed to snake_case for Rust. Serialization outputs
/// snake_case keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaField {
    /// Machine name (e.g. "title", "creator")
    pub field: String,
    /// Human-readable label (e.g. "Title", "Creator/Author")
    pub label: String,
    /// Whether required: "Yes", "No", "Recommended", or "Deprecated"
    pub required: String,
    /// Whether the field accepts multiple values: "Yes" or "No"
    pub repeatable: String,
    /// Whether this is an internal-only field: "Yes" or "No"
    #[serde(
        rename(deserialize = "internal use only"),
        alias = "internal_use_only"
    )]
    pub internal_use_only: String,
    /// Who defines this field: "uploader", "IA admin", "IA software", "user admin"
    #[serde(rename(deserialize = "defined by"), alias = "defined_by")]
    pub defined_by: String,
    /// Who can edit: "uploader", "IA admin", "IA software", "user admin", "not editable"
    #[serde(rename(deserialize = "edit access"), alias = "edit_access")]
    pub edit_access: String,
    /// Description of the field
    #[serde(default)]
    pub definition: String,
    /// What values are accepted
    #[serde(
        rename(deserialize = "accepted values"),
        alias = "accepted_values",
        default
    )]
    pub accepted_values: String,
    /// Additional usage guidance
    #[serde(
        rename(deserialize = "usage notes"),
        alias = "usage_notes",
        default
    )]
    pub usage_notes: String,
    /// Example values
    #[serde(default)]
    pub example: Vec<String>,
}

/// The complete schema data from the `ia-metadata` item.
///
/// Contains two arrays: `metadata_schema` for item-level field
/// definitions and `files_schema` for file-level field definitions.
#[derive(Debug, Deserialize)]
pub struct SchemaData {
    /// Item-level metadata field definitions
    pub metadata_schema: Vec<SchemaField>,
    /// File-level metadata field definitions
    pub files_schema: Vec<SchemaField>,
}

/// Fetch the metadata schema from the ia-metadata item on archive.org.
///
/// Downloads and parses `ia-metadata_schema.json` which contains both
/// item-level (`metadata_schema`) and file-level (`files_schema`) field
/// definitions.
pub async fn fetch_schema(client: &IaClient) -> crate::Result<SchemaData> {
    let url = client.url("/download/ia-metadata/ia-metadata_schema.json");
    let resp = client.http().get(&url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(IaError::Http {
            status: status.as_u16(),
            message: format!("failed to fetch metadata schema: {status}"),
        });
    }
    let body = resp
        .text()
        .await
        .map_err(reqwest_middleware::Error::from)?;
    let data: SchemaData = serde_json::from_str(&body)?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_schema_json() -> &'static str {
        r#"{
            "metadata_schema": [
                {
                    "field": "title",
                    "label": "Title",
                    "required": "Recommended",
                    "repeatable": "No",
                    "internal use only": "No",
                    "defined by": "uploader",
                    "edit access": "uploader",
                    "definition": "Title of media",
                    "accepted values": "String, plain text",
                    "usage notes": "All alphabets supported",
                    "example": ["San Francisco (1955)"]
                },
                {
                    "field": "scanner",
                    "label": "Scanner",
                    "required": "No",
                    "repeatable": "No",
                    "internal use only": "Yes",
                    "defined by": "IA software",
                    "edit access": "IA admin",
                    "definition": "Scanner used to digitize",
                    "accepted values": "String"
                }
            ],
            "files_schema": [
                {
                    "field": "name",
                    "label": "File Name",
                    "required": "Yes",
                    "repeatable": "No",
                    "internal use only": "No",
                    "defined by": "uploader",
                    "edit access": "not editable",
                    "definition": "Name of the file"
                }
            ]
        }"#
    }

    #[test]
    fn deserialize_schema_data() {
        let data: SchemaData = serde_json::from_str(sample_schema_json()).unwrap();
        assert_eq!(data.metadata_schema.len(), 2);
        assert_eq!(data.files_schema.len(), 1);
    }

    #[test]
    fn schema_field_has_all_properties() {
        let data: SchemaData = serde_json::from_str(sample_schema_json()).unwrap();
        let title = &data.metadata_schema[0];
        assert_eq!(title.field, "title");
        assert_eq!(title.label, "Title");
        assert_eq!(title.required, "Recommended");
        assert_eq!(title.repeatable, "No");
        assert_eq!(title.internal_use_only, "No");
        assert_eq!(title.defined_by, "uploader");
        assert_eq!(title.edit_access, "uploader");
        assert_eq!(title.definition, "Title of media");
        assert_eq!(title.accepted_values, "String, plain text");
        assert_eq!(title.usage_notes, "All alphabets supported");
        assert_eq!(title.example, vec!["San Francisco (1955)"]);
    }

    #[test]
    fn missing_optional_fields_default_empty() {
        let data: SchemaData = serde_json::from_str(sample_schema_json()).unwrap();
        let scanner = &data.metadata_schema[1];
        assert_eq!(scanner.usage_notes, "");
        assert!(scanner.example.is_empty());
    }

    #[test]
    fn schema_field_serializes_to_json() {
        let data: SchemaData = serde_json::from_str(sample_schema_json()).unwrap();
        let json = serde_json::to_value(&data.metadata_schema[0]).unwrap();
        assert_eq!(json["field"], "title");
        assert_eq!(json["label"], "Title");
        // Verify serde rename works for output
        assert_eq!(json["internal_use_only"], "No");
        assert_eq!(json["defined_by"], "uploader");
        assert_eq!(json["edit_access"], "uploader");
        assert_eq!(json["accepted_values"], "String, plain text");
        assert_eq!(json["usage_notes"], "All alphabets supported");
    }

    // -- fetch_schema integration tests --

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config
    }

    #[tokio::test]
    async fn fetch_schema_returns_both_schemas() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/download/ia-metadata/ia-metadata_schema.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(sample_schema_json()),
            )
            .mount(&mock_server)
            .await;

        let client =
            crate::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let data = fetch_schema(&client).await.unwrap();
        assert_eq!(data.metadata_schema.len(), 2);
        assert_eq!(data.files_schema.len(), 1);
    }

    #[tokio::test]
    async fn fetch_schema_returns_error_on_404() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/download/ia-metadata/ia-metadata_schema.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let client =
            crate::IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = fetch_schema(&client).await;
        assert!(result.is_err());
    }
}
