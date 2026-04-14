use serde::{Deserialize, Serialize};

/// A single suggested change to one metadata field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataChange {
    /// The metadata field name (e.g. "title", "date", "subject").
    pub field: String,
    /// The current value (None if field is missing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_value: Option<serde_json::Value>,
    /// The suggested new value.
    pub new_value: serde_json::Value,
    /// Human-readable explanation of why this change is suggested.
    pub reason: String,
    /// What category of cleanup this change represents.
    pub category: ChangeCategory,
    /// Whether the user has accepted/rejected this change.
    #[serde(default)]
    pub status: ChangeStatus,
}

/// Categories of metadata cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeCategory {
    /// Schema conformance (e.g. normalizing date format, language codes).
    Schema,
    /// Content improvement (e.g. fixing typos, capitalization).
    Content,
    /// Cross-field inference (e.g. extracting date from title).
    CrossField,
    /// Filling in a missing required/recommended field.
    MissingField,
}

/// The review status of a suggested change.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    /// Not yet reviewed.
    #[default]
    Pending,
    /// User accepted the suggestion as-is.
    Accepted,
    /// User rejected the suggestion.
    Rejected,
    /// User edited the suggestion to a custom value.
    Edited(serde_json::Value),
}

/// All suggestions for one item, produced by the analyzer stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemAnalysis {
    /// The item identifier.
    pub identifier: String,
    /// The original metadata (for display in the TUI).
    pub metadata: serde_json::Value,
    /// Suggested changes from the LLM.
    pub changes: Vec<MetadataChange>,
    /// Token usage for this analysis (None for local LLMs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenUsage>,
    /// When the analysis was performed.
    pub analyzed_at: String,
}

/// Token usage and cost tracking for a single LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Tokens in the prompt.
    pub prompt_tokens: u64,
    /// Tokens in the completion.
    pub completion_tokens: u64,
    /// Estimated cost in USD (None if pricing unknown).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_estimate_usd: Option<f64>,
}

/// Result of applying changes to an item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyResult {
    /// The item identifier.
    pub identifier: String,
    /// The changes that were applied.
    pub changes_applied: Vec<MetadataChange>,
    /// Overall status of the apply operation.
    pub status: ApplyStatus,
    /// Error message if the apply failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Status of applying changes to an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    /// All changes applied successfully.
    Ok,
    /// No changes to apply (all rejected or no suggestions).
    Skipped,
    /// Apply failed with an error.
    Error,
    /// Dry-run mode, nothing actually applied.
    DryRun,
}

/// Focus configuration: which categories and fields to analyze.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FocusConfig {
    // Category flags (when all false, all categories are included)
    pub dates: bool,
    pub titles: bool,
    pub descriptions: bool,
    pub missing_fields: bool,
    pub schema_fix: bool,
    pub typos: bool,

    /// Only suggest changes to these fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only_fields: Option<Vec<String>>,
    /// Never suggest changes to these fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_fields: Option<Vec<String>>,

    /// Path to a custom prompt file to append to the system prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<std::path::PathBuf>,
    /// Complete system prompt override (bypasses all built-in rules).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_override: Option<String>,
}

impl FocusConfig {
    /// Returns true if no category flags are set (meaning all categories apply).
    pub fn is_unfocused(&self) -> bool {
        !self.dates
            && !self.titles
            && !self.descriptions
            && !self.missing_fields
            && !self.schema_fix
            && !self.typos
    }
}

/// LLM API provider — determines wire format and auth headers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// OpenAI-compatible `/chat/completions` format (also used by ollama, vLLM,
    /// OpenRouter, LiteLLM, etc.).
    #[default]
    OpenAi,
    /// Anthropic Messages API (`/v1/messages`) with native auth headers.
    Anthropic,
}

impl Provider {
    /// Auto-detect provider from a base URL.
    ///
    /// Returns `Anthropic` if the URL contains `anthropic.com`, otherwise `OpenAi`.
    pub fn detect(base_url: &str) -> Self {
        if base_url.contains("anthropic.com") {
            Self::Anthropic
        } else {
            Self::OpenAi
        }
    }

    /// Build the full API endpoint URL from a base URL.
    ///
    /// Normalizes the base URL (strips trailing slashes) and appends the
    /// provider-specific path if not already present.
    ///
    /// # Examples
    ///
    /// ```
    /// use ia_core::ai::types::Provider;
    ///
    /// // OpenAI
    /// assert_eq!(
    ///     Provider::OpenAi.endpoint_url("https://api.openai.com/v1"),
    ///     "https://api.openai.com/v1/chat/completions"
    /// );
    /// assert_eq!(
    ///     Provider::OpenAi.endpoint_url("https://api.openai.com"),
    ///     "https://api.openai.com/v1/chat/completions"
    /// );
    ///
    /// // Anthropic
    /// assert_eq!(
    ///     Provider::Anthropic.endpoint_url("https://api.anthropic.com"),
    ///     "https://api.anthropic.com/v1/messages"
    /// );
    /// assert_eq!(
    ///     Provider::Anthropic.endpoint_url("https://api.anthropic.com/v1"),
    ///     "https://api.anthropic.com/v1/messages"
    /// );
    /// ```
    pub fn endpoint_url(&self, base_url: &str) -> String {
        let url = base_url.trim_end_matches('/');
        match self {
            Self::OpenAi => {
                if url.ends_with("/chat/completions") {
                    url.to_string()
                } else if url.ends_with("/v1") {
                    format!("{url}/chat/completions")
                } else {
                    format!("{url}/v1/chat/completions")
                }
            }
            Self::Anthropic => {
                if url.ends_with("/messages") {
                    url.to_string()
                } else if url.ends_with("/v1") {
                    format!("{url}/messages")
                } else {
                    format!("{url}/v1/messages")
                }
            }
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenAi => write!(f, "openai"),
            Self::Anthropic => write!(f, "anthropic"),
        }
    }
}

impl std::str::FromStr for Provider {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "openai" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            _ => Err(format!(
                "unknown provider '{s}': expected 'openai' or 'anthropic'"
            )),
        }
    }
}

/// LLM API configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    /// Base URL for the LLM API.
    pub base_url: String,
    /// API key for authentication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Model name to use.
    pub model: String,
    /// Sampling temperature (0.0 - 2.0).
    pub temperature: f64,
    /// Max tokens in the response.
    pub max_tokens: u64,
    /// API provider (determines wire format and auth headers).
    #[serde(default)]
    pub provider: Provider,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: None,
            model: "gpt-4o-mini".to_string(),
            temperature: 0.2,
            max_tokens: 4096,
            provider: Provider::OpenAi,
        }
    }
}

/// Joblog-specific change record (simplified for serialization).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoblogChange {
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<serde_json::Value>,
    pub new: serde_json::Value,
}

/// Joblog-specific token usage record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoblogTokens {
    pub prompt: u64,
    pub completion: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_status_default_is_pending() {
        assert_eq!(ChangeStatus::default(), ChangeStatus::Pending);
    }

    #[test]
    fn ai_config_default_values() {
        let config = AiConfig::default();
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.model, "gpt-4o-mini");
        assert_eq!(config.temperature, 0.2);
        assert_eq!(config.max_tokens, 4096);
        assert!(config.api_key.is_none());
        assert_eq!(config.provider, Provider::OpenAi);
    }

    #[test]
    fn provider_detect_from_url() {
        assert_eq!(
            Provider::detect("https://api.anthropic.com"),
            Provider::Anthropic
        );
        assert_eq!(
            Provider::detect("https://api.anthropic.com/v1"),
            Provider::Anthropic
        );
        assert_eq!(
            Provider::detect("https://api.openai.com/v1"),
            Provider::OpenAi
        );
        assert_eq!(
            Provider::detect("http://localhost:11434/v1"),
            Provider::OpenAi
        );
        assert_eq!(
            Provider::detect("https://openrouter.ai/api/v1"),
            Provider::OpenAi
        );
    }

    #[test]
    fn provider_endpoint_url_openai() {
        assert_eq!(
            Provider::OpenAi.endpoint_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            Provider::OpenAi.endpoint_url("https://api.openai.com"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            Provider::OpenAi.endpoint_url("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            Provider::OpenAi.endpoint_url("http://localhost:11434/v1"),
            "http://localhost:11434/v1/chat/completions"
        );
        // Already has full path
        assert_eq!(
            Provider::OpenAi.endpoint_url("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn provider_endpoint_url_anthropic() {
        assert_eq!(
            Provider::Anthropic.endpoint_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            Provider::Anthropic.endpoint_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            Provider::Anthropic.endpoint_url("https://api.anthropic.com/v1/"),
            "https://api.anthropic.com/v1/messages"
        );
        // Already has full path
        assert_eq!(
            Provider::Anthropic.endpoint_url("https://api.anthropic.com/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn provider_from_str() {
        assert_eq!("openai".parse::<Provider>().unwrap(), Provider::OpenAi);
        assert_eq!(
            "anthropic".parse::<Provider>().unwrap(),
            Provider::Anthropic
        );
        assert_eq!("OpenAI".parse::<Provider>().unwrap(), Provider::OpenAi);
        assert_eq!(
            "ANTHROPIC".parse::<Provider>().unwrap(),
            Provider::Anthropic
        );
        assert!("unknown".parse::<Provider>().is_err());
    }

    #[test]
    fn provider_display() {
        assert_eq!(Provider::OpenAi.to_string(), "openai");
        assert_eq!(Provider::Anthropic.to_string(), "anthropic");
    }

    #[test]
    fn provider_serde_roundtrip() {
        let openai = Provider::OpenAi;
        let json = serde_json::to_string(&openai).unwrap();
        assert_eq!(json, "\"openai\"");
        let parsed: Provider = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Provider::OpenAi);

        let anthropic = Provider::Anthropic;
        let json = serde_json::to_string(&anthropic).unwrap();
        assert_eq!(json, "\"anthropic\"");
        let parsed: Provider = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Provider::Anthropic);
    }

    #[test]
    fn focus_config_default_is_unfocused() {
        let focus = FocusConfig::default();
        assert!(focus.is_unfocused());
    }

    #[test]
    fn focus_config_with_dates_is_focused() {
        let focus = FocusConfig {
            dates: true,
            ..Default::default()
        };
        assert!(!focus.is_unfocused());
    }

    #[test]
    fn metadata_change_serde_roundtrip() {
        let change = MetadataChange {
            field: "title".to_string(),
            old_value: Some(serde_json::json!("old title")),
            new_value: serde_json::json!("New Title"),
            reason: "Fixed capitalization".to_string(),
            category: ChangeCategory::Content,
            status: ChangeStatus::Pending,
        };
        let json = serde_json::to_string(&change).unwrap();
        let parsed: MetadataChange = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.field, "title");
        assert_eq!(parsed.category, ChangeCategory::Content);
        assert_eq!(parsed.status, ChangeStatus::Pending);
    }

    #[test]
    fn metadata_change_missing_old_value() {
        let change = MetadataChange {
            field: "date".to_string(),
            old_value: None,
            new_value: serde_json::json!("1969-07-20"),
            reason: "Extracted from title".to_string(),
            category: ChangeCategory::CrossField,
            status: ChangeStatus::Accepted,
        };
        let json = serde_json::to_string(&change).unwrap();
        assert!(!json.contains("old_value"));
        let parsed: MetadataChange = serde_json::from_str(&json).unwrap();
        assert!(parsed.old_value.is_none());
    }

    #[test]
    fn change_status_edited_roundtrip() {
        let status = ChangeStatus::Edited(serde_json::json!("custom value"));
        let json = serde_json::to_string(&status).unwrap();
        let parsed: ChangeStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed,
            ChangeStatus::Edited(serde_json::json!("custom value"))
        );
    }

    #[test]
    fn item_analysis_serde_roundtrip() {
        let analysis = ItemAnalysis {
            identifier: "nasa_photo".to_string(),
            metadata: serde_json::json!({"title": "test"}),
            changes: vec![MetadataChange {
                field: "title".to_string(),
                old_value: Some(serde_json::json!("test")),
                new_value: serde_json::json!("Test"),
                reason: "Capitalization".to_string(),
                category: ChangeCategory::Content,
                status: ChangeStatus::Pending,
            }],
            token_usage: Some(TokenUsage {
                prompt_tokens: 1200,
                completion_tokens: 300,
                cost_estimate_usd: Some(0.003),
            }),
            analyzed_at: "2026-02-23T10:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&analysis).unwrap();
        let parsed: ItemAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.identifier, "nasa_photo");
        assert_eq!(parsed.changes.len(), 1);
        assert!(parsed.token_usage.is_some());
    }

    #[test]
    fn item_analysis_without_tokens() {
        let analysis = ItemAnalysis {
            identifier: "test".to_string(),
            metadata: serde_json::json!({}),
            changes: vec![],
            token_usage: None,
            analyzed_at: "2026-02-23T10:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&analysis).unwrap();
        assert!(!json.contains("token_usage"));
    }

    #[test]
    fn apply_result_ok() {
        let result = ApplyResult {
            identifier: "nasa".to_string(),
            changes_applied: vec![],
            status: ApplyStatus::Ok,
            error: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"status\":\"ok\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn apply_result_error() {
        let result = ApplyResult {
            identifier: "bad_item".to_string(),
            changes_applied: vec![],
            status: ApplyStatus::Error,
            error: Some("403 Forbidden".to_string()),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"status\":\"error\""));
        assert!(json.contains("403 Forbidden"));
    }

    #[test]
    fn change_category_serde() {
        for (cat, expected) in [
            (ChangeCategory::Schema, "\"schema\""),
            (ChangeCategory::Content, "\"content\""),
            (ChangeCategory::CrossField, "\"cross_field\""),
            (ChangeCategory::MissingField, "\"missing_field\""),
        ] {
            let json = serde_json::to_string(&cat).unwrap();
            assert_eq!(json, expected);
            let parsed: ChangeCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, cat);
        }
    }

    #[test]
    fn apply_status_serde() {
        for (status, expected) in [
            (ApplyStatus::Ok, "\"ok\""),
            (ApplyStatus::Skipped, "\"skipped\""),
            (ApplyStatus::Error, "\"error\""),
            (ApplyStatus::DryRun, "\"dry_run\""),
        ] {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, expected);
            let parsed: ApplyStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn joblog_change_serde() {
        let change = JoblogChange {
            field: "date".to_string(),
            old: None,
            new: serde_json::json!("1969-07-20"),
        };
        let json = serde_json::to_string(&change).unwrap();
        assert!(!json.contains("\"old\""));
        let parsed: JoblogChange = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.field, "date");
        assert!(parsed.old.is_none());
    }

    #[test]
    fn joblog_tokens_serde() {
        let tokens = JoblogTokens {
            prompt: 1200,
            completion: 300,
        };
        let json = serde_json::to_string(&tokens).unwrap();
        let parsed: JoblogTokens = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.prompt, 1200);
        assert_eq!(parsed.completion, 300);
    }

    #[test]
    fn focus_config_serde_roundtrip() {
        let focus = FocusConfig {
            dates: true,
            titles: false,
            descriptions: false,
            missing_fields: true,
            schema_fix: false,
            typos: false,
            only_fields: Some(vec!["date".to_string(), "title".to_string()]),
            exclude_fields: None,
            prompt_file: None,
            system_prompt_override: None,
        };
        let json = serde_json::to_string(&focus).unwrap();
        let parsed: FocusConfig = serde_json::from_str(&json).unwrap();
        assert!(parsed.dates);
        assert!(!parsed.titles);
        assert!(parsed.missing_fields);
        assert_eq!(parsed.only_fields.unwrap().len(), 2);
        assert!(parsed.exclude_fields.is_none());
    }
}
