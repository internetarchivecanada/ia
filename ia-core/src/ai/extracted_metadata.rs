//! Extracted Metadata JSON — stored per-item, contains metadata extracted by
//! the AI Metadata Extractor derive module plus provenance information.
//!
//! Format on archive.org: "Extracted Metadata JSON"

use serde::{Deserialize, Serialize};

use crate::error::{IaError, Result};
use crate::types::ItemMetadata;

/// Top-level Extracted Metadata JSON envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMetadata {
    pub result: ExtractedMetadataResult,
}

/// The content of an Extracted Metadata JSON file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMetadataResult {
    /// Provenance: model, tokens, and cost information.
    #[serde(rename = "_ai_request_info")]
    pub ai_request_info: AiRequestInfo,
    /// The extracted metadata fields (key-value pairs).
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

/// Provenance information about the AI extraction request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRequestInfo {
    /// The LLM model used for extraction.
    pub model: String,
    /// Number of input tokens consumed.
    pub input_tokens: u64,
    /// Number of output tokens generated.
    pub output_tokens: u64,
    /// Total tokens (input + output).
    pub total_tokens: u64,
    /// Cost of input tokens (e.g., "0.00083150$").
    pub input_cost: String,
    /// Cost of output tokens.
    pub output_cost: String,
    /// Total cost of the request.
    pub total_cost: String,
}

/// Extract the extracted metadata from an already-fetched `ItemMetadata`.
///
/// The `/metadata/{id}` API response includes `extracted_metadata` as a
/// top-level field (with the same shape as `ExtractedMetadataResult`).
/// This avoids a separate HTTP request to download the derivative file.
pub fn extract_from_item(item: &ItemMetadata) -> Result<ExtractedMetadata> {
    let identifier = item
        .metadata
        .identifier
        .as_ref()
        .map(|v| v.first())
        .unwrap_or("<unknown>");
    let value = item
        .extra
        .get("extracted_metadata")
        .ok_or_else(|| IaError::NotFound(format!("no extracted metadata for {identifier}")))?;
    let result: ExtractedMetadataResult = serde_json::from_value(value.clone())?;
    Ok(ExtractedMetadata { result })
}

/// Check if an item has extracted metadata.
pub fn has_extracted_metadata(item: &ItemMetadata) -> bool {
    item.extra.contains_key("extracted_metadata")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn extracted_metadata_serde_roundtrip() {
        let json = r#"{
            "result": {
                "_ai_request_info": {
                    "model": "gpt-5-nano",
                    "input_tokens": 1500,
                    "output_tokens": 200,
                    "total_tokens": 1700,
                    "input_cost": "0.00083150$",
                    "output_cost": "0.00012000$",
                    "total_cost": "0.00095150$"
                },
                "metadata": {
                    "title": "Untersuchungen zur Diagnostik",
                    "creator": ["Leutenegger-Aster, M."],
                    "date": "1987",
                    "language": "German",
                    "institution": "Universität Zürich",
                    "department": "Veterinär-Medizinische Fakultät"
                }
            }
        }"#;

        let parsed: ExtractedMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.result.ai_request_info.model, "gpt-5-nano");
        assert_eq!(parsed.result.ai_request_info.input_tokens, 1500);
        assert_eq!(parsed.result.ai_request_info.total_cost, "0.00095150$");
        assert_eq!(
            parsed.result.metadata.get("title").unwrap(),
            "Untersuchungen zur Diagnostik"
        );
        assert_eq!(parsed.result.metadata.len(), 6);

        // Roundtrip
        let serialized = serde_json::to_string_pretty(&parsed).unwrap();
        let reparsed: ExtractedMetadata = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            reparsed.result.ai_request_info.model,
            parsed.result.ai_request_info.model
        );
    }

    #[test]
    fn ai_request_info_serde() {
        let info = AiRequestInfo {
            model: "gpt-5-nano".to_string(),
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
            input_cost: "0.001$".to_string(),
            output_cost: "0.0005$".to_string(),
            total_cost: "0.0015$".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"model\":\"gpt-5-nano\""));
        let parsed: AiRequestInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.total_tokens, 1500);
    }

    fn make_item_with_extra(
        identifier: &str,
        extra: HashMap<String, serde_json::Value>,
    ) -> ItemMetadata {
        ItemMetadata {
            metadata: crate::types::MetadataFields {
                identifier: Some(crate::types::MetadataValue::Single(identifier.to_string())),
                ..Default::default()
            },
            files: vec![],
            server: None,
            d1: None,
            d2: None,
            dir: None,
            files_count: None,
            item_size: None,
            is_dark: false,
            extra,
        }
    }

    #[test]
    fn has_extracted_metadata_present() {
        let mut extra = HashMap::new();
        extra.insert(
            "extracted_metadata".to_string(),
            serde_json::json!({"_ai_request_info": {}, "metadata": {}}),
        );
        let item = make_item_with_extra("test-item", extra);
        assert!(has_extracted_metadata(&item));
    }

    #[test]
    fn has_extracted_metadata_missing() {
        let item = make_item_with_extra("test-item", HashMap::new());
        assert!(!has_extracted_metadata(&item));
    }

    #[test]
    fn extract_from_item_success() {
        let mut extra = HashMap::new();
        extra.insert(
            "extracted_metadata".to_string(),
            serde_json::json!({
                "_ai_request_info": {
                    "model": "gpt-5-nano-2025-08-07",
                    "input_tokens": 18478,
                    "output_tokens": 3031,
                    "total_tokens": 21509,
                    "input_cost": "0.00092390$",
                    "output_cost": "0.00121240$",
                    "total_cost": "0.00213630$"
                },
                "metadata": {
                    "title": "Essays & Addresses On The Philosophy Of Religion",
                    "creator": ["Friedrich Von Hügel"],
                    "language": ["English"],
                    "publisher": "J. M. Dent & Sons Limited"
                }
            }),
        );
        let item = make_item_with_extra("test-item", extra);
        let extracted = extract_from_item(&item).unwrap();
        assert_eq!(
            extracted.result.ai_request_info.model,
            "gpt-5-nano-2025-08-07"
        );
        assert_eq!(extracted.result.ai_request_info.total_tokens, 21509);
        assert_eq!(
            extracted.result.metadata.get("title").unwrap(),
            "Essays & Addresses On The Philosophy Of Religion"
        );
        assert_eq!(extracted.result.metadata.len(), 4);
    }

    #[test]
    fn extract_from_item_missing() {
        let item = make_item_with_extra("test-item", HashMap::new());
        let err = extract_from_item(&item).unwrap_err();
        assert!(err.to_string().contains("no extracted metadata"));
    }

    #[test]
    fn extract_from_item_roundtrip_from_api_json() {
        // Simulate what happens when the full /metadata/{id} response is deserialized
        // into ItemMetadata and then extracted_metadata is pulled from .extra
        let api_json = r#"{
            "metadata": {"identifier": "test-item"},
            "files": [],
            "extracted_metadata": {
                "_ai_request_info": {
                    "model": "gpt-5-nano",
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "total_tokens": 150,
                    "input_cost": "0$",
                    "output_cost": "0$",
                    "total_cost": "0$"
                },
                "metadata": {
                    "title": "Test Book",
                    "creator": ["Author One"]
                }
            }
        }"#;
        let item: ItemMetadata = serde_json::from_str(api_json).unwrap();
        let extracted = extract_from_item(&item).unwrap();
        assert_eq!(extracted.result.ai_request_info.model, "gpt-5-nano");
        assert_eq!(extracted.result.metadata.get("title").unwrap(), "Test Book");
    }

    #[test]
    fn metadata_with_array_values() {
        let json = r#"{
            "result": {
                "_ai_request_info": {
                    "model": "gpt-5-nano",
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "total_tokens": 150,
                    "input_cost": "0$",
                    "output_cost": "0$",
                    "total_cost": "0$"
                },
                "metadata": {
                    "title": "Test Book",
                    "creator": ["Author One", "Author Two"],
                    "subject": ["science", "technology"]
                }
            }
        }"#;

        let parsed: ExtractedMetadata = serde_json::from_str(json).unwrap();
        let creators = parsed.result.metadata.get("creator").unwrap();
        assert!(creators.is_array());
        assert_eq!(creators.as_array().unwrap().len(), 2);
    }
}
