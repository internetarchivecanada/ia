use crate::backend::AppBackend;
use crate::{AppWindow, SearchResultData};
use futures::StreamExt;
use ia_core::search::{SearchOpts, SearchResult};
use ia_core::IaClient;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::sync::Arc;

impl AppBackend {
    pub fn setup_search(&self, app: &AppWindow, runtime: &tokio::runtime::Handle) {
        let client = Arc::clone(&self.client);
        let rt = runtime.clone();
        let weak = app.as_weak();

        app.on_search_requested(move |query, backend| {
            let client = Arc::clone(&client);
            let weak = weak.clone();
            let query = query.to_string();
            let backend = backend.to_string();

            // Set searching state
            if let Some(app) = weak.upgrade() {
                app.set_search_in_progress(true);
                app.set_search_status(SharedString::from("Searching..."));
                app.set_search_results(ModelRc::new(VecModel::default()));
            }

            rt.spawn(async move {
                let result = run_search(&client, &query, &backend).await;

                slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_search_in_progress(false);
                        match result {
                            Ok(results) => {
                                let count = results.len();
                                let model = VecModel::from(results);
                                app.set_search_results(ModelRc::new(model));
                                app.set_search_status(SharedString::from(format!(
                                    "{count} results"
                                )));
                            }
                            Err(e) => {
                                app.set_search_status(SharedString::from(format!("Error: {e}")));
                            }
                        }
                    }
                })
                .ok();
            });
        });
    }
}

async fn run_search(
    client: &IaClient,
    query: &str,
    backend: &str,
) -> anyhow::Result<Vec<SearchResultData>> {
    let opts = SearchOpts {
        fields: vec![
            "identifier".into(),
            "title".into(),
            "mediatype".into(),
            "description".into(),
        ],
        count: 50,
        ..Default::default()
    };

    let mut stream = match backend {
        "Advanced" => ia_core::search::advanced(client, query, &opts),
        "Full-Text" => ia_core::search::fts(client, query, &opts),
        _ => ia_core::search::scrape(client, query, &opts),
    };

    let mut results = Vec::new();
    while let Some(item) = stream.next().await {
        let item = item?;
        results.push(search_result_to_slint(&item));
    }

    Ok(results)
}

fn search_result_to_slint(result: &SearchResult) -> SearchResultData {
    let title = result
        .fields
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mediatype = result
        .fields
        .get("mediatype")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = result
        .fields
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    SearchResultData {
        identifier: SharedString::from(&result.identifier),
        title: SharedString::from(title),
        mediatype: SharedString::from(mediatype),
        description: SharedString::from(description),
    }
}
