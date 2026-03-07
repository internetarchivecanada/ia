use serde::Deserialize;

/// Parsed check_limit API response.
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // used once upload loop calls check_limit
struct CheckLimitResponse {
    over_limit: Option<i64>,
}

/// Parse a check_limit JSON response. Returns `true` if over limit.
///
/// Conservative: returns `true` (overloaded) on any parse error or missing field.
#[allow(dead_code)] // used once upload loop calls check_limit
pub(crate) fn parse_check_limit_response(body: &str) -> bool {
    match serde_json::from_str::<CheckLimitResponse>(body) {
        Ok(resp) => resp.over_limit.unwrap_or(1) != 0,
        Err(_) => true, // conservative: treat as overloaded
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
