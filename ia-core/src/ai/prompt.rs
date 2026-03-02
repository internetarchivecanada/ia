use crate::ai::types::FocusConfig;
use crate::error::{IaError, Result};

/// Build the system prompt for the LLM, filtered by focus configuration.
///
/// If `focus.system_prompt_override` is set, returns that directly.
/// If `focus.prompt_file` is set, its contents are appended as a
/// "## Custom Rules" section.
/// Otherwise builds a structured prompt with role, output format, schema rules,
/// cleanup rules (filtered by FocusConfig), and constraints.
///
/// Returns an error if `prompt_file` is set but cannot be read.
pub fn build_system_prompt(focus: &FocusConfig) -> Result<String> {
    if let Some(ref override_prompt) = focus.system_prompt_override {
        return Ok(override_prompt.clone());
    }

    let mut sections = Vec::new();

    sections.push(ROLE.to_string());
    sections.push(OUTPUT_FORMAT.to_string());
    sections.push(SCHEMA_RULES.to_string());

    // Cleanup rules: include all when unfocused, or filter by flags
    let mut cleanup_rules = Vec::new();
    if focus.is_unfocused() || focus.dates {
        cleanup_rules.push(RULES_DATES);
    }
    if focus.is_unfocused() || focus.titles {
        cleanup_rules.push(RULES_TITLES);
    }
    if focus.is_unfocused() || focus.descriptions {
        cleanup_rules.push(RULES_DESCRIPTIONS);
    }
    if focus.is_unfocused() || focus.missing_fields {
        cleanup_rules.push(RULES_MISSING_FIELDS);
    }
    if focus.is_unfocused() || focus.schema_fix {
        cleanup_rules.push(RULES_SCHEMA_FIX);
    }
    if focus.is_unfocused() || focus.typos {
        cleanup_rules.push(RULES_TYPOS);
    }

    if !cleanup_rules.is_empty() {
        sections.push(format!(
            "## Cleanup Rules\n\nApply the following cleanup rules:\n\n{}",
            cleanup_rules.join("\n\n")
        ));
    }

    // Field restrictions
    if let Some(ref only) = focus.only_fields {
        sections.push(format!(
            "## Field Restrictions\n\nOnly suggest changes to these fields: {}",
            only.join(", ")
        ));
    }
    if let Some(ref exclude) = focus.exclude_fields {
        sections.push(format!(
            "## Field Exclusions\n\nNever suggest changes to these fields: {}",
            exclude.join(", ")
        ));
    }

    // Custom prompt file
    if let Some(ref path) = focus.prompt_file {
        let content = std::fs::read_to_string(path).map_err(|e| {
            IaError::Config(format!(
                "failed to read prompt file '{}': {}",
                path.display(),
                e
            ))
        })?;
        sections.push(format!("## Custom Rules\n\n{}", content.trim()));
    }

    sections.push(CONSTRAINTS.to_string());

    Ok(sections.join("\n\n"))
}

/// Build the user message containing the item metadata for the LLM to analyze.
pub fn build_user_message(metadata: &serde_json::Value) -> String {
    format!(
        "Analyze this Internet Archive item metadata and suggest improvements:\n\n```json\n{}\n```",
        serde_json::to_string_pretty(metadata).unwrap_or_else(|_| metadata.to_string())
    )
}

const ROLE: &str = "\
You are an Internet Archive metadata specialist. Your job is to analyze item metadata \
and suggest improvements to make items more discoverable, accurate, and standards-compliant.\n\
\n\
You are conservative and precise. You never invent information — you only infer from \
the available context (title, description, collection, file names, etc.).";

const OUTPUT_FORMAT: &str = "\
## Output Format\n\
\n\
Respond with a JSON array of suggested changes. Each change is an object with:\n\
\n\
```json\n\
[\n\
  {\n\
    \"field\": \"field_name\",\n\
    \"old_value\": \"current value or null if missing\",\n\
    \"new_value\": \"suggested new value\",\n\
    \"reason\": \"Brief explanation of why this change improves the metadata\",\n\
    \"category\": \"schema | content | cross_field | missing_field\"\n\
  }\n\
]\n\
```\n\
\n\
If no changes are needed, return an empty array: `[]`\n\
\n\
Categories:\n\
- `schema`: Fixing format to match IA standards (dates, language codes, etc.)\n\
- `content`: Improving existing content (typos, capitalization, etc.)\n\
- `cross_field`: Inferring one field from another (e.g., date from title)\n\
- `missing_field`: Filling in an empty required/recommended field";

const SCHEMA_RULES: &str = "\
## Internet Archive Schema Rules\n\
\n\
- **Dates**: ISO 8601 format (YYYY-MM-DD, YYYY-MM, or YYYY). Never use other formats.\n\
- **Language codes**: ISO 639-2/B three-letter codes (e.g., \"eng\", \"fra\", \"deu\").\n\
- **Mediatypes**: One of: texts, etree, audio, movies, software, image, data, web, collection, account.\n\
- **Subjects**: Semicolon-separated tags. Use title case. Avoid overly generic tags.\n\
- **Required fields**: identifier, mediatype, collection.\n\
- **Recommended fields**: title, description, date, creator, subject, language.";

const RULES_DATES: &str = "\
### Date Cleanup\n\
- Extract dates from titles when present (e.g., \"Photo from 1969-07-20\" → date: \"1969-07-20\")\n\
- Normalize non-standard date formats to ISO 8601 (e.g., \"July 20, 1969\" → \"1969-07-20\")\n\
- If only a year is known, use just the year (e.g., \"1969\")\n\
- Never guess dates that aren't supported by context";

const RULES_TITLES: &str = "\
### Title Cleanup\n\
- Fix capitalization: use title case, not ALL CAPS or all lowercase\n\
- Remove redundant date info if the date is also in the date field\n\
- Remove file extensions from titles (e.g., \"document.pdf\" → \"Document\")\n\
- Trim excessive whitespace and leading/trailing punctuation";

const RULES_DESCRIPTIONS: &str = "\
### Description Cleanup\n\
- Generate concise descriptions from available context (title, collection, creator, file names)\n\
- Fix HTML encoding issues (e.g., \"&amp;\" → \"&\")\n\
- Don't generate long descriptions — 1-3 sentences is ideal\n\
- Never fabricate information not supported by the metadata";

const RULES_MISSING_FIELDS: &str = "\
### Missing Field Suggestions\n\
- Suggest values for empty recommended fields when inferable from context\n\
- Suggest subjects/tags based on title, description, and collection\n\
- Suggest language based on title/description text\n\
- Only suggest fields where you have reasonable confidence";

const RULES_SCHEMA_FIX: &str = "\
### Schema Conformance\n\
- Fix language codes to ISO 639-2/B (e.g., \"English\" → \"eng\", \"en\" → \"eng\")\n\
- Fix date formats to ISO 8601\n\
- Normalize subject separators to semicolons\n\
- Fix mediatype values to valid IA mediatypes";

const RULES_TYPOS: &str = "\
### Typo Fixes\n\
- Fix obvious spelling errors in titles, descriptions, and other text fields\n\
- Fix common OCR errors (e.g., \"rn\" → \"m\", \"0\" ↔ \"O\")\n\
- Only fix clear typos — don't change intentional unusual spellings or names";

const CONSTRAINTS: &str = "\
## Constraints\n\
\n\
- **Never modify**: identifier, mediatype (these are system fields)\n\
- **Never remove** valid data — only add or improve\n\
- **Be conservative**: when in doubt, don't suggest the change\n\
- **One change per field**: if you want to change a field, include a single entry for it\n\
- **Preserve meaning**: never change the semantic meaning of a field's content\n\
- **No hallucination**: every suggestion must be directly supported by available metadata";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_focus_includes_all_rules() {
        let focus = FocusConfig::default();
        let prompt = build_system_prompt(&focus).unwrap();
        assert!(prompt.contains("Date Cleanup"));
        assert!(prompt.contains("Title Cleanup"));
        assert!(prompt.contains("Description Cleanup"));
        assert!(prompt.contains("Missing Field Suggestions"));
        assert!(prompt.contains("Schema Conformance"));
        assert!(prompt.contains("Typo Fixes"));
        assert!(prompt.contains("Constraints"));
        assert!(prompt.contains("Output Format"));
        assert!(prompt.contains("metadata specialist"));
    }

    #[test]
    fn dates_only_focus() {
        let focus = FocusConfig {
            dates: true,
            ..Default::default()
        };
        let prompt = build_system_prompt(&focus).unwrap();
        assert!(prompt.contains("Date Cleanup"));
        assert!(!prompt.contains("Title Cleanup"));
        assert!(!prompt.contains("Typo Fixes"));
        // Should still have role, output format, schema rules, constraints
        assert!(prompt.contains("metadata specialist"));
        assert!(prompt.contains("Output Format"));
        assert!(prompt.contains("Constraints"));
    }

    #[test]
    fn titles_only_focus() {
        let focus = FocusConfig {
            titles: true,
            ..Default::default()
        };
        let prompt = build_system_prompt(&focus).unwrap();
        assert!(prompt.contains("Title Cleanup"));
        assert!(!prompt.contains("Date Cleanup"));
        assert!(!prompt.contains("Typo Fixes"));
    }

    #[test]
    fn multiple_focus_flags() {
        let focus = FocusConfig {
            dates: true,
            typos: true,
            ..Default::default()
        };
        let prompt = build_system_prompt(&focus).unwrap();
        assert!(prompt.contains("Date Cleanup"));
        assert!(prompt.contains("Typo Fixes"));
        assert!(!prompt.contains("Title Cleanup"));
        assert!(!prompt.contains("Description Cleanup"));
    }

    #[test]
    fn system_prompt_override() {
        let focus = FocusConfig {
            system_prompt_override: Some("Custom prompt here.".to_string()),
            dates: true, // Should be ignored
            ..Default::default()
        };
        let prompt = build_system_prompt(&focus).unwrap();
        assert_eq!(prompt, "Custom prompt here.");
    }

    #[test]
    fn custom_prompt_file_appended() {
        let dir = tempfile::tempdir().unwrap();
        let prompt_path = dir.path().join("custom.txt");
        std::fs::write(&prompt_path, "Always prioritize NASA collections.\nBe extra thorough with dates.").unwrap();

        let focus = FocusConfig {
            prompt_file: Some(prompt_path),
            ..Default::default()
        };
        let prompt = build_system_prompt(&focus).unwrap();
        assert!(prompt.contains("Custom Rules"));
        assert!(prompt.contains("Always prioritize NASA collections."));
        assert!(prompt.contains("Be extra thorough with dates."));
        // Constraints should come after custom rules
        assert!(prompt.contains("Constraints"));
    }

    #[test]
    fn missing_prompt_file_returns_error() {
        let focus = FocusConfig {
            prompt_file: Some(std::path::PathBuf::from("/nonexistent/path/prompt.txt")),
            ..Default::default()
        };
        let result = build_system_prompt(&focus);
        assert!(result.is_err());
    }

    #[test]
    fn field_restrictions_only_fields() {
        let focus = FocusConfig {
            only_fields: Some(vec!["date".to_string(), "title".to_string()]),
            ..Default::default()
        };
        let prompt = build_system_prompt(&focus).unwrap();
        assert!(prompt.contains("Only suggest changes to these fields: date, title"));
    }

    #[test]
    fn field_restrictions_exclude_fields() {
        let focus = FocusConfig {
            exclude_fields: Some(vec!["description".to_string()]),
            ..Default::default()
        };
        let prompt = build_system_prompt(&focus).unwrap();
        assert!(prompt.contains("Never suggest changes to these fields: description"));
    }

    #[test]
    fn build_user_message_contains_metadata() {
        let metadata = serde_json::json!({
            "title": "Test Item",
            "date": "2026-01-01",
            "collection": "test_collection"
        });
        let message = build_user_message(&metadata);
        assert!(message.contains("Test Item"));
        assert!(message.contains("2026-01-01"));
        assert!(message.contains("test_collection"));
        assert!(message.contains("```json"));
    }

    #[test]
    fn schema_rules_always_included() {
        let focus = FocusConfig {
            typos: true,
            ..Default::default()
        };
        let prompt = build_system_prompt(&focus).unwrap();
        assert!(prompt.contains("Internet Archive Schema Rules"));
        assert!(prompt.contains("ISO 8601"));
        assert!(prompt.contains("ISO 639-2/B"));
    }

    #[test]
    fn output_format_always_included() {
        let focus = FocusConfig {
            dates: true,
            ..Default::default()
        };
        let prompt = build_system_prompt(&focus).unwrap();
        assert!(prompt.contains("Output Format"));
        assert!(prompt.contains("\"field\""));
        assert!(prompt.contains("\"category\""));
    }
}
