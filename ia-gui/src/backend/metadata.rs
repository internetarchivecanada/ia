use crate::backend::AppBackend;
use crate::{AppWindow, FileEntryData, ItemDetailData};
use ia_core::IaClient;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::sync::Arc;

pub struct FetchResult {
    pub detail: ItemDetailData,
    pub files: Vec<FileEntryData>,
}

impl AppBackend {
    pub fn setup_metadata_fetch(&self, app: &AppWindow, runtime: &tokio::runtime::Handle) {
        let client = Arc::clone(&self.client);
        let rt = runtime.clone();
        let weak = app.as_weak();

        app.on_search_result_clicked(move |index| {
            let weak = weak.clone();
            let client = Arc::clone(&client);

            // Get the identifier from current search results
            let identifier = {
                if let Some(app) = weak.upgrade() {
                    let results = app.get_search_results();
                    if let Some(result) =
                        slint::Model::row_data(&results, index as usize)
                    {
                        // Show immediately with search result data while loading
                        app.set_item_detail(ItemDetailData {
                            identifier: result.identifier.clone(),
                            title: result.title.clone(),
                            mediatype: result.mediatype.clone(),
                            description: result.description.clone(),
                            creator: SharedString::default(),
                            date: SharedString::default(),
                            collections: SharedString::default(),
                            file_count: 0,
                            total_size: SharedString::default(),
                            json_text: SharedString::default(),
                        });
                        app.set_item_detail_files(ModelRc::new(VecModel::default()));
                        app.set_item_detail_loading(true);
                        app.set_showing_item_detail(true);
                        result.identifier.to_string()
                    } else {
                        return;
                    }
                } else {
                    return;
                }
            };

            rt.spawn(async move {
                let result = fetch_metadata(&client, &identifier).await;

                slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_item_detail_loading(false);
                        match result {
                            Ok(fetched) => {
                                app.set_item_detail(fetched.detail);
                                app.set_item_detail_files(ModelRc::new(
                                    VecModel::from(fetched.files),
                                ));
                            }
                            Err(e) => {
                                let mut current = app.get_item_detail();
                                current.title = SharedString::from(format!(
                                    "Error loading: {}",
                                    e
                                ));
                                app.set_item_detail(current);
                            }
                        }
                    }
                })
                .ok();
            });
        });
    }
}

async fn fetch_metadata(
    client: &IaClient,
    identifier: &str,
) -> anyhow::Result<FetchResult> {
    let meta = ia_core::metadata::get(client, identifier).await?;
    Ok(fetch_metadata_from_item(&meta))
}

/// Convert an already-fetched ItemMetadata into UI data types.
pub fn fetch_metadata_from_item(meta: &ia_core::types::ItemMetadata) -> FetchResult {
    let identifier = meta.metadata.identifier.as_deref().unwrap_or("");
    let title = meta
        .metadata
        .title
        .as_ref()
        .map(|v| v.first().to_string())
        .unwrap_or_default();
    let description = meta
        .metadata
        .description
        .as_ref()
        .map(|v| v.first().to_string())
        .unwrap_or_default();
    let mediatype = meta.metadata.mediatype.clone().unwrap_or_default();
    let creator = meta
        .metadata
        .creator
        .as_ref()
        .map(|v| v.first().to_string())
        .unwrap_or_default();
    let date = meta.metadata.date.clone().unwrap_or_default();
    let collections = meta
        .metadata
        .collection
        .as_ref()
        .map(|v| match v {
            ia_core::types::StringOrVec::Single(s) => s.clone(),
            ia_core::types::StringOrVec::Multiple(v) => v.join(", "),
        })
        .unwrap_or_default();

    let file_count = meta.files.len() as i32;
    let total_bytes: u64 = meta.files.iter().filter_map(|f| f.size).sum();
    let total_size = format_bytes(total_bytes);

    // Build file entries
    let files: Vec<FileEntryData> = meta
        .files
        .iter()
        .map(|f| FileEntryData {
            name: SharedString::from(&f.name),
            format: SharedString::from(f.format.as_deref().unwrap_or("")),
            size: SharedString::from(format_bytes(f.size.unwrap_or(0))),
            source: SharedString::from(f.source.as_deref().unwrap_or("")),
        })
        .collect();

    // Pretty-print the raw JSON
    let json_text = serde_json::to_string_pretty(&meta).unwrap_or_default();

    FetchResult {
        detail: ItemDetailData {
            identifier: SharedString::from(identifier),
            title: SharedString::from(title),
            mediatype: SharedString::from(mediatype),
            description: SharedString::from(description),
            creator: SharedString::from(creator),
            date: SharedString::from(date),
            collections: SharedString::from(collections),
            file_count,
            total_size: SharedString::from(total_size),
            json_text: SharedString::from(json_text),
        },
        files,
    }
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2.00 GB");
    }
}
