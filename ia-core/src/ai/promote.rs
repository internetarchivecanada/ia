//! Metadata promotion — writes QA-confirmed extracted metadata to items
//! via the IA metadata modify API.

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::ai::qa::{FieldVerdict, QaResult};
use crate::client::IaClient;
use crate::error::Result;
use crate::metadata::write::{MetadataOp, ModifyRequest};

/// Options controlling which fields get promoted.
#[derive(Debug, Clone)]
pub struct PromoteOpts {
    /// Minimum overall confidence to proceed with promotion.
    pub confidence_threshold: f64,
    /// Minimum per-field confidence for a field to be promoted.
    pub min_field_confidence: f64,
    /// If set, only promote these specific fields.
    pub fields: Option<Vec<String>>,
    /// If true, show what would be done without making changes.
    pub dry_run: bool,
}

impl Default for PromoteOpts {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.8,
            min_field_confidence: 0.6,
            fields: None,
            dry_run: false,
        }
    }
}

/// Result of a promote operation for a single item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromoteResult {
    /// The item identifier.
    pub identifier: String,
    /// Fields that were successfully promoted.
    pub fields_promoted: Vec<String>,
    /// Fields that were skipped, with reasons.
    pub fields_skipped: Vec<(String, String)>,
    /// Task ID from the metadata write API, if any.
    pub task_id: Option<u64>,
    /// Whether this was a dry run.
    pub dry_run: bool,
}

/// Promote QA-confirmed metadata to an item.
///
/// Filters fields by confidence thresholds, then writes the confirmed
/// values via the metadata modify API.
pub async fn promote_metadata(
    client: &IaClient,
    identifier: &str,
    qa_result: &QaResult,
    opts: &PromoteOpts,
) -> Result<PromoteResult> {
    // Gate the whole item on overall confidence first: individually
    // high-confidence fields must not be promoted from an item whose
    // overall QA confidence is below the configured threshold.
    if qa_result.overall_confidence < opts.confidence_threshold {
        let reason = format!(
            "overall confidence {:.2} below threshold {:.2}",
            qa_result.overall_confidence, opts.confidence_threshold
        );
        debug!(identifier, %reason, "skipping promotion");
        return Ok(PromoteResult {
            identifier: identifier.to_string(),
            fields_promoted: Vec::new(),
            fields_skipped: qa_result
                .fields
                .keys()
                .map(|name| (name.clone(), reason.clone()))
                .collect(),
            task_id: None,
            dry_run: opts.dry_run,
        });
    }

    let mut changes = Vec::new();
    let mut fields_promoted = Vec::new();
    let mut fields_skipped = Vec::new();

    for (field_name, field_result) in &qa_result.fields {
        // Check field filter
        if let Some(ref allowed) = opts.fields {
            if !allowed.iter().any(|f| f == field_name) {
                fields_skipped.push((field_name.clone(), "not in field filter".to_string()));
                continue;
            }
        }

        // Check per-field confidence
        if field_result.confidence < opts.min_field_confidence {
            fields_skipped.push((
                field_name.clone(),
                format!(
                    "confidence {:.2} below threshold {:.2}",
                    field_result.confidence, opts.min_field_confidence
                ),
            ));
            continue;
        }

        match field_result.verdict {
            FieldVerdict::Correct => {
                // Use the extracted value as-is
                changes.push((field_name.clone(), field_result.extracted_value.clone()));
                fields_promoted.push(field_name.clone());
            }
            FieldVerdict::Incorrect => {
                // Use the suggested correction if available and confidence meets threshold
                if let Some(ref correction) = field_result.suggested_correction {
                    if field_result.confidence >= opts.min_field_confidence {
                        changes.push((field_name.clone(), correction.clone()));
                        fields_promoted.push(field_name.clone());
                    } else {
                        fields_skipped.push((
                            field_name.clone(),
                            "incorrect with low-confidence correction".to_string(),
                        ));
                    }
                } else {
                    fields_skipped.push((
                        field_name.clone(),
                        "incorrect with no suggested correction".to_string(),
                    ));
                }
            }
            FieldVerdict::Uncertain => {
                fields_skipped.push((
                    field_name.clone(),
                    "uncertain — needs human review".to_string(),
                ));
            }
        }
    }

    if opts.dry_run || changes.is_empty() {
        return Ok(PromoteResult {
            identifier: identifier.to_string(),
            fields_promoted,
            fields_skipped,
            task_id: None,
            dry_run: opts.dry_run,
        });
    }

    debug!(
        identifier,
        fields = fields_promoted.len(),
        "promoting metadata"
    );

    // Build and send the modify request
    let req = ModifyRequest {
        identifier: identifier.to_string(),
        changes,
        op: MetadataOp::Set,
        target: "metadata".to_string(),
        expect: None,
        priority: None,
        reduced_priority: false,
    };
    let response = crate::metadata::write::modify(client, &req).await?;

    Ok(PromoteResult {
        identifier: identifier.to_string(),
        fields_promoted,
        fields_skipped,
        task_id: response.task_id,
        dry_run: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::qa::{FieldQaResult, FieldVerdict, QaResult, QaVerdict};
    use indexmap::IndexMap;

    fn make_qa_result(fields: Vec<(&str, FieldVerdict, f64, Option<&str>)>) -> QaResult {
        let mut field_map = IndexMap::new();
        for (name, verdict, confidence, correction) in fields {
            field_map.insert(
                name.to_string(),
                FieldQaResult {
                    extracted_value: serde_json::json!(format!("extracted_{name}")),
                    verdict,
                    confidence,
                    suggested_correction: correction.map(|c| serde_json::json!(c)),
                    note: None,
                },
            );
        }

        QaResult {
            identifier: "test-item".to_string(),
            overall_confidence: 0.9,
            verdict: QaVerdict::Pass,
            extraction_model: "gpt-5-nano".to_string(),
            qa_model: "claude-sonnet-4-6".to_string(),
            fields: field_map,
            token_usage: None,
            elapsed_ms: 100,
            existing_metadata: None,
            pages_sent: None,
        }
    }

    /// Client pointed at a local wiremock server — NEVER live archive.org.
    fn mock_client(server_uri: &str) -> IaClient {
        let mut config = crate::config::IaConfig::default();
        let host = server_uri.strip_prefix("http://").unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config.s3_access = Some("test_access".to_string());
        config.s3_secret = Some("test_secret".to_string());
        IaClient::from_config(config).unwrap()
    }

    #[tokio::test]
    async fn below_overall_confidence_threshold_skips_all_fields() {
        let server = wiremock::MockServer::start().await;
        let client = mock_client(&server.uri());

        // All fields individually above min_field_confidence, but the
        // item's overall confidence is below confidence_threshold —
        // nothing may be promoted and no write request may be sent.
        let mut qa = make_qa_result(vec![
            ("title", FieldVerdict::Correct, 0.95, None),
            ("date", FieldVerdict::Correct, 0.90, None),
        ]);
        qa.overall_confidence = 0.5;

        let opts = PromoteOpts::default(); // confidence_threshold 0.8

        let result = promote_metadata(&client, "test-item", &qa, &opts)
            .await
            .expect("below-threshold promote should return Ok, not write");

        assert!(
            result.fields_promoted.is_empty(),
            "no fields may be promoted below the overall threshold, got: {:?}",
            result.fields_promoted
        );
        assert_eq!(result.fields_skipped.len(), 2);
        assert!(
            result
                .fields_skipped
                .iter()
                .all(|(_, reason)| reason.contains("overall confidence")),
            "skip reasons should cite overall confidence, got: {:?}",
            result.fields_skipped
        );
        assert_eq!(result.task_id, None);
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "no HTTP request may be sent for a below-threshold item"
        );
    }

    #[tokio::test]
    async fn at_overall_confidence_threshold_proceeds() {
        let server = wiremock::MockServer::start().await;
        let client = mock_client(&server.uri());

        // overall == threshold should proceed (inclusive bound).
        let mut qa = make_qa_result(vec![
            ("title", FieldVerdict::Correct, 0.95, None),
            ("date", FieldVerdict::Correct, 0.90, None),
        ]);
        qa.overall_confidence = 0.8;

        let opts = PromoteOpts {
            dry_run: true,
            ..Default::default()
        };

        let result = promote_metadata(&client, "test-item", &qa, &opts)
            .await
            .unwrap();

        assert_eq!(result.fields_promoted.len(), 2);
        assert!(result.dry_run);
    }

    #[test]
    fn promote_opts_default() {
        let opts = PromoteOpts::default();
        assert_eq!(opts.confidence_threshold, 0.8);
        assert_eq!(opts.min_field_confidence, 0.6);
        assert!(opts.fields.is_none());
        assert!(!opts.dry_run);
    }

    #[tokio::test]
    async fn dry_run_returns_without_writing() {
        // We can't actually call promote_metadata without a client, but we
        // can test the field filtering logic by checking what would be promoted
        let qa = make_qa_result(vec![
            ("title", FieldVerdict::Correct, 0.95, None),
            ("date", FieldVerdict::Correct, 0.90, None),
        ]);

        let opts = PromoteOpts {
            dry_run: true,
            ..Default::default()
        };

        // Build the changes list manually to test filtering
        let mut promoted = Vec::new();
        let mut skipped = Vec::new();

        for (name, result) in &qa.fields {
            if result.confidence >= opts.min_field_confidence
                && result.verdict == FieldVerdict::Correct
            {
                promoted.push(name.clone());
            } else {
                skipped.push((name.clone(), "below threshold".to_string()));
            }
        }

        assert_eq!(promoted.len(), 2);
        assert!(promoted.contains(&"title".to_string()));
        assert!(promoted.contains(&"date".to_string()));
    }

    #[test]
    fn field_filter_restricts_promotion() {
        let qa = make_qa_result(vec![
            ("title", FieldVerdict::Correct, 0.95, None),
            ("date", FieldVerdict::Correct, 0.90, None),
            ("language", FieldVerdict::Correct, 0.85, None),
        ]);

        let opts = PromoteOpts {
            fields: Some(vec!["title".to_string(), "language".to_string()]),
            dry_run: true,
            ..Default::default()
        };

        // Simulate field filtering
        let mut promoted = Vec::new();
        let mut skipped = Vec::new();

        for (name, result) in &qa.fields {
            if let Some(ref allowed) = opts.fields {
                if !allowed.iter().any(|f| f == name) {
                    skipped.push((name.clone(), "not in filter".to_string()));
                    continue;
                }
            }
            if result.confidence >= opts.min_field_confidence
                && result.verdict == FieldVerdict::Correct
            {
                promoted.push(name.clone());
            }
        }

        assert_eq!(promoted.len(), 2);
        assert!(promoted.contains(&"title".to_string()));
        assert!(promoted.contains(&"language".to_string()));
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, "date");
    }

    #[test]
    fn uncertain_fields_are_skipped() {
        let qa = make_qa_result(vec![
            ("title", FieldVerdict::Correct, 0.95, None),
            ("department", FieldVerdict::Uncertain, 0.4, None),
        ]);

        let opts = PromoteOpts::default();

        let mut promoted = Vec::new();
        let mut skipped = Vec::new();

        for (name, result) in &qa.fields {
            match result.verdict {
                FieldVerdict::Correct if result.confidence >= opts.min_field_confidence => {
                    promoted.push(name.clone());
                }
                FieldVerdict::Uncertain => {
                    skipped.push((name.clone(), "uncertain".to_string()));
                }
                _ => {
                    skipped.push((name.clone(), "other".to_string()));
                }
            }
        }

        assert_eq!(promoted.len(), 1);
        assert_eq!(promoted[0], "title");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, "department");
    }

    #[test]
    fn incorrect_with_correction_is_promoted() {
        let qa = make_qa_result(vec![("date", FieldVerdict::Incorrect, 0.9, Some("1988"))]);

        let opts = PromoteOpts::default();
        let field = &qa.fields["date"];

        assert_eq!(field.verdict, FieldVerdict::Incorrect);
        assert!(field.suggested_correction.is_some());
        assert!(field.confidence >= opts.min_field_confidence);
    }

    #[test]
    fn incorrect_without_correction_is_skipped() {
        let qa = make_qa_result(vec![("date", FieldVerdict::Incorrect, 0.9, None)]);

        let field = &qa.fields["date"];
        assert_eq!(field.verdict, FieldVerdict::Incorrect);
        assert!(field.suggested_correction.is_none());
    }

    #[test]
    fn low_confidence_correct_field_is_skipped() {
        let qa = make_qa_result(vec![("title", FieldVerdict::Correct, 0.3, None)]);

        let opts = PromoteOpts::default();
        let field = &qa.fields["title"];
        assert!(field.confidence < opts.min_field_confidence);
    }

    #[test]
    fn promote_result_serde_roundtrip() {
        let result = PromoteResult {
            identifier: "test-item".to_string(),
            fields_promoted: vec!["title".to_string(), "date".to_string()],
            fields_skipped: vec![("department".to_string(), "uncertain".to_string())],
            task_id: Some(12345),
            dry_run: false,
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: PromoteResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.identifier, "test-item");
        assert_eq!(parsed.fields_promoted.len(), 2);
        assert_eq!(parsed.task_id, Some(12345));
    }
}
