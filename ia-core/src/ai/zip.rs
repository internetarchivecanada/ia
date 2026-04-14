//! AI-specific page selection from zip archives.
//!
//! General-purpose zip member listing and download lives in
//! [`crate::download::zip`]. This module adds AI-specific page selection
//! logic that maps an AI Config's `pageInfo` to specific image files.

// Re-export the core zip types and functions for convenience.
pub use crate::download::zip::{
    download_zip_member, download_zip_member_converted, find_jp2_zip, list_zip_contents, ZipEntry,
};

use crate::ai::ia_config::{PageInfo, PageType};
use crate::client::IaClient;
use crate::download::zip;

/// Build the download URL for a zip member with format conversion.
///
/// Returns a URL like `https://archive.org/download/{id}/{zip}/{member}&ext={ext}`
/// that triggers server-side conversion (e.g., JP2 → JPEG).
pub fn page_image_url(
    client: &IaClient,
    identifier: &str,
    zip_filename: &str,
    member_path: &str,
    ext: &str,
) -> String {
    let base =
        crate::download::zip::build_zip_member_url(client, identifier, zip_filename, member_path);
    format!("{base}&ext={ext}")
}

/// Select page image paths from zip entries based on a pageInfo specification.
///
/// Returns the member paths to download, in order. Filters to image files
/// (JP2, JPEG, PNG, TIFF) and sorts by name for deterministic page order.
pub fn select_pages(entries: &[zip::ZipEntry], page_info: &[PageInfo]) -> Vec<String> {
    // Filter to image files and sort by name
    let mut images: Vec<&zip::ZipEntry> = entries
        .iter()
        .filter(|e| {
            let lower = e.path.to_lowercase();
            lower.ends_with(".jp2")
                || lower.ends_with(".jpg")
                || lower.ends_with(".jpeg")
                || lower.ends_with(".png")
                || lower.ends_with(".tif")
                || lower.ends_with(".tiff")
        })
        .collect();

    images.sort_by(|a, b| a.path.cmp(&b.path));

    let mut selected = Vec::new();
    let mut idx = 0;

    for info in page_info {
        match info.page_type {
            PageType::Cover => {
                // Cover is always the first image (index 0)
                if let Some(entry) = images.first() {
                    selected.push(entry.path.clone());
                }
                idx = 1; // next normal pages start after cover
            }
            PageType::Title => {
                // Title page: take the next image at current index
                if let Some(entry) = images.get(idx) {
                    selected.push(entry.path.clone());
                }
                idx += 1;
            }
            PageType::Normal | PageType::Other => {
                let count = info.count.unwrap_or(1);
                for i in idx..idx + count {
                    if let Some(entry) = images.get(i) {
                        selected.push(entry.path.clone());
                    }
                }
                idx += count;
            }
        }
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ia_config::PageInfo;
    use crate::download::zip::ZipEntry;

    #[test]
    fn page_image_url_constructs_correct_url() {
        let config = crate::config::IaConfig::default();
        let client = crate::IaClient::from_config(config).unwrap();
        let url = page_image_url(
            &client,
            "test-item",
            "test-item_jp2.zip",
            "test-item_jp2/page_0001.jp2",
            "jpg",
        );
        assert!(url.contains("test-item"));
        assert!(url.contains("test-item_jp2.zip"));
        assert!(url.contains("page_0001.jp2"));
        assert!(url.ends_with("&ext=jpg"));
        assert!(url.starts_with("https://"));
    }

    #[test]
    fn select_pages_cover_and_normal() {
        let entries = vec![
            ZipEntry {
                path: "item_jp2/item_0000.jp2".into(),
                size: None,
                modified: None,
            },
            ZipEntry {
                path: "item_jp2/item_0001.jp2".into(),
                size: None,
                modified: None,
            },
            ZipEntry {
                path: "item_jp2/item_0002.jp2".into(),
                size: None,
                modified: None,
            },
            ZipEntry {
                path: "item_jp2/item_0003.jp2".into(),
                size: None,
                modified: None,
            },
        ];
        let page_info = vec![
            PageInfo {
                page_type: PageType::Cover,
                count: None,
            },
            PageInfo {
                page_type: PageType::Normal,
                count: Some(2),
            },
        ];
        let selected = select_pages(&entries, &page_info);
        assert_eq!(selected.len(), 3);
        assert_eq!(selected[0], "item_jp2/item_0000.jp2");
        assert_eq!(selected[1], "item_jp2/item_0001.jp2");
        assert_eq!(selected[2], "item_jp2/item_0002.jp2");
    }

    #[test]
    fn select_pages_normal_only() {
        let entries = vec![
            ZipEntry {
                path: "page_0000.jp2".into(),
                size: None,
                modified: None,
            },
            ZipEntry {
                path: "page_0001.jp2".into(),
                size: None,
                modified: None,
            },
        ];
        let page_info = vec![PageInfo {
            page_type: PageType::Normal,
            count: Some(1),
        }];
        let selected = select_pages(&entries, &page_info);
        assert_eq!(selected, vec!["page_0000.jp2"]);
    }

    #[test]
    fn select_pages_more_requested_than_available() {
        let entries = vec![ZipEntry {
            path: "page_0000.jp2".into(),
            size: None,
            modified: None,
        }];
        let page_info = vec![
            PageInfo {
                page_type: PageType::Cover,
                count: None,
            },
            PageInfo {
                page_type: PageType::Normal,
                count: Some(5),
            },
        ];
        assert_eq!(select_pages(&entries, &page_info).len(), 1);
    }

    #[test]
    fn select_pages_filters_non_images() {
        let entries = vec![
            ZipEntry {
                path: "manifest.txt".into(),
                size: None,
                modified: None,
            },
            ZipEntry {
                path: "page_0000.jp2".into(),
                size: None,
                modified: None,
            },
            ZipEntry {
                path: "page_0001.xml".into(),
                size: None,
                modified: None,
            },
        ];
        let page_info = vec![PageInfo {
            page_type: PageType::Cover,
            count: None,
        }];
        let selected = select_pages(&entries, &page_info);
        assert_eq!(selected, vec!["page_0000.jp2"]);
    }

    #[test]
    fn select_pages_empty_entries() {
        let page_info = vec![PageInfo {
            page_type: PageType::Cover,
            count: None,
        }];
        assert!(select_pages(&[], &page_info).is_empty());
    }
}
