use std::path::Path;

use crate::error::IaError;

/// Extract an identifier from a line, handling both plain text and JSONL formats.
///
/// Supports:
///   - Plain identifier: `my-item-id`
///   - JSONL from `ia search --json`: `{"identifier": "my-item-id", ...}`
///   - Empty lines and comment lines (starting with `#`): returns `None`
///
/// For JSON objects without an `"identifier"` field, returns the raw line.
/// For malformed JSON (starts with `{` but fails to parse), returns the raw line.
pub fn parse_identifier_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    // Try to parse as JSON if it looks like a JSON object
    if trimmed.starts_with('{') {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(id) = obj.get("identifier").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }

    Some(trimmed.to_string())
}

/// Validate an IA identifier.
///
/// Rules: 3-100 chars, `[a-zA-Z0-9._-@]`, must start with alphanumeric or `@`.
///
/// # Examples
///
/// ```
/// use ia_core::identifier::validate_identifier;
///
/// assert!(validate_identifier("nasa").is_ok());
/// assert!(validate_identifier("@username").is_ok());
/// assert!(validate_identifier("ab").is_err()); // too short
/// assert!(validate_identifier("has space").is_err()); // invalid char
/// ```
pub fn validate_identifier(id: &str) -> Result<(), IaError> {
    if id.is_empty() || id.len() < 3 {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "must be at least 3 characters".into(),
        });
    }
    if id.len() > 100 {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "must be at most 100 characters".into(),
        });
    }

    let Some(first) = id.chars().next() else {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "identifier is empty".into(),
        });
    };
    if !first.is_ascii_alphanumeric() && first != '@' {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: format!("must start with alphanumeric or '@', got '{first}'"),
        });
    }

    if let Some(bad) = id
        .chars()
        .find(|c| !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' | '@'))
    {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: format!("contains invalid character '{bad}'"),
        });
    }

    Ok(())
}

/// Sanitize a string into a valid IA identifier component.
///
/// - Lowercases ASCII alphanumeric characters
/// - Replaces non-allowed characters with `-`
/// - Strips leading non-alphanumeric characters
/// - Returns empty string if result is less than 3 chars
/// - Truncates to 100 chars
pub fn sanitize_identifier(s: &str) -> String {
    let sanitized: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Ensure starts with alphanumeric
    let sanitized = sanitized
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string();
    if sanitized.len() < 3 {
        String::new() // too short, leave for user to fill in
    } else if sanitized.len() > 100 {
        sanitized[..100].to_string()
    } else {
        sanitized
    }
}

/// Generate an identifier for a file based on naming options.
///
/// Derives an identifier from the file's stem or parent directory name,
/// optionally prepending a prefix. Returns empty string if neither mode
/// is enabled or the sanitized result is too short.
pub fn generate_identifier(
    path: &Path,
    prefix: Option<&str>,
    from_filename: bool,
    from_dirname: bool,
) -> String {
    let raw = if from_filename {
        // Use filename without extension
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    } else if from_dirname {
        // Use parent directory name
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        return String::new();
    };

    let sanitized = sanitize_identifier(&raw);
    if sanitized.is_empty() {
        return String::new();
    }

    match prefix {
        Some(prefix) => format!("{prefix}-{sanitized}"),
        None => sanitized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_identifiers() {
        assert!(validate_identifier("nasa").is_ok());
        assert!(validate_identifier("my-item-123").is_ok());
        assert!(validate_identifier("test.item").is_ok());
        assert!(validate_identifier("a_b_c").is_ok());
        assert!(validate_identifier("abc").is_ok());
        assert!(validate_identifier("@username").is_ok());
    }

    #[test]
    fn invalid_identifier_too_short() {
        assert!(validate_identifier("ab").is_err());
        assert!(validate_identifier("").is_err());
    }

    #[test]
    fn invalid_identifier_too_long() {
        let long = "a".repeat(101);
        assert!(validate_identifier(&long).is_err());
    }

    #[test]
    fn invalid_identifier_bad_chars() {
        assert!(validate_identifier("has space").is_err());
        assert!(validate_identifier("has!bang").is_err());
        assert!(validate_identifier("has#hash").is_err());
    }

    #[test]
    fn invalid_identifier_bad_start() {
        assert!(validate_identifier(".dotstart").is_err());
        assert!(validate_identifier("_understart").is_err());
        assert!(validate_identifier("-dashstart").is_err());
    }

    #[test]
    fn valid_identifier_at_max_length() {
        let exactly_100 = "a".repeat(100);
        assert!(validate_identifier(&exactly_100).is_ok());
    }

    // --- sanitize_identifier tests ---

    #[test]
    fn sanitize_simple() {
        assert_eq!(sanitize_identifier("Hello World"), "hello-world");
    }

    #[test]
    fn sanitize_strips_leading_non_alnum() {
        assert_eq!(sanitize_identifier("--my-file"), "my-file");
    }

    #[test]
    fn sanitize_too_short() {
        assert_eq!(sanitize_identifier("ab"), "");
    }

    #[test]
    fn sanitize_truncates_long() {
        let long = "a".repeat(150);
        assert_eq!(sanitize_identifier(&long).len(), 100);
    }

    #[test]
    fn sanitize_preserves_dots_and_underscores() {
        assert_eq!(sanitize_identifier("my_file.v2"), "my_file.v2");
    }

    #[test]
    fn sanitize_empty_input() {
        assert_eq!(sanitize_identifier(""), "");
    }

    #[test]
    fn sanitize_all_special_chars() {
        assert_eq!(sanitize_identifier("@#$"), "");
    }

    // --- generate_identifier tests ---

    #[test]
    fn generate_from_filename() {
        let path = Path::new("/tmp/My Document.pdf");
        assert_eq!(generate_identifier(path, None, true, false), "my-document");
    }

    #[test]
    fn generate_from_dirname() {
        let path = Path::new("/tmp/My Collection/file.txt");
        assert_eq!(
            generate_identifier(path, None, false, true),
            "my-collection"
        );
    }

    #[test]
    fn generate_with_prefix() {
        let path = Path::new("/tmp/file.txt");
        assert_eq!(
            generate_identifier(path, Some("myproject"), true, false),
            "myproject-file"
        );
    }

    #[test]
    fn generate_neither_mode_returns_empty() {
        let path = Path::new("/tmp/file.txt");
        assert_eq!(generate_identifier(path, None, false, false), "");
    }

    #[test]
    fn generate_prefix_without_mode_returns_empty() {
        let path = Path::new("/tmp/file.txt");
        assert_eq!(
            generate_identifier(path, Some("myproject"), false, false),
            ""
        );
    }

    #[test]
    fn generate_short_name_returns_empty() {
        let path = Path::new("/tmp/ab.txt");
        assert_eq!(generate_identifier(path, None, true, false), "");
    }

    #[test]
    fn generate_root_path_dirname_returns_empty() {
        let path = Path::new("/file.txt");
        assert_eq!(generate_identifier(path, None, false, true), "");
    }

    // --- parse_identifier_line tests ---

    #[test]
    fn parse_plain_identifier() {
        assert_eq!(parse_identifier_line("nasa"), Some("nasa".to_string()));
    }

    #[test]
    fn parse_plain_identifier_with_whitespace() {
        assert_eq!(parse_identifier_line("  nasa  "), Some("nasa".to_string()));
    }

    #[test]
    fn parse_jsonl_identifier() {
        assert_eq!(
            parse_identifier_line(r#"{"identifier": "cubanc_000418"}"#),
            Some("cubanc_000418".to_string())
        );
    }

    #[test]
    fn parse_jsonl_with_extra_fields() {
        assert_eq!(
            parse_identifier_line(
                r#"{"identifier": "nasa", "title": "NASA Images", "mediatype": "image"}"#
            ),
            Some("nasa".to_string())
        );
    }

    #[test]
    fn parse_empty_and_comment_lines() {
        assert_eq!(parse_identifier_line(""), None);
        assert_eq!(parse_identifier_line("  "), None);
        assert_eq!(parse_identifier_line("# comment"), None);
    }

    #[test]
    fn parse_json_without_identifier_field() {
        assert_eq!(
            parse_identifier_line(r#"{"title": "something"}"#),
            Some(r#"{"title": "something"}"#.to_string())
        );
    }

    #[test]
    fn parse_malformed_json() {
        // Malformed JSON falls back to raw line
        assert_eq!(
            parse_identifier_line(r#"{"identifier": "broken"#),
            Some(r#"{"identifier": "broken"#.to_string())
        );
    }

    // --- coherence test ---

    #[test]
    fn sanitize_output_passes_validate_or_is_empty() {
        let inputs = [
            "Hello World",
            "test",
            "a b c d e",
            "@#$",
            "ab",
            "",
            "valid-id",
        ];
        for input in inputs {
            let sanitized = sanitize_identifier(input);
            if !sanitized.is_empty() {
                assert!(
                    validate_identifier(&sanitized).is_ok(),
                    "sanitize_identifier({input:?}) = {sanitized:?} should pass validation"
                );
            }
        }
    }
}
