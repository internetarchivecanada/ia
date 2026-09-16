/// Parsed S3 error response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Error {
    /// S3 error code (e.g. "AccessDenied", "SlowDown").
    pub code: String,
    /// Human-readable error message from S3.
    pub message: String,
}

/// Parse an S3 XML error response body.
///
/// Extracts `<Code>` and `<Message>` using string matching (no XML crate).
/// Returns `None` if the body doesn't look like an S3 error.
///
/// Example input:
/// ```xml
/// <Error>
///   <Code>AccessDenied</Code>
///   <Message>Access Denied</Message>
///   <Resource>/my-item/file.pdf</Resource>
///   <RequestId>db1b9e2b-...</RequestId>
/// </Error>
/// ```
#[must_use = "parsed error should be inspected"]
pub fn parse_s3_error(body: &str) -> Option<S3Error> {
    let code = extract_xml_field(body, "Code")?;
    let message = extract_xml_field(body, "Message").unwrap_or_default();
    Some(S3Error { code, message })
}

/// Extract text between `<Tag>` and `</Tag>` using simple string matching.
fn extract_xml_field(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].trim().to_string())
}

impl S3Error {
    /// Whether this S3 error code indicates a retryable condition.
    ///
    /// Retryable: SlowDown, InternalError, ServiceUnavailable, OperationAborted,
    /// RequestTimeout, RequestLimitExceeded, ThrottlingException
    /// Non-retryable: AccessDenied, InvalidAccessKeyId, BadDigest, MissingContentLength, etc.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.code.as_str(),
            "SlowDown"
                | "InternalError"
                | "ServiceUnavailable"
                | "OperationAborted"
                | "RequestTimeout"
                | "RequestLimitExceeded"
                | "ThrottlingException"
        )
    }
}

/// Whether an IA-S3 response should be retried.
///
/// The single source of truth for every upload request: single-file PUTs,
/// multipart part PUTs, and the multipart control calls. They are the same
/// protocol against the same endpoint, so they get the same policy.
///
/// Decided on the S3 error `<Code>` when the body is a parseable S3 error,
/// because the code says what actually happened and the status does not. IA
/// returns 503 both for `SlowDown` (throttled, request never applied, retry
/// is correct) and for genuine faults, and returns non-retryable conditions
/// under a range of statuses.
///
/// Falls back to the status when the body is not an S3 error — an HTML error
/// page from a proxy, or an empty body. There, 5xx and 429 are the only
/// retryable signals; 4xx means the request was understood and refused.
///
/// `429` is retryable here even though the middleware declines it for other
/// endpoints: on S3 there is no separate application-level rate-limit loop
/// for part uploads, so leaving it to a caller means leaving it unhandled.
#[must_use]
pub fn should_retry_s3(status: reqwest::StatusCode, body: &str) -> bool {
    match parse_s3_error(body) {
        Some(err) => err.is_retryable(),
        None => status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS,
    }
}

/// Strip XML/HTML tags and collapse whitespace so raw S3 response bodies
/// don't leak through to user-facing error messages.
///
/// ```
/// use ia_core::upload::s3_error::strip_xml;
/// assert_eq!(strip_xml("<Error><Code>Oops</Code></Error>"), "Oops");
/// assert_eq!(strip_xml("plain text"), "plain text");
/// ```
#[must_use]
pub fn strip_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_access_denied() {
        let xml = r#"<?xml version='1.0' encoding='UTF-8'?>
<Error><Code>AccessDenied</Code><Message>Access Denied</Message><Resource>/my-item/file.pdf</Resource><RequestId>abc123</RequestId></Error>"#;
        let err = parse_s3_error(xml).unwrap();
        assert_eq!(err.code, "AccessDenied");
        assert_eq!(err.message, "Access Denied");
        assert!(!err.is_retryable());
    }

    #[test]
    fn parse_slow_down() {
        let xml = "<Error><Code>SlowDown</Code><Message>Please reduce your request rate.</Message></Error>";
        let err = parse_s3_error(xml).unwrap();
        assert_eq!(err.code, "SlowDown");
        assert!(err.is_retryable());
    }

    #[test]
    fn parse_bad_digest() {
        let xml = "<Error><Code>BadDigest</Code><Message>The Content-MD5 you specified did not match.</Message></Error>";
        let err = parse_s3_error(xml).unwrap();
        assert_eq!(err.code, "BadDigest");
        assert!(!err.is_retryable());
    }

    #[test]
    fn parse_internal_error() {
        let xml = "<Error><Code>InternalError</Code><Message>We encountered an internal error.</Message></Error>";
        let err = parse_s3_error(xml).unwrap();
        assert!(err.is_retryable());
    }

    #[test]
    fn parse_operation_aborted() {
        let xml = "<Error><Code>OperationAborted</Code><Message>A conflicting operation is in progress.</Message></Error>";
        let err = parse_s3_error(xml).unwrap();
        assert!(err.is_retryable());
    }

    #[test]
    fn parse_multiline_xml() {
        let xml = r#"<?xml version='1.0' encoding='UTF-8'?>
<Error>
  <Code>AccessDenied</Code>
  <Message>Access Denied</Message>
  <Resource>/my-item/file.pdf</Resource>
  <RequestId>db1b9e2b-1234</RequestId>
</Error>"#;
        let err = parse_s3_error(xml).unwrap();
        assert_eq!(err.code, "AccessDenied");
        assert_eq!(err.message, "Access Denied");
    }

    #[test]
    fn parse_not_xml_returns_none() {
        assert!(parse_s3_error("just plain text").is_none());
        assert!(parse_s3_error("").is_none());
        assert!(parse_s3_error("{}").is_none());
    }

    #[test]
    fn parse_missing_code_returns_none() {
        let xml = "<Error><Message>Something</Message></Error>";
        assert!(parse_s3_error(xml).is_none());
    }

    // ── should_retry_s3: the shared upload policy ────────────────────────

    #[test]
    fn s3_code_decides_over_status() {
        use reqwest::StatusCode;

        // 503 SlowDown is throttling: rejected, never applied, so retry.
        let slowdown = "<Error><Code>SlowDown</Code><Message>Reduce rate</Message></Error>";
        assert!(should_retry_s3(StatusCode::SERVICE_UNAVAILABLE, slowdown));

        // A non-retryable code wins even when the status says 5xx. Retrying
        // this twelve times is what the old status-only check did.
        let denied = "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>";
        assert!(!should_retry_s3(StatusCode::SERVICE_UNAVAILABLE, denied));

        // And a retryable code wins even when the status is not 5xx.
        let throttled = "<Error><Code>ThrottlingException</Code><Message>slow</Message></Error>";
        assert!(should_retry_s3(StatusCode::BAD_REQUEST, throttled));
    }

    #[test]
    fn falls_back_to_status_when_body_is_not_an_s3_error() {
        use reqwest::StatusCode;

        // Proxy HTML, empty bodies: no code to read, so trust the status.
        for body in ["<html>502 Bad Gateway</html>", "", "not xml"] {
            assert!(
                should_retry_s3(StatusCode::BAD_GATEWAY, body),
                "body: {body:?}"
            );
            assert!(
                should_retry_s3(StatusCode::TOO_MANY_REQUESTS, body),
                "body: {body:?}"
            );
            assert!(
                !should_retry_s3(StatusCode::FORBIDDEN, body),
                "body: {body:?}"
            );
            assert!(
                !should_retry_s3(StatusCode::NOT_FOUND, body),
                "body: {body:?}"
            );
            assert!(
                !should_retry_s3(StatusCode::BAD_REQUEST, body),
                "body: {body:?}"
            );
        }
    }

    #[test]
    fn no_such_upload_is_not_retryable() {
        use reqwest::StatusCode;

        // Retrying a completed upload's completion is pointless; the caller
        // interprets this code instead.
        let body = "<Error><Code>NoSuchUpload</Code><Message>no such upload</Message></Error>";
        assert!(!should_retry_s3(StatusCode::NOT_FOUND, body));
    }

    #[test]
    fn retryable_codes() {
        let retryable = [
            "SlowDown",
            "InternalError",
            "ServiceUnavailable",
            "OperationAborted",
            "RequestTimeout",
            "RequestLimitExceeded",
            "ThrottlingException",
        ];
        for code in retryable {
            let err = S3Error {
                code: code.into(),
                message: String::new(),
            };
            assert!(err.is_retryable(), "{code} should be retryable");
        }
    }

    #[test]
    fn non_retryable_codes() {
        let non_retryable = [
            "AccessDenied",
            "InvalidAccessKeyId",
            "BadDigest",
            "MissingContentLength",
            "NoSuchBucket",
            "InvalidArgument",
        ];
        for code in non_retryable {
            let err = S3Error {
                code: code.into(),
                message: String::new(),
            };
            assert!(!err.is_retryable(), "{code} should NOT be retryable");
        }
    }

    #[test]
    fn strip_xml_removes_tags() {
        let xml = r#"<?xml version='1.0'?><Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>"#;
        assert_eq!(strip_xml(xml), "AccessDenied Access Denied");
    }

    #[test]
    fn strip_xml_plain_text_unchanged() {
        assert_eq!(strip_xml("plain text"), "plain text");
        assert_eq!(
            strip_xml("HTTP 500: server error"),
            "HTTP 500: server error"
        );
    }

    #[test]
    fn strip_xml_collapses_whitespace() {
        assert_eq!(strip_xml("  lots   of   space  "), "lots of space");
    }
}
