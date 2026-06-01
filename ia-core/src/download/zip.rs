//! Zip member listing and download from archive.org.
//!
//! archive.org serves a browsable HTML view of ZIP file contents at
//! `/download/{identifier}/{zip_filename}/`. Individual members can be
//! downloaded directly, and JP2 images can be converted to JPEG on the fly
//! by appending `&ext=jpg`.

use tracing::debug;

use crate::client::IaClient;
use crate::error::{IaError, Result};

/// A single entry within a ZIP file on archive.org.
#[derive(Debug, Clone)]
pub struct ZipEntry {
    /// Path within the zip (e.g., "itemname_jp2/itemname_0001.jp2").
    pub path: String,
    /// File size in bytes, if available.
    pub size: Option<u64>,
    /// Last modified timestamp string, if available.
    pub modified: Option<String>,
}

/// List the contents of a ZIP file on archive.org by parsing the HTML zip view.
///
/// Fetches the HTML directory listing at `/download/{identifier}/{zip_filename}/`
/// and parses the table rows to extract file entries.
pub async fn list_zip_contents(
    client: &IaClient,
    identifier: &str,
    zip_filename: &str,
) -> Result<Vec<ZipEntry>> {
    // Trailing `/` is required: without it archive.org redirects to the raw
    // binary zip download; with it, the redirect goes to the HTML directory
    // listing page (view_archive.php) that we parse for file entries.
    let url = client.url(&format!(
        "/download/{}/{}/",
        identifier,
        urlencoding::encode(zip_filename),
    ));

    debug!(identifier, zip_filename, "listing zip contents");

    // Use fetch_response for auth headers + redirect following with auth
    // preservation (reqwest strips Authorization on redirect by default).
    // count_views=false → inject cnt=0 so zip listings/members don't
    // increment the public view counter for the parent item.
    let response = super::fetch_response(client, &url, None, false).await?;

    let html = response.text().await.map_err(|e| IaError::Http {
        status: 0,
        message: format!("failed to read zip listing body: {e}"),
    })?;

    Ok(parse_zip_listing_html(&html))
}

/// Download a single file from within a ZIP on archive.org.
///
/// Uses `fetch_response` internally for auth headers, redirect following with
/// auth preservation, SSRF guard, and HTML error page stripping.
pub async fn download_zip_member(
    client: &IaClient,
    identifier: &str,
    zip_filename: &str,
    member_path: &str,
) -> Result<Vec<u8>> {
    let url = build_zip_member_url(client, identifier, zip_filename, member_path);

    debug!(
        identifier,
        zip_filename, member_path, "downloading zip member"
    );

    // count_views=false → inject cnt=0 so zip listings/members don't
    // increment the public view counter for the parent item.
    let response = super::fetch_response(client, &url, None, false).await?;

    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| IaError::Http {
            status: 0,
            message: format!("failed to read zip member body: {e}"),
        })
}

/// Download a ZIP member and convert to another format (e.g., JP2 → JPEG).
///
/// Uses archive.org's on-the-fly conversion by appending `&ext=jpg` (or
/// similar) to the download URL.
///
/// Uses `fetch_response` internally for auth headers, redirect following with
/// auth preservation, SSRF guard, and HTML error page stripping.
pub async fn download_zip_member_converted(
    client: &IaClient,
    identifier: &str,
    zip_filename: &str,
    member_path: &str,
    ext: &str,
) -> Result<Vec<u8>> {
    let base_url = build_zip_member_url(client, identifier, zip_filename, member_path);
    let url = format!("{base_url}&ext={ext}");

    debug!(
        identifier,
        zip_filename, member_path, ext, "downloading converted zip member"
    );

    // count_views=false → inject cnt=0 so zip listings/members don't
    // increment the public view counter for the parent item.
    let response = super::fetch_response(client, &url, None, false).await?;

    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| IaError::Http {
            status: 0,
            message: format!("failed to read converted zip member body: {e}"),
        })
}

/// Find the best JP2 zip file for an item.
///
/// Prefers `{identifier}_jp2.zip` over `{identifier}_raw_jp2.zip`.
/// Returns `None` if no JP2 zip is found.
pub fn find_jp2_zip(identifier: &str, files: &[crate::types::FileMetadata]) -> Option<String> {
    let preferred = format!("{identifier}_jp2.zip");
    let fallback = format!("{identifier}_raw_jp2.zip");

    if files.iter().any(|f| f.name == preferred) {
        return Some(preferred);
    }
    if files.iter().any(|f| f.name == fallback) {
        return Some(fallback);
    }

    // Fall back to any file ending in _jp2.zip
    files
        .iter()
        .find(|f| f.name.ends_with("_jp2.zip"))
        .map(|f| f.name.clone())
}

/// Build the download URL for a zip member.
pub(crate) fn build_zip_member_url(
    client: &IaClient,
    identifier: &str,
    zip_filename: &str,
    member_path: &str,
) -> String {
    let encoded_member = member_path
        .split('/')
        .map(|seg| urlencoding::encode(seg))
        .collect::<Vec<_>>()
        .join("/");

    client.url(&format!(
        "/download/{}/{}/{}",
        identifier,
        urlencoding::encode(zip_filename),
        encoded_member,
    ))
}

/// Parse the HTML zip listing page into `ZipEntry` structs.
///
/// The archive.org zip view (`view_archive.php`) renders a table with rows
/// containing links to files. Real HTML uses unclosed `<td>` tags:
///
/// ```text
/// <tr><td><a href="//archive.org/download/id/zip/path%2Ffile.jp2">path/file.jp2</a>
///     <td id="jpg"><a href="...&ext=jpg">jpg</a>
///     <td>2026-03-24 20:30<td id="size">105368</tr>
/// ```
///
/// We only match lines starting with `<tr>` to avoid picking up navigation
/// links (`#maincontent`, external URLs) that appear elsewhere in the page.
pub(crate) fn parse_zip_listing_html(html: &str) -> Vec<ZipEntry> {
    let mut entries = Vec::new();

    for line in html.lines() {
        let trimmed = line.trim();

        // Only process table rows — skip navigation/header links outside <tr>.
        if !trimmed.starts_with("<tr>") {
            continue;
        }

        // Look for the first <a href="..."> in the row (the file link).
        if let Some(start) = trimmed.find("<a href=\"") {
            let after_href = &trimmed[start + 9..];
            if let Some(end_quote) = after_href.find('"') {
                let href = &after_href[..end_quote];

                // Skip parent directory links and non-file links
                if href.ends_with('/') || href == ".." || href == "." {
                    continue;
                }

                // Extract the displayed text (filename/path)
                if let Some(tag_close) = after_href.find('>') {
                    let after_tag = &after_href[tag_close + 1..];
                    if let Some(end_a) = after_tag.find("</a>") {
                        let display_name = after_tag[..end_a].trim();
                        if !display_name.is_empty() {
                            let (size, modified) = parse_table_cells(after_tag);
                            entries.push(ZipEntry {
                                path: display_name.to_string(),
                                size,
                                modified,
                            });
                        }
                    }
                }
            }
        }
    }

    entries
}

/// Extract size and modified date from the table cells after the file link.
///
/// Real archive.org HTML uses unclosed `<td>` tags with `id` attributes to
/// identify cell types. Positional parsing doesn't work because the cells
/// lack `</td>` closing tags, so we match by `id` attribute instead:
///
/// - `<td id="size">` → file size in bytes
/// - Plain `<td>` with digit content → modified date
/// - `<td id="jpg">` → jpg conversion link (skipped)
fn parse_table_cells(after_link: &str) -> (Option<u64>, Option<String>) {
    let mut size = None;
    let mut modified = None;

    let mut remaining = after_link;

    while let Some(td_start) = remaining.find("<td") {
        let td_tag = &remaining[td_start..];

        // Find the end of the opening tag
        let Some(tag_end) = td_tag.find('>') else {
            break;
        };
        let tag_header = &td_tag[..tag_end];
        let after_tag = &td_tag[tag_end + 1..];

        // Cell content runs until the next `<` (next tag boundary).
        // This handles both closed (`</td>`) and unclosed `<td>` styles.
        let content_end = after_tag.find('<').unwrap_or(after_tag.len());
        let cell_text = after_tag[..content_end].trim();

        if tag_header.contains("id=\"size\"") {
            // Size cell: parse as u64, stripping any comma separators.
            if let Ok(s) = cell_text.replace(',', "").parse::<u64>() {
                size = Some(s);
            }
        } else if !tag_header.contains("id=") && !cell_text.is_empty() && cell_text != "-" {
            // Plain <td> without id — likely the modified date if it has digits.
            if cell_text.chars().any(|c| c.is_ascii_digit()) {
                modified = Some(cell_text.to_string());
            }
        }

        remaining = &td_tag[tag_end + 1..];
    }

    (size, modified)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_zip_url_has_trailing_slash() {
        let config = crate::config::IaConfig::default();
        let client = crate::IaClient::from_config(config).unwrap();
        let url = client.url(&format!(
            "/download/{}/{}/",
            "test-item",
            urlencoding::encode("test_jp2.zip"),
        ));
        assert!(
            url.ends_with('/'),
            "zip listing URL must end with / to get HTML listing instead of binary download"
        );
    }

    /// Test with realistic archive.org HTML (unclosed `<td>` tags, `id` attributes).
    #[test]
    fn parse_zip_listing_real_html() {
        // This matches the actual format served by archive.org's view_archive.php.
        // Uses r##"..."## because HTML contains `"#` (e.g., href="#maincontent").
        let html = r##"
<a href="#maincontent" class="hidden-for-screen-readers">Skip to main content</a>
<p><a href="https://change.org/LetReadersRead">Ask the publishers</a></p>
<table>
<tr><td><a href="//archive.org/download/item/item_jp2.zip/item_jp2%2Fitem_0000.jp2">item_jp2/item_0000.jp2</a><td id="jpg"><a href="//archive.org/download/item/item_jp2.zip/item_jp2%2Fitem_0000.jp2&ext=jpg">jpg</a><td>2026-03-24 20:30<td id="size">105368</tr>
<tr><td><a href="//archive.org/download/item/item_jp2.zip/item_jp2%2Fitem_0001.jp2">item_jp2/item_0001.jp2</a><td id="jpg"><a href="//archive.org/download/item/item_jp2.zip/item_jp2%2Fitem_0001.jp2&ext=jpg">jpg</a><td>2026-03-24 20:31<td id="size">234567</tr>
<tr><td><a href="//archive.org/download/item/item_jp2.zip/item_jp2%2Fitem_0002.jp2">item_jp2/item_0002.jp2</a><td id="jpg"><a href="//archive.org/download/item/item_jp2.zip/item_jp2%2Fitem_0002.jp2&ext=jpg">jpg</a><td>2026-03-24 20:31<td id="size">345678</tr>
</table>
        "##;

        let entries = parse_zip_listing_html(html);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, "item_jp2/item_0000.jp2");
        assert_eq!(entries[1].path, "item_jp2/item_0001.jp2");
        assert_eq!(entries[2].path, "item_jp2/item_0002.jp2");
        // Verify size and modified are parsed from unclosed <td> tags
        assert_eq!(entries[0].size, Some(105368));
        assert_eq!(entries[0].modified, Some("2026-03-24 20:30".to_string()));
        assert_eq!(entries[2].size, Some(345678));
    }

    #[test]
    fn parse_zip_listing_skips_navigation_links() {
        // Navigation/header links outside <tr> rows must not be picked up.
        let html = r##"
<a href="#maincontent">Skip to main content</a>
<a href="https://change.org/LetReadersRead">change.org</a>
<tr><td><a href="//archive.org/download/item/zip/item_jp2%2Ffile.jp2">item_jp2/file.jp2</a><td>2026-01-01<td id="size">999</tr>
        "##;

        let entries = parse_zip_listing_html(html);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "item_jp2/file.jp2");
    }

    #[test]
    fn parse_zip_listing_skips_directory_links() {
        let html = r#"
<tr><td><a href="/download/item/item_jp2.zip/">../</a></tr>
<tr><td><a href="/download/item/item_jp2.zip/item_jp2/">item_jp2/</a></tr>
<tr><td><a href="/download/item/item_jp2.zip/item_jp2/file.jp2">item_jp2/file.jp2</a><td>2024-01-01<td id="size">100</tr>
        "#;

        let entries = parse_zip_listing_html(html);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "item_jp2/file.jp2");
    }

    #[test]
    fn parse_zip_listing_empty_html() {
        let entries = parse_zip_listing_html("");
        assert!(entries.is_empty());
    }

    /// Legacy HTML with `</td>` closing tags should still work.
    #[test]
    fn parse_zip_listing_closed_td_tags() {
        let html = r#"
<tr><td><a href="/download/item/zip/file.jp2">file.jp2</a></td><td>2024-01-01</td><td id="size">12345</td></tr>
        "#;

        let entries = parse_zip_listing_html(html);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "file.jp2");
        assert_eq!(entries[0].size, Some(12345));
        assert_eq!(entries[0].modified, Some("2024-01-01".to_string()));
    }

    #[test]
    fn find_jp2_zip_preferred() {
        use std::collections::HashMap;
        let files = vec![
            crate::types::FileMetadata {
                name: "item_jp2.zip".to_string(),
                source: None,
                format: None,
                md5: None,
                size: None,
                mtime: None,
                sha1: None,
                crc32: None,
                original: None,
                rotation: None,
                extra: HashMap::new(),
            },
            crate::types::FileMetadata {
                name: "item_raw_jp2.zip".to_string(),
                source: None,
                format: None,
                md5: None,
                size: None,
                mtime: None,
                sha1: None,
                crc32: None,
                original: None,
                rotation: None,
                extra: HashMap::new(),
            },
        ];
        assert_eq!(
            find_jp2_zip("item", &files),
            Some("item_jp2.zip".to_string())
        );
    }

    #[test]
    fn find_jp2_zip_fallback_to_raw() {
        use std::collections::HashMap;
        let files = vec![crate::types::FileMetadata {
            name: "item_raw_jp2.zip".to_string(),
            source: None,
            format: None,
            md5: None,
            size: None,
            mtime: None,
            sha1: None,
            crc32: None,
            original: None,
            rotation: None,
            extra: HashMap::new(),
        }];
        assert_eq!(
            find_jp2_zip("item", &files),
            Some("item_raw_jp2.zip".to_string())
        );
    }

    #[test]
    fn find_jp2_zip_none() {
        use std::collections::HashMap;
        let files = vec![crate::types::FileMetadata {
            name: "item_meta.xml".to_string(),
            source: None,
            format: None,
            md5: None,
            size: None,
            mtime: None,
            sha1: None,
            crc32: None,
            original: None,
            rotation: None,
            extra: HashMap::new(),
        }];
        assert_eq!(find_jp2_zip("item", &files), None);
    }

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config
    }

    /// Zip directory listing must send `cnt=0` to suppress view-counting.
    #[tokio::test]
    async fn list_zip_contents_sends_cnt_zero() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/download/test-item/item.zip/"))
            .and(query_param("cnt", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<tr><td><a href="/download/test-item/item.zip/file.txt">file.txt</a><td>2024<td id="size">10</tr>"#,
            ))
            .mount(&server)
            .await;

        let client = crate::IaClient::from_config(mock_config(&server.uri())).unwrap();
        let entries = list_zip_contents(&client, "test-item", "item.zip")
            .await
            .expect("list_zip_contents should succeed when cnt=0 is sent");
        assert_eq!(entries.len(), 1);
    }

    /// `download_zip_member_converted` builds URLs with IA's `&ext=` quirk
    /// (no preceding `?`). Verify `cnt=0` is still sent as a proper query
    /// parameter and the `&ext=` segment is preserved in the path.
    #[tokio::test]
    async fn download_zip_member_converted_sends_cnt_zero() {
        use wiremock::matchers::{method, query_param, query_param_is_missing};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            // wiremock decodes the request path; the `&ext=jpg` lives in the
            // path (not the query) because of IA's quirky URL form, so the
            // matched path includes it verbatim.
            .and(wiremock::matchers::path(
                "/download/item/zip.zip/page.jp2&ext=jpg",
            ))
            .and(query_param("cnt", "0"))
            // `ext` must NOT appear as a query parameter — it stays in the path.
            .and(query_param_is_missing("ext"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0xff, 0xd8]))
            .mount(&server)
            .await;

        let client = crate::IaClient::from_config(mock_config(&server.uri())).unwrap();
        let bytes = download_zip_member_converted(&client, "item", "zip.zip", "page.jp2", "jpg")
            .await
            .expect("converted zip member download should succeed with cnt=0");
        assert_eq!(bytes, vec![0xff, 0xd8]);
    }
}
