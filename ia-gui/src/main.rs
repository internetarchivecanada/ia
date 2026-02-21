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

    let _backend = backend::AppBackend::new(client);
    let _rt = runtime.handle().clone();

    // Create and run Slint app
    let app = AppWindow::new()?;
    app.run()?;

    Ok(())
}
