/// Parsed S3 error response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Error {
    pub code: String,
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
    /// Retryable: SlowDown, InternalError, ServiceUnavailable, OperationAborted
    /// Non-retryable: AccessDenied, InvalidAccessKeyId, BadDigest, MissingContentLength, etc.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.code.as_str(),
            "SlowDown" | "InternalError" | "ServiceUnavailable" | "OperationAborted"
        )
    }
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

    #[test]
    fn retryable_codes() {
        let retryable = ["SlowDown", "InternalError", "ServiceUnavailable", "OperationAborted"];
        for code in retryable {
            let err = S3Error { code: code.into(), message: String::new() };
            assert!(err.is_retryable(), "{code} should be retryable");
        }
    }

    #[test]
    fn non_retryable_codes() {
        let non_retryable = [
            "AccessDenied", "InvalidAccessKeyId", "BadDigest",
            "MissingContentLength", "NoSuchBucket", "InvalidArgument",
        ];
        for code in non_retryable {
            let err = S3Error { code: code.into(), message: String::new() };
            assert!(!err.is_retryable(), "{code} should NOT be retryable");
        }
    }
}
