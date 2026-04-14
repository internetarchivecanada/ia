//! Scandata XML parser — page structure metadata for scanned books.
//!
//! Scandata XML files (`{id}_scandata.xml`) are produced by the book
//! scanning pipeline and contain per-page metadata: page type annotations
//! (Cover, Title, Normal, etc.), crop boxes, rotation, and PPI.
//!
//! This module provides a general-purpose parser for scandata XML that can
//! be consumed by any feature needing page structure information.

use crate::error::{IaError, Result};
use crate::types::{FileMetadata, ItemMetadata};
use crate::IaClient;
use tracing::debug;

// ── Types ──────────────────────────────────────────────────────────────

/// A single page from scandata with its structural metadata.
#[derive(Debug, Clone)]
pub struct ScandataPage {
    /// The leaf number — maps to the image filename in the JP2 zip
    /// (e.g., leaf 3 → `item_0003.jp2`).
    pub leaf_num: u32,
    /// Semantic page type as annotated in scandata (e.g., "Cover", "Title",
    /// "Normal", "Color Card"). Case varies by scanner.
    pub page_type: String,
    /// Crop box for the page content area. Uses `cropBox` from scandata.
    pub crop_box: Option<CropBox>,
    /// Rotation in degrees to apply for correct orientation.
    pub rotate_degree: i32,
    /// Whether this page should be included in access formats.
    pub add_to_access_formats: bool,
}

/// Crop coordinates defining the content area of a scanned page.
#[derive(Debug, Clone, Copy)]
pub struct CropBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

// ── Finding scandata ───────────────────────────────────────────────────

/// Find the scandata filename in an item's file list.
///
/// Looks for `{identifier}_scandata.xml` by name, or any file with
/// format "Scandata".
pub fn find_scandata_file(identifier: &str, files: &[FileMetadata]) -> Option<String> {
    let expected_name = format!("{identifier}_scandata.xml");

    // Try exact name match first
    if let Some(f) = files.iter().find(|f| f.name == expected_name) {
        return Some(f.name.clone());
    }

    // Fall back to format match
    files.iter().find_map(|f| {
        let format = f.format.as_deref().unwrap_or_default();
        if format.contains("Scandata") && f.name.ends_with(".xml") {
            Some(f.name.clone())
        } else {
            None
        }
    })
}

/// Convenience: find scandata file from an `ItemMetadata`.
pub fn find_scandata_file_from_item(item: &ItemMetadata) -> Option<String> {
    let identifier = item.metadata.identifier.as_deref().unwrap_or_default();
    find_scandata_file(identifier, &item.files)
}

// ── Fetching + parsing ─────────────────────────────────────────────────

/// Download and parse scandata.xml for an item.
pub async fn fetch_scandata(
    client: &IaClient,
    identifier: &str,
    scandata_filename: &str,
) -> Result<Vec<ScandataPage>> {
    let url = client.url(&format!(
        "/download/{}/{}",
        identifier,
        urlencoding::encode(scandata_filename),
    ));

    debug!(identifier, scandata_filename, "fetching scandata");

    let response = client
        .http()
        .get(&url)
        .send()
        .await
        .map_err(|e| IaError::Http {
            status: 0,
            message: format!("failed to fetch scandata: {e}"),
        })?;

    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status,
            message: format!("failed to fetch scandata for {identifier}: {body}"),
        });
    }

    let body = response.text().await.map_err(|e| IaError::Http {
        status: 0,
        message: format!("failed to read scandata body: {e}"),
    })?;

    parse_scandata(&body)
}

/// Parse scandata XML into a list of pages.
///
/// Uses simple string-based XML parsing (consistent with the codebase's
/// approach in `s3_error.rs`). Handles the standard scandata format:
///
/// ```xml
/// <book>
///   <pageData>
///     <page leafNum="3">
///       <pageType>Normal</pageType>
///       <rotateDegree>90</rotateDegree>
///       <cropBox><x>100</x><y>200</y><w>3000</w><h>4000</h></cropBox>
///       <addToAccessFormats>true</addToAccessFormats>
///     </page>
///     ...
///   </pageData>
/// </book>
/// ```
pub fn parse_scandata(xml: &str) -> Result<Vec<ScandataPage>> {
    let mut pages = Vec::new();

    // Split on <page to find each page element
    let page_chunks: Vec<&str> = xml.split("<page ").collect();

    // Skip the first chunk (everything before the first <page)
    for chunk in page_chunks.iter().skip(1) {
        // Find the end of this page element
        let page_xml = match chunk.find("</page>") {
            Some(end) => &chunk[..end],
            None => continue,
        };

        let leaf_num = match extract_attr(page_xml, "leafNum") {
            Some(s) => match s.parse::<u32>() {
                Ok(n) => n,
                Err(_) => continue,
            },
            None => continue,
        };

        let page_type = extract_element(page_xml, "pageType").unwrap_or_default();
        if page_type.is_empty() {
            continue;
        }

        let rotate_degree = extract_element(page_xml, "rotateDegree")
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0);

        let add_to_access = extract_element(page_xml, "addToAccessFormats")
            .map(|s| s == "true")
            .unwrap_or(true);

        let crop_box = parse_crop_box(page_xml);

        pages.push(ScandataPage {
            leaf_num,
            page_type,
            crop_box,
            rotate_degree,
            add_to_access_formats: add_to_access,
        });
    }

    if pages.is_empty() {
        return Err(IaError::Config(
            "scandata XML contained no valid pages".to_string(),
        ));
    }

    Ok(pages)
}

/// Find pages matching a given page type (case-insensitive).
///
/// Returns indices into the `pages` slice, matching the behavior of
/// `scandata_get_pagetype_pages()` from the Python extractor.
pub fn find_pages_by_type(pages: &[ScandataPage], page_type: &str) -> Vec<usize> {
    let needle = page_type.to_lowercase();
    pages
        .iter()
        .enumerate()
        .filter(|(_, p)| p.page_type.to_lowercase() == needle)
        .map(|(i, _)| i)
        .collect()
}

/// Map a leaf number to its zip member path.
///
/// Searches the zip entry list for a file whose name (sans extension)
/// ends with the zero-padded leaf number. For example, leaf 3 matches
/// `item_jp2/item_0003.jp2`.
pub fn leaf_to_zip_path(leaf_num: u32, zip_entries: &[String]) -> Option<String> {
    // Try 4-digit padding first (most common), then wider
    for width in [4, 5, 6, 8] {
        let suffix = format!("{:0>width$}", leaf_num, width = width);
        if let Some(path) = zip_entries.iter().find(|entry| {
            let stem = entry
                .rsplit('/')
                .next()
                .unwrap_or(entry)
                .rsplit_once('.')
                .map(|(s, _)| s)
                .unwrap_or(entry);
            stem.ends_with(&suffix)
        }) {
            return Some(path.clone());
        }
    }
    None
}

// ── Internal parsing helpers ───────────────────────────────────────────

/// Extract an XML attribute value: `leafNum="3"` → `"3"`.
fn extract_attr(xml: &str, attr_name: &str) -> Option<String> {
    let pattern = format!("{attr_name}=\"");
    let start = xml.find(&pattern)? + pattern.len();
    let end = xml[start..].find('"')? + start;
    Some(xml[start..end].to_string())
}

/// Extract text content of a simple XML element: `<pageType>Normal</pageType>` → `"Normal"`.
fn extract_element(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let text = xml[start..end].trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Parse the `<cropBox>` element from a page's XML.
///
/// Looks for `<cropBox><x>N</x><y>N</y><w>N</w><h>N</h></cropBox>`.
/// Returns `None` if the element is missing or any coordinate fails to parse.
fn parse_crop_box(page_xml: &str) -> Option<CropBox> {
    // Find the cropBox section — use the first one (not nested inside cropBoxAutoDetect)
    // The direct <cropBox> appears before <cropBoxAutoDetect> in standard scandata.
    let crop_start = page_xml.find("<cropBox>")?;

    // Make sure we're not inside a cropBoxAutoDetect by checking what's before
    let before = &page_xml[..crop_start];
    if before.ends_with("adjusted") || before.contains("<cropBoxAutoDetect>") {
        // This cropBox is inside autodetect — look for one before autodetect
        // Actually, the direct cropBox always comes before cropBoxAutoDetect
        // in standard scandata, so if we found one after autodetect opened,
        // there should be one before it too. Let's just take the first match.
    }

    let crop_end = page_xml[crop_start..].find("</cropBox>")? + crop_start;
    let crop_xml = &page_xml[crop_start..crop_end];

    let x = extract_element(crop_xml, "x")?.parse::<u32>().ok()?;
    let y = extract_element(crop_xml, "y")?.parse::<u32>().ok()?;
    let w = extract_element(crop_xml, "w")?.parse::<u32>().ok()?;
    let h = extract_element(crop_xml, "h")?.parse::<u32>().ok()?;

    Some(CropBox { x, y, w, h })
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal scandata XML for testing.
    const SCANDATA_FIXTURE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<book>
	<pageData>
		<page leafNum="0">
			<pageType>Color Card</pageType>
			<rotateDegree>-90</rotateDegree>
			<addToAccessFormats>false</addToAccessFormats>
			<cropBox>
				<x>100</x>
				<y>100</y>
				<w>600</w>
				<h>600</h>
			</cropBox>
		</page>
		<page leafNum="1">
			<pageType>Cover</pageType>
			<rotateDegree>90</rotateDegree>
			<addToAccessFormats>true</addToAccessFormats>
			<cropBox>
				<x>390</x>
				<y>1085</y>
				<w>3153</w>
				<h>3975</h>
			</cropBox>
			<cropBoxAutoDetect>
				<cropScore>7.90</cropScore>
				<pass2Crop><x>168</x><y>984</y><w>3420</w><h>4121</h></pass2Crop>
			</cropBoxAutoDetect>
		</page>
		<page leafNum="2">
			<pageType>Normal</pageType>
			<rotateDegree>-90</rotateDegree>
			<addToAccessFormats>true</addToAccessFormats>
			<cropBox>
				<x>542</x>
				<y>1178</y>
				<w>2729</w>
				<h>3669</h>
			</cropBox>
		</page>
		<page leafNum="3">
			<pageType>Normal</pageType>
			<rotateDegree>90</rotateDegree>
			<addToAccessFormats>true</addToAccessFormats>
			<cropBox>
				<x>300</x>
				<y>900</y>
				<w>3000</w>
				<h>4000</h>
			</cropBox>
		</page>
		<page leafNum="4">
			<pageType>Normal</pageType>
			<rotateDegree>-90</rotateDegree>
			<addToAccessFormats>true</addToAccessFormats>
			<cropBox>
				<x>400</x>
				<y>1000</y>
				<w>2800</w>
				<h>3800</h>
			</cropBox>
		</page>
		<page leafNum="5">
			<pageType>Title</pageType>
			<rotateDegree>90</rotateDegree>
			<addToAccessFormats>true</addToAccessFormats>
			<cropBox>
				<x>347</x>
				<y>1347</y>
				<w>2729</w>
				<h>3669</h>
			</cropBox>
		</page>
		<page leafNum="6">
			<pageType>Normal</pageType>
			<rotateDegree>-90</rotateDegree>
			<addToAccessFormats>true</addToAccessFormats>
			<cropBox>
				<x>500</x>
				<y>1100</y>
				<w>2700</w>
				<h>3700</h>
			</cropBox>
		</page>
		<page leafNum="7">
			<pageType>Normal</pageType>
			<rotateDegree>90</rotateDegree>
			<addToAccessFormats>true</addToAccessFormats>
			<cropBox>
				<x>350</x>
				<y>950</y>
				<w>2900</w>
				<h>3900</h>
			</cropBox>
		</page>
		<page leafNum="28">
			<pageType>Cover</pageType>
			<rotateDegree>-90</rotateDegree>
			<addToAccessFormats>true</addToAccessFormats>
			<cropBox>
				<x>200</x>
				<y>800</y>
				<w>3100</w>
				<h>4100</h>
			</cropBox>
		</page>
		<page leafNum="29">
			<pageType>Color Card</pageType>
			<rotateDegree>90</rotateDegree>
			<addToAccessFormats>false</addToAccessFormats>
			<cropBox>
				<x>100</x>
				<y>100</y>
				<w>600</w>
				<h>600</h>
			</cropBox>
		</page>
	</pageData>
</book>"#;

    #[test]
    fn parse_all_pages() {
        let pages = parse_scandata(SCANDATA_FIXTURE).unwrap();
        assert_eq!(pages.len(), 10);

        // First page: Color Card at leaf 0
        assert_eq!(pages[0].leaf_num, 0);
        assert_eq!(pages[0].page_type, "Color Card");
        assert_eq!(pages[0].rotate_degree, -90);
        assert!(!pages[0].add_to_access_formats);

        // Cover at leaf 1
        assert_eq!(pages[1].leaf_num, 1);
        assert_eq!(pages[1].page_type, "Cover");
        assert_eq!(pages[1].rotate_degree, 90);

        // Title at leaf 5
        assert_eq!(pages[5].leaf_num, 5);
        assert_eq!(pages[5].page_type, "Title");

        // Back cover at leaf 28
        assert_eq!(pages[8].leaf_num, 28);
        assert_eq!(pages[8].page_type, "Cover");
    }

    #[test]
    fn parse_crop_boxes() {
        let pages = parse_scandata(SCANDATA_FIXTURE).unwrap();

        // Color card: simple cropBox
        let crop = pages[0].crop_box.unwrap();
        assert_eq!((crop.x, crop.y, crop.w, crop.h), (100, 100, 600, 600));

        // Cover: has cropBox AND cropBoxAutoDetect — should use the direct cropBox
        let crop = pages[1].crop_box.unwrap();
        assert_eq!((crop.x, crop.y, crop.w, crop.h), (390, 1085, 3153, 3975));

        // Title: normal cropBox
        let crop = pages[5].crop_box.unwrap();
        assert_eq!((crop.x, crop.y, crop.w, crop.h), (347, 1347, 2729, 3669));
    }

    #[test]
    fn find_pages_by_type_case_insensitive() {
        let pages = parse_scandata(SCANDATA_FIXTURE).unwrap();

        let covers = find_pages_by_type(&pages, "cover");
        assert_eq!(covers.len(), 2);
        assert_eq!(pages[covers[0]].leaf_num, 1);
        assert_eq!(pages[covers[1]].leaf_num, 28);

        let covers_upper = find_pages_by_type(&pages, "Cover");
        assert_eq!(covers_upper, covers);

        let titles = find_pages_by_type(&pages, "title");
        assert_eq!(titles.len(), 1);
        assert_eq!(pages[titles[0]].leaf_num, 5);

        let normals = find_pages_by_type(&pages, "Normal");
        assert_eq!(normals.len(), 5);

        let color_cards = find_pages_by_type(&pages, "color card");
        assert_eq!(color_cards.len(), 2);
    }

    #[test]
    fn find_pages_no_match() {
        let pages = parse_scandata(SCANDATA_FIXTURE).unwrap();
        let result = find_pages_by_type(&pages, "Copyright");
        assert!(result.is_empty());
    }

    #[test]
    fn leaf_to_zip_path_4_digit() {
        let entries = vec![
            "item_jp2/item_0000.jp2".to_string(),
            "item_jp2/item_0001.jp2".to_string(),
            "item_jp2/item_0005.jp2".to_string(),
            "item_jp2/item_0028.jp2".to_string(),
        ];

        assert_eq!(
            leaf_to_zip_path(0, &entries),
            Some("item_jp2/item_0000.jp2".to_string())
        );
        assert_eq!(
            leaf_to_zip_path(5, &entries),
            Some("item_jp2/item_0005.jp2".to_string())
        );
        assert_eq!(
            leaf_to_zip_path(28, &entries),
            Some("item_jp2/item_0028.jp2".to_string())
        );
        assert_eq!(leaf_to_zip_path(99, &entries), None);
    }

    #[test]
    fn leaf_to_zip_path_various_prefixes() {
        let entries = vec![
            "mybook_jp2/mybook_0003.jp2".to_string(),
            "other_dir/page_0010.jp2".to_string(),
        ];

        assert_eq!(
            leaf_to_zip_path(3, &entries),
            Some("mybook_jp2/mybook_0003.jp2".to_string())
        );
        assert_eq!(
            leaf_to_zip_path(10, &entries),
            Some("other_dir/page_0010.jp2".to_string())
        );
    }

    #[test]
    fn parse_empty_scandata_returns_error() {
        let result = parse_scandata("<book><pageData></pageData></book>");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no valid pages"));
    }

    #[test]
    fn parse_page_without_page_type_is_skipped() {
        let xml = r#"<book><pageData>
            <page leafNum="0">
                <rotateDegree>0</rotateDegree>
            </page>
            <page leafNum="1">
                <pageType>Normal</pageType>
                <rotateDegree>0</rotateDegree>
            </page>
        </pageData></book>"#;
        let pages = parse_scandata(xml).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].leaf_num, 1);
    }

    #[test]
    fn parse_page_without_crop_box() {
        let xml = r#"<book><pageData>
            <page leafNum="0">
                <pageType>Normal</pageType>
                <rotateDegree>0</rotateDegree>
            </page>
        </pageData></book>"#;
        let pages = parse_scandata(xml).unwrap();
        assert!(pages[0].crop_box.is_none());
    }

    fn test_file(name: &str, format: &str) -> FileMetadata {
        FileMetadata {
            name: name.to_string(),
            source: None,
            format: Some(format.to_string()),
            md5: None,
            size: None,
            mtime: None,
            sha1: None,
            crc32: None,
            original: None,
            rotation: None,
            extra: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn find_scandata_file_by_name() {
        let files = vec![
            test_file("item_meta.xml", "Metadata"),
            test_file("myitem_scandata.xml", "Scandata"),
        ];

        assert_eq!(
            find_scandata_file("myitem", &files),
            Some("myitem_scandata.xml".to_string())
        );
    }

    #[test]
    fn find_scandata_file_by_format_fallback() {
        let files = vec![test_file("weird_name_sd.xml", "Scandata")];

        assert_eq!(
            find_scandata_file("item", &files),
            Some("weird_name_sd.xml".to_string())
        );
    }

    #[test]
    fn find_scandata_file_none() {
        let files = vec![test_file("item_meta.xml", "Metadata")];

        assert_eq!(find_scandata_file("item", &files), None);
    }

    /// Simulate the extractor's behavior for the uoftgovpubs config:
    /// pageInfo: [cover, title, other, normal:5]
    ///
    /// Expected: covers (leaf 1, 28) + title (leaf 5) + first 5 normals (2,3,4,6,7)
    /// After dedup+sort: [1, 2, 3, 4, 5, 6, 7, 28]
    #[test]
    fn extractor_behavior_uoftgovpubs_config() {
        let pages = parse_scandata(SCANDATA_FIXTURE).unwrap();

        // Simulate get_pagetype_pages for each spec entry
        let mut all_indices: Vec<usize> = Vec::new();

        // {"type": "cover"} — no count limit → all covers
        let covers = find_pages_by_type(&pages, "cover");
        all_indices.extend(&covers);

        // {"type": "title"} — no count limit → all titles
        let titles = find_pages_by_type(&pages, "title");
        all_indices.extend(&titles);

        // {"type": "other"} — no match
        let others = find_pages_by_type(&pages, "other");
        assert!(others.is_empty());

        // {"type": "normal", "count": 5} — first 5
        let normals = find_pages_by_type(&pages, "normal");
        all_indices.extend(&normals[..5.min(normals.len())]);

        // Dedup + sort (matching extractor behavior)
        all_indices.sort_unstable();
        all_indices.dedup();

        let leaf_nums: Vec<u32> = all_indices.iter().map(|&i| pages[i].leaf_num).collect();
        assert_eq!(leaf_nums, vec![1, 2, 3, 4, 5, 6, 7, 28]);
    }
}
