mod backend;

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("ia_gui=info".parse()?),
        )
        .init();

    // Start tokio runtime in background
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Create IaClient
    let config = ia_core::IaConfig::load().unwrap_or_default();
    let client = ia_core::IaClient::from_config(config)?;

    let app_backend = backend::AppBackend::new(client);

    // Create download manager
    let download_dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let download_manager = std::sync::Arc::new(backend::downloads::DownloadManager::new(
        std::sync::Arc::clone(&app_backend.client),
        runtime.handle().clone(),
        2, // concurrency
        download_dir.clone(),
    ));

    // Create list manager
    let list_manager = std::sync::Arc::new(backend::lists::ListManager::new(
        backend::lists::ListManager::default_dir(),
    ));

    // Create and run Slint app
    let app = AppWindow::new()?;

    // Wire up backends
    app_backend.setup_search(&app, runtime.handle());
    app_backend.setup_export(&app);
    app_backend.setup_metadata_fetch(&app, runtime.handle());

    // Load download history from job log
    let joblog_path = backend::history::default_joblog_path();
    {
        let history = backend::history::load_history(&joblog_path);
        app.set_download_history(slint::ModelRc::new(slint::VecModel::from(history)));
    }

    // Load initial lists
    refresh_lists(&app, &list_manager);

    // Wire retry-failed and clear-history callbacks
    {
        let dm = std::sync::Arc::clone(&download_manager);
        let path = joblog_path.clone();
        let weak = app.as_weak();
        app.on_downloads_retry_failed(move || {
            let failed = backend::history::failed_identifiers(&path);
            for id in &failed {
                dm.queue_download(id);
            }
        });

        let weak2 = weak.clone();
        app.on_downloads_clear_history(move || {
            if let Some(app) = weak2.upgrade() {
                app.set_download_history(slint::ModelRc::new(slint::VecModel::default()));
            }
        });
    }

    // Wire download button
    {
        let dm = std::sync::Arc::clone(&download_manager);
        app.on_item_detail_download(move |identifier| {
            dm.queue_download(&identifier);
        });
    }

    // Wire "Add to List" from item detail — adds to "Favorites" list
    {
        let lm = std::sync::Arc::clone(&list_manager);
        let weak = app.as_weak();
        app.on_item_detail_add_to_list(move |identifier| {
            let id = identifier.to_string();
            let _ = lm.add_identifiers("Favorites", &[id]);
            if let Some(app) = weak.upgrade() {
                refresh_lists(&app, &lm);
            }
        });
    }

    // Wire list callbacks
    {
        let lm = std::sync::Arc::clone(&list_manager);
        let weak = app.as_weak();
        app.on_lists_create(move |name| {
            let name = name.to_string();
            if !name.is_empty() {
                let _ = lm.create(&name);
                if let Some(app) = weak.upgrade() {
                    refresh_lists(&app, &lm);
                }
            }
        });
    }
    {
        let lm = std::sync::Arc::clone(&list_manager);
        let weak = app.as_weak();
        app.on_lists_delete(move |name| {
            let name = name.to_string();
            let _ = lm.delete(&name);
            if let Some(app) = weak.upgrade() {
                app.set_selected_list_name(slint::SharedString::default());
                app.set_selected_list_items(slint::ModelRc::new(slint::VecModel::default()));
                refresh_lists(&app, &lm);
            }
        });
    }
    {
        let lm = std::sync::Arc::clone(&list_manager);
        let weak = app.as_weak();
        app.on_lists_select(move |name| {
            let name = name.to_string();
            if let Some(app) = weak.upgrade() {
                app.set_selected_list_name(slint::SharedString::from(&name));
                if let Some(list) = lm.load(&name) {
                    let items: Vec<ListItemData> = list
                        .identifiers
                        .iter()
                        .map(|id| ListItemData {
                            identifier: slint::SharedString::from(id.as_str()),
                            title: slint::SharedString::default(),
                        })
                        .collect();
                    app.set_selected_list_items(slint::ModelRc::new(slint::VecModel::from(items)));
                }
            }
        });
    }
    {
        let lm = std::sync::Arc::clone(&list_manager);
        let weak = app.as_weak();
        app.on_lists_remove_item(move |list_name, identifier| {
            let list_name = list_name.to_string();
            let identifier = identifier.to_string();
            if let Some(mut list) = lm.load(&list_name) {
                list.identifiers.retain(|id| id != &identifier);
                let _ = lm.save(&list);
                if let Some(app) = weak.upgrade() {
                    refresh_lists(&app, &lm);
                    // Re-select the list to refresh items
                    let items: Vec<ListItemData> = list
                        .identifiers
                        .iter()
                        .map(|id| ListItemData {
                            identifier: slint::SharedString::from(id.as_str()),
                            title: slint::SharedString::default(),
                        })
                        .collect();
                    app.set_selected_list_items(slint::ModelRc::new(slint::VecModel::from(items)));
                }
            }
        });
    }
    {
        let dm = std::sync::Arc::clone(&download_manager);
        let lm = std::sync::Arc::clone(&list_manager);
        app.on_lists_download(move |name| {
            let name = name.to_string();
            if let Some(list) = lm.load(&name) {
                for id in &list.identifiers {
                    dm.queue_download(id);
                }
            }
        });
    }
    {
        let lm = std::sync::Arc::clone(&list_manager);
        app.on_lists_export(move |name| {
            let name = name.to_string();
            if let Some(list) = lm.load(&name) {
                let records: Vec<backend::export::ExportRecord> = list
                    .identifiers
                    .iter()
                    .map(|id| backend::export::ExportRecord {
                        identifier: id.clone(),
                        title: String::new(),
                        mediatype: String::new(),
                        description: String::new(),
                    })
                    .collect();
                let content =
                    backend::export::format_records(&records, backend::export::ExportFormat::Identifiers);
                let export_path = std::env::temp_dir().join(format!("{name}-export.txt"));
                let _ = std::fs::write(&export_path, content);
                tracing::info!("Exported list '{}' to {}", name, export_path.display());
            }
        });
    }
    // Wire lists-import (placeholder - logs a message since we can't open native file dialog without additional deps)
    {
        app.on_lists_import(move |name| {
            tracing::info!(
                "Import requested for list '{}' - file dialog not yet implemented",
                name
            );
        });
    }
    // Wire lists-item-clicked to open item detail
    {
        let weak = app.as_weak();
        let client = std::sync::Arc::clone(&app_backend.client);
        let handle = runtime.handle().clone();
        app.on_lists_item_clicked(move |identifier| {
            let identifier = identifier.to_string();
            let weak = weak.clone();
            let client = std::sync::Arc::clone(&client);
            if let Some(app) = weak.upgrade() {
                app.set_item_detail_loading(true);
                app.set_showing_item_detail(true);
            }
            let weak2 = weak.clone();
            handle.spawn(async move {
                match ia_core::metadata::get(&client, &identifier).await {
                    Ok(meta) => {
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(app) = weak2.upgrade() {
                                let result =
                                    backend::metadata::fetch_metadata_from_item(&meta);
                                app.set_item_detail(result.detail);
                                app.set_item_detail_files(slint::ModelRc::new(
                                    slint::VecModel::from(result.files),
                                ));
                                app.set_item_detail_loading(false);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Failed to fetch metadata for {}: {}", identifier, e);
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(app) = weak2.upgrade() {
                                app.set_item_detail_loading(false);
                            }
                        });
                    }
                }
            });
        });
    }

    // Wire metadata page
    {
        let weak = app.as_weak();
        let client = std::sync::Arc::clone(&app_backend.client);
        let handle = runtime.handle().clone();
        app.on_metadata_fetch_requested(move |identifier| {
            let identifier = identifier.to_string();
            let weak = weak.clone();
            let client = std::sync::Arc::clone(&client);
            if let Some(app) = weak.upgrade() {
                app.set_metadata_loading(true);
                app.set_metadata_has_result(false);
            }
            let weak2 = weak.clone();
            handle.spawn(async move {
                match ia_core::metadata::get(&client, &identifier).await {
                    Ok(meta) => {
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(app) = weak2.upgrade() {
                                let result =
                                    backend::metadata::fetch_metadata_from_item(&meta);
                                app.set_metadata_item(result.detail);
                                app.set_metadata_files(slint::ModelRc::new(
                                    slint::VecModel::from(result.files),
                                ));
                                app.set_metadata_loading(false);
                                app.set_metadata_has_result(true);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Failed to fetch metadata for {}: {}", identifier, e);
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(app) = weak2.upgrade() {
                                app.set_metadata_loading(false);
                            }
                        });
                    }
                }
            });
        });
    }
    {
        let dm = std::sync::Arc::clone(&download_manager);
        app.on_metadata_download(move |identifier| {
            dm.queue_download(&identifier);
        });
    }
    {
        let lm = std::sync::Arc::clone(&list_manager);
        let weak = app.as_weak();
        app.on_metadata_add_to_list(move |identifier| {
            let id = identifier.to_string();
            let _ = lm.add_identifiers("Favorites", &[id]);
            if let Some(app) = weak.upgrade() {
                refresh_lists(&app, &lm);
            }
        });
    }
    {
        app.on_metadata_save_json(move |identifier, json| {
            let id = identifier.to_string();
            let json = json.to_string();
            let save_dir = dirs::download_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let path = save_dir.join(format!("{id}-metadata.json"));
            match std::fs::write(&path, &json) {
                Ok(()) => tracing::info!("Saved metadata to {}", path.display()),
                Err(e) => tracing::error!("Failed to save metadata: {}", e),
            }
        });
    }

    // Track which downloads have already been added to the "Downloaded" list
    let tracked_downloads: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));

    // Timer to poll download progress and update UI
    {
        let dm = std::sync::Arc::clone(&download_manager);
        let lm = std::sync::Arc::clone(&list_manager);
        let tracked = std::sync::Arc::clone(&tracked_downloads);
        let weak = app.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(500),
            move || {
                let downloads = dm.get_downloads();

                // Auto-add completed downloads to "Downloaded" list
                {
                    let mut tracked = tracked.lock().unwrap();
                    for d in &downloads {
                        if d.status == backend::downloads::DownloadJobStatus::Complete
                            && !tracked.contains(&d.identifier)
                        {
                            let _ = lm.add_identifiers("Downloaded", &[d.identifier.clone()]);
                            tracked.insert(d.identifier.clone());
                            if let Some(app) = weak.upgrade() {
                                refresh_lists(&app, &lm);
                            }
                        }
                    }
                }

                if let Some(app) = weak.upgrade() {
                    let active: Vec<ActiveDownloadData> = downloads
                        .iter()
                        .map(|d| {
                            let progress = if d.bytes_total > 0 {
                                d.bytes_downloaded as f32 / d.bytes_total as f32
                            } else if d.files_total > 0 {
                                (d.files_completed + d.files_skipped + d.files_failed) as f32
                                    / d.files_total as f32
                            } else {
                                0.0
                            };
                            ActiveDownloadData {
                                identifier: slint::SharedString::from(&d.identifier),
                                status: slint::SharedString::from(match &d.status {
                                    backend::downloads::DownloadJobStatus::Queued => "Queued",
                                    backend::downloads::DownloadJobStatus::Downloading => {
                                        "Downloading"
                                    }
                                    backend::downloads::DownloadJobStatus::Complete => "Complete",
                                    backend::downloads::DownloadJobStatus::Failed => "Failed",
                                }),
                                files_completed: d.files_completed as i32,
                                files_total: d.files_total as i32,
                                bytes_downloaded: slint::SharedString::from(
                                    backend::metadata::format_bytes(d.bytes_downloaded),
                                ),
                                bytes_total: slint::SharedString::from(
                                    backend::metadata::format_bytes(d.bytes_total),
                                ),
                                progress,
                            }
                        })
                        .collect();
                    app.set_active_downloads(slint::ModelRc::new(slint::VecModel::from(active)));
                }
            },
        );
        // Keep timer alive by leaking it (it's for the lifetime of the app)
        std::mem::forget(timer);
    }

    app.run()?;
    Ok(())
}

fn refresh_lists(app: &AppWindow, lm: &backend::lists::ListManager) {
    let names = lm.list_names();
    let summaries: Vec<ListSummaryData> = names
        .iter()
        .map(|name| {
            let count = lm
                .load(name)
                .map(|l| l.identifiers.len())
                .unwrap_or(0);
            ListSummaryData {
                name: slint::SharedString::from(name.as_str()),
                item_count: count as i32,
                is_builtin: name == "Downloaded",
            }
        })
        .collect();
    app.set_lists(slint::ModelRc::new(slint::VecModel::from(summaries)));
}
