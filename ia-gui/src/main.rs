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
        download_dir,
    ));

    // Create and run Slint app
    let app = AppWindow::new()?;

    // Wire up backends
    app_backend.setup_search(&app, runtime.handle());
    app_backend.setup_export(&app);
    app_backend.setup_metadata_fetch(&app, runtime.handle());

    // Wire download button
    {
        let dm = std::sync::Arc::clone(&download_manager);
        app.on_item_detail_download(move |identifier| {
            dm.queue_download(&identifier);
        });
    }

    // Timer to poll download progress and update UI
    {
        let dm = std::sync::Arc::clone(&download_manager);
        let weak = app.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(500),
            move || {
                let downloads = dm.get_downloads();
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
