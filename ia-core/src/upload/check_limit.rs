/// Parse a check_limit JSON response. Returns `true` if over limit.
///
/// The IA API returns `over_limit` as either a number (`0`) or a string
/// (`"0"`) depending on the endpoint version. We parse as `serde_json::Value`
/// and coerce both forms. Conservative: returns `true` (overloaded) on any
/// parse error or missing field.
pub(crate) fn parse_check_limit_response(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        tracing::warn!("check_limit: failed to parse response body as JSON");
        return true; // conservative: treat as overloaded
    };

    match value.get("over_limit") {
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(1) != 0,
        Some(serde_json::Value::String(s)) => s.parse::<i64>().unwrap_or(1) != 0,
        Some(_) => {
            tracing::warn!("check_limit: over_limit has unexpected type");
            true
        }
        None => true, // field missing — conservative
    }
}

/// Check if a 503 response body indicates spam detection.
#[allow(dead_code)] // used once upload loop handles 503 responses
pub(crate) fn is_spam_response(body: &str) -> bool {
    body.contains("appears to be spam")
}

/// Status reported during rate limit polling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitStatus {
    /// Sending check_limit request.
    Polling,
    /// Waiting before next poll.
    Waiting { seconds: u64 },
    /// Rate limit cleared, resuming uploads.
    Cleared,
    /// Retries exhausted.
    Exhausted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_check_limit_clear() {
        let json = r#"{"bucket":"test","accesskey":"xxx","over_limit":0,"detail":"ok"}"#;
        assert!(!parse_check_limit_response(json));
    }

    #[test]
    fn parse_check_limit_over() {
        let json = r#"{"bucket":"test","accesskey":"xxx","over_limit":1,"detail":"slow"}"#;
        assert!(parse_check_limit_response(json));
    }

    #[test]
    fn parse_check_limit_malformed_json() {
        assert!(parse_check_limit_response("not json"));
    }

    #[test]
    fn parse_check_limit_missing_field() {
        let json = r#"{"bucket":"test"}"#;
        assert!(parse_check_limit_response(json));
    }

    #[test]
    fn parse_check_limit_string_zero_clear() {
        // IA API sometimes returns over_limit as a string "0"
        let json = r#"{"bucket":"test","accesskey":"xxx","over_limit":"0","detail":"ok"}"#;
        assert!(!parse_check_limit_response(json));
    }

    #[test]
    fn parse_check_limit_string_one_over() {
        let json = r#"{"bucket":"test","accesskey":"xxx","over_limit":"1","detail":"slow"}"#;
        assert!(parse_check_limit_response(json));
    }

    #[test]
    fn parse_check_limit_string_non_numeric() {
        // Non-numeric string — conservative: treat as overloaded
        let json = r#"{"bucket":"test","over_limit":"yes"}"#;
        assert!(parse_check_limit_response(json));
    }

    #[test]
    fn is_spam_response_detects_spam() {
        assert!(is_spam_response("Your upload appears to be spam."));
        assert!(is_spam_response("blah blah appears to be spam blah"));
    }

    #[test]
    fn is_spam_response_normal_503() {
        assert!(!is_spam_response("Please reduce your request rate."));
        assert!(!is_spam_response(""));
    }
}
