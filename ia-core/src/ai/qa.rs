//! QA pipeline — verifies AI-extracted metadata using a second LLM for
//! two-model consensus. Sends page images + extracted metadata to a QA model
//! and parses structured per-field verdicts with confidence scores.

use serde::{Deserialize, Serialize};

use crate::ai::client::LlmClient;
use crate::ai::extracted_metadata::ExtractedMetadata;
use crate::ai::ia_config::IaAiConfig;
use crate::error::Result;

/// Result of QA verification for a single item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaResult {
    /// The item identifier.
    pub identifier: String,
    /// Overall confidence score (0.0–1.0).
    pub overall_confidence: f64,
    /// Summary verdict for the item.
    pub verdict: QaVerdict,
    /// The model that performed the original extraction.
    pub extraction_model: String,
    /// The model used for QA verification.
    pub qa_model: String,
    /// Per-field QA results (ordered by field name).
    pub fields: indexmap::IndexMap<String, FieldQaResult>,
    /// Token usage for the QA call.
    pub token_usage: Option<crate::ai::types::TokenUsage>,
    /// Wall-clock time for the QA call in milliseconds.
    pub elapsed_ms: u64,
}

/// Overall verdict for an item's extracted metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaVerdict {
    /// All fields confirmed with high confidence.
    Pass,
    /// One or more fields are incorrect.
    Fail,
    /// Some fields are uncertain and need human review.
    NeedsReview,
}

/// QA result for a single metadata field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldQaResult {
    /// The value extracted by the original model.
    pub extracted_value: serde_json::Value,
    /// The QA model's verdict for this field.
    pub verdict: FieldVerdict,
    /// Confidence score for this field (0.0–1.0).
    pub confidence: f64,
    /// Suggested correction if the field is incorrect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_correction: Option<serde_json::Value>,
    /// Additional note from the QA model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Verdict for a single field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldVerdict {
    /// Field value matches the source material.
    Correct,
    /// Field value does not match the source material.
    Incorrect,
    /// Cannot determine correctness from available evidence.
    Uncertain,
}

/// Options controlling QA behavior.
#[derive(Debug, Clone)]
pub struct QaOpts {
    /// The LLM model to use for QA (should differ from extraction model).
    pub model: String,
    /// Sampling temperature for the QA model.
    pub temperature: f64,
    /// Max tokens for the QA response.
    pub max_tokens: u32,
    /// Minimum overall confidence to mark item as Pass.
    pub confidence_threshold: f64,
    /// Minimum per-field confidence.
    pub min_field_confidence: f64,
    /// If true, don't call the LLM — return a placeholder result.
    pub dry_run: bool,
}

impl Default for QaOpts {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-6".to_string(),
            temperature: 0.2,
            max_tokens: 4096,
            confidence_threshold: 0.8,
            min_field_confidence: 0.6,
            dry_run: false,
        }
    }
}

/// QA prompt construction constants.
pub const QA_SYSTEM_PROMPT: &str = "\
You are a metadata QA agent performing two-model consensus verification. Another AI \
model extracted metadata from the page images you are about to see. Your job is to \
independently verify each extracted field against the source images.

## Your Task

For each metadata field, examine the page images carefully and determine whether the \
extracted value is correct, incorrect, or uncertain. You are the second pair of eyes — \
be thorough but fair. Minor formatting differences (e.g., \"1988\" vs \"1988-01-01\") are \
acceptable if the core information is correct.

## Verification Guidelines

- **Titles**: Check title pages, cover pages, and headers. Accept minor punctuation or \
capitalization differences if the words match. Flag truncated or substantially different titles.
- **Dates**: Look for dates on title pages, copyright pages, and colophons. A year-only \
extraction is correct if the full date isn't visible. Flag wrong years or decades.
- **Authors/Creators**: Check title pages and bylines. Accept name format variations \
(e.g., \"J. Smith\" vs \"John Smith\") as correct. Flag misspellings or wrong names.
- **Publishers**: Check title pages and copyright pages. Accept abbreviations.
- **Languages**: Verify by examining the actual text content in the images.
- **Subjects/Topics**: Use your judgment — these may not appear verbatim in the images. \
Mark as \"uncertain\" if you cannot verify from visual evidence alone.
- **Schema-defined fields**: If the expected schema defines allowed values or formats, \
verify the extracted value conforms.

## When to use each verdict

- **\"correct\"**: The extracted value accurately represents what is shown in the images. \
High confidence that the extraction is right.
- **\"incorrect\"**: The extracted value clearly contradicts what is shown in the images. \
You MUST provide a \"suggested_correction\" with the correct value.
- **\"uncertain\"**: The field cannot be verified from the available images (e.g., the \
relevant page wasn't included, or the information isn't visually apparent). Do NOT use \
\"uncertain\" as a hedge when you can see the answer — commit to correct/incorrect.

## Response Format

Respond with a JSON object where keys are field names and values are objects with \
\"verdict\", \"confidence\", \"suggested_correction\" (optional), and \"note\" (optional).

{
  \"field_name\": {
    \"verdict\": \"correct\" | \"incorrect\" | \"uncertain\",
    \"confidence\": 0.0 to 1.0,
    \"suggested_correction\": \"...\",
    \"note\": \"...\"
  }
}

Only include fields that were present in the extracted metadata. Do not invent new fields.";

/// Build the QA user message from page images and extracted metadata.
pub fn build_qa_user_message(extracted: &ExtractedMetadata, config: &IaAiConfig) -> String {
    let metadata_json =
        serde_json::to_string_pretty(&extracted.result.metadata).unwrap_or_default();
    let schema_json =
        serde_json::to_string_pretty(&config.result.schema.format.schema).unwrap_or_default();

    format!(
        "## Extracted Metadata\n\
         ```json\n{metadata_json}\n```\n\n\
         ## Expected Schema\n\
         ```json\n{schema_json}\n```\n\n\
         Please verify each extracted field against the page images shown above."
    )
}

/// Page images for QA, in either pre-downloaded or URL form.
#[derive(Debug)]
pub enum PageImages {
    /// Downloaded image bytes: Vec of (member_path, jpeg_bytes).
    Base64(Vec<(String, Vec<u8>)>),
    /// Image URLs to pass directly to the LLM: Vec of (member_path, url).
    Urls(Vec<(String, String)>),
}

impl PageImages {
    /// Number of page images.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            PageImages::Base64(v) => v.len(),
            PageImages::Urls(v) => v.len(),
        }
    }

    /// Whether there are no page images.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Run QA on a single item.
///
/// Fetches page images, builds the QA prompt with vision support, sends to
/// the QA LLM, and parses the structured response.
pub async fn qa_item(
    llm_client: &LlmClient,
    identifier: &str,
    config: &IaAiConfig,
    extracted: &ExtractedMetadata,
    page_images: &PageImages,
    opts: &QaOpts,
) -> Result<QaResult> {
    use std::time::Instant;

    let start = Instant::now();

    if opts.dry_run {
        return Ok(dry_run_result(identifier, extracted, opts));
    }

    // Build vision messages: page images + text prompt
    let mut content: Vec<crate::ai::client::MessageContent> = match page_images {
        PageImages::Base64(images) => images
            .iter()
            .map(|(_filename, image_data)| {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(image_data);
                crate::ai::client::MessageContent::image_base64("image/jpeg", b64)
            })
            .collect(),
        PageImages::Urls(urls) => urls
            .iter()
            .map(|(_member_path, url)| crate::ai::client::MessageContent::image_url(url))
            .collect(),
    };

    // Add the text prompt
    let user_text = build_qa_user_message(extracted, config);
    content.push(crate::ai::client::MessageContent::text(user_text));

    let response = llm_client.chat_vision(QA_SYSTEM_PROMPT, content).await?;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    // Parse the structured QA response
    let fields = parse_qa_response(&response.content, extracted)?;

    // Compute overall verdict
    let (verdict, overall_confidence) = compute_verdict(&fields, opts);

    Ok(QaResult {
        identifier: identifier.to_string(),
        overall_confidence,
        verdict,
        extraction_model: extracted.result.ai_request_info.model.clone(),
        qa_model: opts.model.clone(),
        fields,
        token_usage: response.token_usage,
        elapsed_ms,
    })
}

/// Parse the LLM's QA response JSON into per-field results.
fn parse_qa_response(
    content: &str,
    extracted: &ExtractedMetadata,
) -> Result<indexmap::IndexMap<String, FieldQaResult>> {
    // Extract JSON from the response, handling:
    // 1. Bare JSON (no fences)
    // 2. ```json ... ``` fences (possibly with preamble text before)
    // 3. ``` ... ``` fences (possibly with preamble text before)
    let json_str = extract_json_block(content);

    let parsed: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json_str)
        .map_err(|e| {
            let preview: String = json_str.chars().take(200).collect();
            crate::error::IaError::LlmApi {
                status: 200,
                message: format!(
                    "failed to parse QA response: {e} — response preview: {preview:?}"
                ),
            }
        })?;

    let mut fields = indexmap::IndexMap::new();

    for (field_name, field_data) in &parsed {
        let extracted_value = extracted
            .result
            .metadata
            .get(field_name)
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let verdict_str = field_data
            .get("verdict")
            .and_then(|v| v.as_str())
            .unwrap_or("uncertain");

        let verdict = match verdict_str {
            "correct" => FieldVerdict::Correct,
            "incorrect" => FieldVerdict::Incorrect,
            _ => FieldVerdict::Uncertain,
        };

        let confidence = field_data
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);

        let suggested_correction = field_data.get("suggested_correction").cloned();
        let note = field_data
            .get("note")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        fields.insert(
            field_name.clone(),
            FieldQaResult {
                extracted_value,
                verdict,
                confidence,
                suggested_correction,
                note,
            },
        );
    }

    Ok(fields)
}

/// Extract a JSON object from an LLM response that may contain preamble text
/// and/or markdown code fences.
fn extract_json_block(content: &str) -> &str {
    let trimmed = content.trim();

    // Try: ```json ... ``` with possible preamble
    if let Some(start) = trimmed.find("```json") {
        let after_fence = start + 7; // len("```json")
        if let Some(end) = trimmed[after_fence..].find("```") {
            return trimmed[after_fence..after_fence + end].trim();
        }
    }

    // Try: ``` ... ``` with possible preamble
    if let Some(start) = trimmed.find("```") {
        let after_fence = start + 3;
        if let Some(end) = trimmed[after_fence..].find("```") {
            return trimmed[after_fence..after_fence + end].trim();
        }
    }

    // Try: find first { and last } (bare JSON)
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return &trimmed[start..=end];
            }
        }
    }

    trimmed
}

/// Compute overall verdict from per-field results.
fn compute_verdict(
    fields: &indexmap::IndexMap<String, FieldQaResult>,
    opts: &QaOpts,
) -> (QaVerdict, f64) {
    if fields.is_empty() {
        return (QaVerdict::NeedsReview, 0.0);
    }

    let total_confidence: f64 = fields.values().map(|f| f.confidence).sum();
    let overall = total_confidence / fields.len() as f64;

    let has_incorrect = fields
        .values()
        .any(|f| f.verdict == FieldVerdict::Incorrect);
    let has_uncertain = fields
        .values()
        .any(|f| f.verdict == FieldVerdict::Uncertain || f.confidence < opts.min_field_confidence);

    let verdict = if has_incorrect {
        QaVerdict::Fail
    } else if has_uncertain || overall < opts.confidence_threshold {
        QaVerdict::NeedsReview
    } else {
        QaVerdict::Pass
    };

    (verdict, overall)
}

/// Generate a dry-run placeholder result.
fn dry_run_result(identifier: &str, extracted: &ExtractedMetadata, opts: &QaOpts) -> QaResult {
    let fields = extracted
        .result
        .metadata
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                FieldQaResult {
                    extracted_value: v.clone(),
                    verdict: FieldVerdict::Uncertain,
                    confidence: 0.0,
                    suggested_correction: None,
                    note: Some("dry run — not verified".to_string()),
                },
            )
        })
        .collect();

    QaResult {
        identifier: identifier.to_string(),
        overall_confidence: 0.0,
        verdict: QaVerdict::NeedsReview,
        extraction_model: extracted.result.ai_request_info.model.clone(),
        qa_model: opts.model.clone(),
        fields,
        token_usage: None,
        elapsed_ms: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qa_system_prompt_contains_key_sections() {
        assert!(QA_SYSTEM_PROMPT.contains("two-model consensus"));
        assert!(QA_SYSTEM_PROMPT.contains("Verification Guidelines"));
        assert!(QA_SYSTEM_PROMPT.contains("Titles"));
        assert!(QA_SYSTEM_PROMPT.contains("Dates"));
        assert!(QA_SYSTEM_PROMPT.contains("Authors"));
        assert!(QA_SYSTEM_PROMPT.contains("When to use each verdict"));
        assert!(QA_SYSTEM_PROMPT.contains("Response Format"));
        assert!(QA_SYSTEM_PROMPT.contains("suggested_correction"));
    }

    #[test]
    fn qa_verdict_serde() {
        assert_eq!(serde_json::to_string(&QaVerdict::Pass).unwrap(), "\"pass\"");
        assert_eq!(serde_json::to_string(&QaVerdict::Fail).unwrap(), "\"fail\"");
        assert_eq!(
            serde_json::to_string(&QaVerdict::NeedsReview).unwrap(),
            "\"needs_review\""
        );
    }

    #[test]
    fn field_verdict_serde() {
        assert_eq!(
            serde_json::to_string(&FieldVerdict::Correct).unwrap(),
            "\"correct\""
        );
        assert_eq!(
            serde_json::to_string(&FieldVerdict::Incorrect).unwrap(),
            "\"incorrect\""
        );
        assert_eq!(
            serde_json::to_string(&FieldVerdict::Uncertain).unwrap(),
            "\"uncertain\""
        );
    }

    #[test]
    fn compute_verdict_all_correct_high_confidence() {
        let mut fields = indexmap::IndexMap::new();
        fields.insert(
            "title".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!("Test Title"),
                verdict: FieldVerdict::Correct,
                confidence: 0.95,
                suggested_correction: None,
                note: None,
            },
        );
        fields.insert(
            "date".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!("2020"),
                verdict: FieldVerdict::Correct,
                confidence: 0.90,
                suggested_correction: None,
                note: None,
            },
        );

        let opts = QaOpts::default();
        let (verdict, confidence) = compute_verdict(&fields, &opts);
        assert_eq!(verdict, QaVerdict::Pass);
        assert!(confidence > 0.8);
    }

    #[test]
    fn compute_verdict_has_incorrect() {
        let mut fields = indexmap::IndexMap::new();
        fields.insert(
            "title".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!("Wrong Title"),
                verdict: FieldVerdict::Incorrect,
                confidence: 0.95,
                suggested_correction: Some(serde_json::json!("Correct Title")),
                note: None,
            },
        );

        let opts = QaOpts::default();
        let (verdict, _) = compute_verdict(&fields, &opts);
        assert_eq!(verdict, QaVerdict::Fail);
    }

    #[test]
    fn compute_verdict_has_uncertain() {
        let mut fields = indexmap::IndexMap::new();
        fields.insert(
            "title".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!("Title"),
                verdict: FieldVerdict::Correct,
                confidence: 0.95,
                suggested_correction: None,
                note: None,
            },
        );
        fields.insert(
            "department".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!("Dept"),
                verdict: FieldVerdict::Uncertain,
                confidence: 0.4,
                suggested_correction: None,
                note: Some("not visible in images".to_string()),
            },
        );

        let opts = QaOpts::default();
        let (verdict, _) = compute_verdict(&fields, &opts);
        assert_eq!(verdict, QaVerdict::NeedsReview);
    }

    #[test]
    fn compute_verdict_empty_fields() {
        let fields = indexmap::IndexMap::new();
        let opts = QaOpts::default();
        let (verdict, confidence) = compute_verdict(&fields, &opts);
        assert_eq!(verdict, QaVerdict::NeedsReview);
        assert_eq!(confidence, 0.0);
    }

    #[test]
    fn parse_qa_response_valid_json() {
        let content = r#"{
            "title": {"verdict": "correct", "confidence": 0.95},
            "date": {"verdict": "incorrect", "confidence": 0.9, "suggested_correction": "1988", "note": "Year shown on cover is 1988, not 1987"}
        }"#;

        let extracted = crate::ai::extracted_metadata::ExtractedMetadata {
            result: crate::ai::extracted_metadata::ExtractedMetadataResult {
                ai_request_info: crate::ai::extracted_metadata::AiRequestInfo {
                    model: "test".to_string(),
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                    input_cost: "0$".to_string(),
                    output_cost: "0$".to_string(),
                    total_cost: "0$".to_string(),
                },
                metadata: {
                    let mut m = serde_json::Map::new();
                    m.insert("title".to_string(), serde_json::json!("Test Book"));
                    m.insert("date".to_string(), serde_json::json!("1987"));
                    m
                },
            },
        };

        let fields = parse_qa_response(content, &extracted).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields["title"].verdict, FieldVerdict::Correct);
        assert_eq!(fields["date"].verdict, FieldVerdict::Incorrect);
        assert_eq!(
            fields["date"].suggested_correction,
            Some(serde_json::json!("1988"))
        );
        assert!(fields["date"].note.is_some());
    }

    #[test]
    fn parse_qa_response_with_code_fences() {
        let content = "```json\n{\"title\": {\"verdict\": \"correct\", \"confidence\": 0.9}}\n```";

        let extracted = crate::ai::extracted_metadata::ExtractedMetadata {
            result: crate::ai::extracted_metadata::ExtractedMetadataResult {
                ai_request_info: crate::ai::extracted_metadata::AiRequestInfo {
                    model: "test".to_string(),
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                    input_cost: "0$".to_string(),
                    output_cost: "0$".to_string(),
                    total_cost: "0$".to_string(),
                },
                metadata: {
                    let mut m = serde_json::Map::new();
                    m.insert("title".to_string(), serde_json::json!("Test"));
                    m
                },
            },
        };

        let fields = parse_qa_response(content, &extracted).unwrap();
        assert_eq!(fields["title"].verdict, FieldVerdict::Correct);
    }

    #[test]
    fn qa_result_serde_roundtrip() {
        let mut fields = indexmap::IndexMap::new();
        fields.insert(
            "title".to_string(),
            FieldQaResult {
                extracted_value: serde_json::json!("Test"),
                verdict: FieldVerdict::Correct,
                confidence: 0.95,
                suggested_correction: None,
                note: None,
            },
        );

        let result = QaResult {
            identifier: "test-item".to_string(),
            overall_confidence: 0.95,
            verdict: QaVerdict::Pass,
            extraction_model: "gpt-5-nano".to_string(),
            qa_model: "claude-sonnet-4-6".to_string(),
            fields,
            token_usage: None,
            elapsed_ms: 1500,
        };

        let json = serde_json::to_string_pretty(&result).unwrap();
        let parsed: QaResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.identifier, "test-item");
        assert_eq!(parsed.verdict, QaVerdict::Pass);
        assert_eq!(parsed.fields.len(), 1);
    }

    #[test]
    fn build_qa_user_message_includes_metadata_and_schema() {
        let extracted = crate::ai::extracted_metadata::ExtractedMetadata {
            result: crate::ai::extracted_metadata::ExtractedMetadataResult {
                ai_request_info: crate::ai::extracted_metadata::AiRequestInfo {
                    model: "test".to_string(),
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                    input_cost: "0$".to_string(),
                    output_cost: "0$".to_string(),
                    total_cost: "0$".to_string(),
                },
                metadata: {
                    let mut m = serde_json::Map::new();
                    m.insert("title".to_string(), serde_json::json!("My Book"));
                    m
                },
            },
        };

        let config = crate::ai::ia_config::default_ai_config();
        let msg = build_qa_user_message(&extracted, &config);
        assert!(msg.contains("My Book"));
        assert!(msg.contains("Extracted Metadata"));
        assert!(msg.contains("Expected Schema"));
    }

    #[test]
    fn page_images_url_mode_produces_url_content() {
        let urls = PageImages::Urls(vec![
            (
                "page_0000.jp2".to_string(),
                "https://example.com/page0.jpg".to_string(),
            ),
            (
                "page_0001.jp2".to_string(),
                "https://example.com/page1.jpg".to_string(),
            ),
        ]);

        if let PageImages::Urls(ref url_vec) = urls {
            let content: Vec<crate::ai::client::MessageContent> = url_vec
                .iter()
                .map(|(_, url)| crate::ai::client::MessageContent::image_url(url))
                .collect();

            assert_eq!(content.len(), 2);
            let json = content[0].to_openai_json();
            assert_eq!(json["type"], "image_url");
            assert_eq!(json["image_url"]["url"], "https://example.com/page0.jpg");
            assert!(!json["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:"));
        }

        assert_eq!(urls.len(), 2);
        assert!(!urls.is_empty());
    }

    #[test]
    fn page_images_base64_mode_produces_data_uri_content() {
        let images =
            PageImages::Base64(vec![("page_0000.jp2".to_string(), vec![0xFF, 0xD8, 0xFF])]);

        if let PageImages::Base64(ref img_vec) = images {
            use base64::Engine;
            let content: Vec<crate::ai::client::MessageContent> = img_vec
                .iter()
                .map(|(_, data)| {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
                    crate::ai::client::MessageContent::image_base64("image/jpeg", b64)
                })
                .collect();

            assert_eq!(content.len(), 1);
            let json = content[0].to_openai_json();
            assert!(json["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,"));
        }

        assert_eq!(images.len(), 1);
    }
}
