use std::collections::HashMap;
use urlencoding::encode as url_encode;

/// Check if a string value needs uri() encoding.
///
/// Returns true if the string contains non-ASCII characters or any whitespace.
pub(crate) fn needs_quote(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if !s.is_ascii() {
        return true;
    }
    s.chars().any(|c| c.is_whitespace())
}

/// Encode a value for an IA S3 metadata header.
///
/// If the value contains non-ASCII or whitespace, wraps it as `uri({percent_encoded})`.
fn encode_value(value: &str) -> String {
    if needs_quote(value) {
        format!("uri({})", url_encode(value))
    } else {
        value.to_string()
    }
}

/// Encode a metadata key for IA S3 headers.
///
/// Replaces underscores with double-dashes per IA convention.
fn encode_key(key: &str) -> String {
    key.replace('_', "--")
}

/// Encode metadata key-value pairs into x-archive-meta headers.
///
/// Handles multivalue fields (incrementing index per field name),
/// underscore-to-double-dash key encoding, and uri() value encoding.
/// Skips empty values.
#[must_use = "encoded headers must be used"]
pub fn encode_metadata_headers(metadata: &[(String, String)]) -> Vec<(String, String)> {
    encode_headers_with_prefix(metadata, "meta")
}

/// Encode file-level metadata into x-archive-filemeta headers.
#[must_use = "encoded headers must be used"]
pub fn encode_file_metadata_headers(metadata: &[(String, String)]) -> Vec<(String, String)> {
    encode_headers_with_prefix(metadata, "filemeta")
}

fn encode_headers_with_prefix(
    metadata: &[(String, String)],
    prefix: &str,
) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut index_counters: HashMap<String, usize> = HashMap::new();

    for (key, value) in metadata {
        if value.is_empty() {
            continue;
        }

        let idx = index_counters.entry(key.clone()).or_insert(0);
        let header_key = format!("x-archive-{}{:02}-{}", prefix, *idx, encode_key(key));
        let header_value = encode_value(value);

        result.push((header_key, header_value));
        *idx += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- needs_quote tests --

    #[test]
    fn needs_quote_ascii_no_spaces() {
        assert!(!needs_quote("hello"));
        assert!(!needs_quote("foo-bar_baz.123"));
    }

    #[test]
    fn needs_quote_with_spaces() {
        assert!(needs_quote("hello world"));
        assert!(needs_quote("foo\tbar"));
        assert!(needs_quote("line\nbreak"));
    }

    #[test]
    fn needs_quote_non_ascii() {
        assert!(needs_quote("snowman ☃"));
        assert!(needs_quote("日本語"));
        assert!(needs_quote("café"));
    }

    #[test]
    fn needs_quote_empty() {
        assert!(!needs_quote(""));
    }

    // -- encode_metadata_headers tests --

    #[test]
    fn simple_single_value() {
        let headers = encode_metadata_headers(&[("title".into(), "My Item".into())]);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "x-archive-meta00-title");
        assert_eq!(headers[0].1, "uri(My%20Item)");
    }

    #[test]
    fn no_space_no_encoding() {
        let headers = encode_metadata_headers(&[("mediatype".into(), "texts".into())]);
        assert_eq!(headers[0].1, "texts");
    }

    #[test]
    fn underscore_in_key_becomes_double_dash() {
        let headers = encode_metadata_headers(&[("my_field".into(), "value".into())]);
        assert_eq!(headers[0].0, "x-archive-meta00-my--field");
    }

    #[test]
    fn multivalue_incrementing_index() {
        let headers = encode_metadata_headers(&[
            ("subject".into(), "rust".into()),
            ("subject".into(), "archive".into()),
            ("subject".into(), "cli".into()),
        ]);
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0].0, "x-archive-meta00-subject");
        assert_eq!(headers[0].1, "rust");
        assert_eq!(headers[1].0, "x-archive-meta01-subject");
        assert_eq!(headers[1].1, "archive");
        assert_eq!(headers[2].0, "x-archive-meta02-subject");
        assert_eq!(headers[2].1, "cli");
    }

    #[test]
    fn mixed_fields_each_start_at_zero() {
        let headers = encode_metadata_headers(&[
            ("title".into(), "Test".into()),
            ("subject".into(), "a".into()),
            ("subject".into(), "b".into()),
        ]);
        let title_h: Vec<_> = headers.iter().filter(|h| h.0.contains("title")).collect();
        let subj_h: Vec<_> = headers.iter().filter(|h| h.0.contains("subject")).collect();
        assert_eq!(title_h.len(), 1);
        assert_eq!(title_h[0].0, "x-archive-meta00-title");
        assert_eq!(subj_h.len(), 2);
        assert_eq!(subj_h[0].0, "x-archive-meta00-subject");
        assert_eq!(subj_h[1].0, "x-archive-meta01-subject");
    }

    #[test]
    fn empty_value_skipped() {
        let headers = encode_metadata_headers(&[
            ("title".into(), "".into()),
            ("mediatype".into(), "texts".into()),
        ]);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "x-archive-meta00-mediatype");
    }

    #[test]
    fn non_ascii_uri_encoded() {
        let headers = encode_metadata_headers(&[("title".into(), "snowman ☃".into())]);
        assert_eq!(headers[0].1, "uri(snowman%20%E2%98%83)");
    }

    #[test]
    fn cjk_uri_encoded() {
        let headers = encode_metadata_headers(&[("title".into(), "日本語".into())]);
        assert!(headers[0].1.starts_with("uri("));
    }

    #[test]
    fn emoji_uri_encoded() {
        let headers = encode_metadata_headers(&[("title".into(), "🚀".into())]);
        assert!(headers[0].1.starts_with("uri("));
    }

    // -- encode_file_metadata_headers tests --

    #[test]
    fn file_metadata_uses_filemeta_prefix() {
        let headers = encode_file_metadata_headers(&[("title".into(), "MyFile".into())]);
        assert_eq!(headers[0].0, "x-archive-filemeta00-title");
    }
}
