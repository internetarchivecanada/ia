use slint::ComponentHandle;

/// Runs an async operation on the tokio runtime and sends the result
/// back to the Slint event loop.
///
/// - `handle` — weak reference to the Slint component
/// - `runtime` — tokio runtime handle
/// - `task` — async closure that produces a result
/// - `on_done` — closure that runs on Slint event loop with the result
#[allow(dead_code)]
pub fn spawn_async<T, F, Fut, O, D>(
    handle: &slint::Weak<T>,
    runtime: &tokio::runtime::Handle,
    task: F,
    on_done: D,
) where
    T: ComponentHandle + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<O>> + Send,
    O: Send + 'static,
    D: FnOnce(&T, anyhow::Result<O>) + Send + 'static,
{
    let weak = handle.clone();
    runtime.spawn(async move {
        let result = task().await;
        slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                on_done(&app, result);
            }
        })
        .ok();
    });
}
