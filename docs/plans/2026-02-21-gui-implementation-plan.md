# ia GUI (Slint Desktop Application) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a native desktop GUI for the Internet Archive CLI using Slint, targeting non-technical librarians and archivists.

**Architecture:** New `ia-gui` crate in the workspace, consuming `ia-core` the same way `ia-cli` does. Slint `.slint` files define the UI (compiled at build time), Rust backend bridges ia-core to Slint via properties (data down) and callbacks (events up). Tokio runtime runs in background thread; results pushed to Slint event loop via `slint::invoke_from_event_loop`.

**Tech Stack:** Slint 1.9+, ia-core (workspace), tokio 1, serde/serde_json, anyhow, dirs (platform data dirs), reqwest (thumbnail fetching via ia-core's client)

**Design Doc:** `docs/plans/2026-02-21-gui-design.md`

---

## Milestone 1: Foundation (Tasks 1-3)

### Task 1: Scaffold ia-gui Crate

**Files:**
- Create: `ia-gui/Cargo.toml`
- Create: `ia-gui/build.rs`
- Create: `ia-gui/src/main.rs`
- Create: `ia-gui/ui/app.slint`
- Modify: `Cargo.toml` (workspace root, add `ia-gui` to members)

**Step 1: Add ia-gui to workspace**

In root `Cargo.toml`, add `"ia-gui"` to `workspace.members`:

```toml
[workspace]
members = ["ia-core", "ia-cli", "ia-gui"]
```

**Step 2: Create ia-gui/Cargo.toml**

```toml
[package]
name = "ia-gui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[[bin]]
name = "ia-gui"
path = "src/main.rs"

[dependencies]
ia-core = { path = "../ia-core" }
slint = "1.9"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "fs"] }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
dirs = "6"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[build-dependencies]
slint-build = "1.9"
```

**Step 3: Create ia-gui/build.rs**

```rust
fn main() {
    slint_build::compile("ui/app.slint").unwrap();
}
```

**Step 4: Create minimal ia-gui/ui/app.slint**

```slint
export component AppWindow inherits Window {
    title: "Internet Archive";
    preferred-width: 1024px;
    preferred-height: 768px;

    Text {
        text: "Internet Archive";
        font-size: 24px;
    }
}
```

**Step 5: Create ia-gui/src/main.rs**

```rust
slint::include_modules!();

fn main() -> anyhow::Result<()> {
    let app = AppWindow::new()?;
    app.run()?;
    Ok(())
}
```

**Step 6: Build and verify**

Run: `cargo build -p ia-gui`
Expected: Compiles successfully, no errors.

Run: `cargo run -p ia-gui` (briefly, Ctrl+C to quit)
Expected: A window opens titled "Internet Archive" with text displayed.

**Step 7: Commit**

```bash
git add ia-gui/ Cargo.toml
git commit -m "feat(gui): scaffold ia-gui crate with Slint"
```

---

### Task 2: Sidebar Navigation Shell

**Files:**
- Create: `ia-gui/ui/components/sidebar.slint`
- Modify: `ia-gui/ui/app.slint`
- Modify: `ia-gui/src/main.rs`

**Step 1: Create sidebar component**

Create `ia-gui/ui/components/sidebar.slint`:

```slint
import { VerticalBox, Button } from "std-widgets.slint";

export enum Page {
    search,
    downloads,
    lists,
    metadata,
    settings,
}

component SidebarItem inherits Rectangle {
    in property <string> label;
    in property <bool> active: false;
    callback clicked();

    height: 40px;
    background: active ? #e0e7ff : transparent;
    border-radius: 6px;

    HorizontalLayout {
        padding-left: 12px;
        alignment: start;

        Text {
            text: label;
            font-size: 14px;
            font-weight: active ? 700 : 400;
            vertical-alignment: center;
            color: active ? #4338ca : #374151;
        }
    }

    TouchArea {
        clicked => { root.clicked(); }
    }
}

export component Sidebar inherits Rectangle {
    in-out property <Page> current-page: Page.search;

    width: 200px;
    background: #f9fafb;

    VerticalBox {
        padding: 12px;
        spacing: 4px;

        Text {
            text: "Internet Archive";
            font-size: 16px;
            font-weight: 700;
            color: #111827;
            horizontal-alignment: left;
        }

        Rectangle { height: 16px; }

        SidebarItem {
            label: "Search";
            active: current-page == Page.search;
            clicked => { current-page = Page.search; }
        }
        SidebarItem {
            label: "Downloads";
            active: current-page == Page.downloads;
            clicked => { current-page = Page.downloads; }
        }
        SidebarItem {
            label: "Lists";
            active: current-page == Page.lists;
            clicked => { current-page = Page.lists; }
        }
        SidebarItem {
            label: "Metadata";
            active: current-page == Page.metadata;
            clicked => { current-page = Page.metadata; }
        }

        Rectangle { vertical-stretch: 1; }

        SidebarItem {
            label: "Settings";
            active: current-page == Page.settings;
            clicked => { current-page = Page.settings; }
        }
    }
}
```

**Step 2: Update app.slint with sidebar and content area**

Replace `ia-gui/ui/app.slint`:

```slint
import { Sidebar, Page } from "components/sidebar.slint";

export { Page }

export component AppWindow inherits Window {
    title: "Internet Archive";
    preferred-width: 1024px;
    preferred-height: 768px;
    min-width: 800px;
    min-height: 600px;

    in-out property <Page> current-page <=> sidebar.current-page;

    HorizontalLayout {
        sidebar := Sidebar {}

        Rectangle {
            background: white;
            horizontal-stretch: 1;

            // Placeholder content per page
            if current-page == Page.search : Text {
                text: "Search";
                font-size: 20px;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
            if current-page == Page.downloads : Text {
                text: "Downloads";
                font-size: 20px;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
            if current-page == Page.lists : Text {
                text: "Lists";
                font-size: 20px;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
            if current-page == Page.metadata : Text {
                text: "Metadata";
                font-size: 20px;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
            if current-page == Page.settings : Text {
                text: "Settings";
                font-size: 20px;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
        }
    }
}
```

**Step 3: Build and verify**

Run: `cargo build -p ia-gui`
Expected: Compiles.

Run: `cargo run -p ia-gui`
Expected: Window with sidebar on left. Clicking sidebar items changes the content area text.

**Step 4: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add sidebar navigation with page routing"
```

---

### Task 3: Async Backend Infrastructure

**Files:**
- Create: `ia-gui/src/backend/mod.rs`
- Create: `ia-gui/src/backend/state.rs`
- Modify: `ia-gui/src/main.rs`

This task sets up the tokio runtime integration and the pattern all pages will use to communicate between Slint UI and async Rust code.

**Step 1: Create backend module**

Create `ia-gui/src/backend/mod.rs`:

```rust
pub mod state;

use ia_core::IaClient;
use std::sync::Arc;

/// Shared application state accessible from async tasks.
pub struct AppBackend {
    pub client: Arc<IaClient>,
}

impl AppBackend {
    pub fn new(client: IaClient) -> Self {
        Self {
            client: Arc::new(client),
        }
    }
}
```

**Step 2: Create state module with channel-based communication pattern**

Create `ia-gui/src/backend/state.rs`:

```rust
use slint::ComponentHandle;

/// Runs an async operation on the tokio runtime and sends the result
/// back to the Slint event loop.
///
/// `handle` - weak reference to the Slint component
/// `runtime` - tokio runtime handle
/// `task` - async closure that produces a result
/// `on_done` - closure that runs on Slint event loop with the result
pub fn spawn_async<T, F, Fut, D>(
    handle: &slint::Weak<T>,
    runtime: &tokio::runtime::Handle,
    task: F,
    on_done: D,
) where
    T: ComponentHandle + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<D::Output>> + Send,
    D: ResultHandler<T> + Send + 'static,
    D::Output: Send + 'static,
{
    let weak = handle.clone();
    runtime.spawn(async move {
        let result = task().await;
        slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                on_done.handle(&app, result);
            }
        })
        .ok();
    });
}

/// Trait for handling async results back on the UI thread.
pub trait ResultHandler<T: ComponentHandle> {
    type Output;
    fn handle(self, app: &T, result: anyhow::Result<Self::Output>);
}

/// Simple callback-based result handler.
pub struct Callback<T: ComponentHandle, O> {
    pub callback: Box<dyn FnOnce(&T, anyhow::Result<O>) + 'static>,
}

impl<T: ComponentHandle, O> ResultHandler<T> for Callback<T, O> {
    type Output = O;
    fn handle(self, app: &T, result: anyhow::Result<O>) {
        (self.callback)(app, result);
    }
}
```

**Step 3: Update main.rs with tokio runtime and IaClient**

Replace `ia-gui/src/main.rs`:

```rust
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
    let client = runtime.block_on(async {
        ia_core::IaClient::from_config(config)
    })?;

    let _backend = backend::AppBackend::new(client);
    let _rt = runtime.handle().clone();

    // Create and run Slint app
    let app = AppWindow::new()?;
    app.run()?;

    Ok(())
}
```

**Step 4: Build and verify**

Run: `cargo build -p ia-gui`
Expected: Compiles. The backend infrastructure is in place but not yet wired to UI.

Run: `cargo run -p ia-gui`
Expected: Window opens as before. No visible change, but tokio + IaClient are initialized.

**Step 5: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add async backend infrastructure with tokio + IaClient"
```

---

## Milestone 2: Search Page (Tasks 4-7)

### Task 4: Search Page UI Shell

**Files:**
- Create: `ia-gui/ui/pages/search.slint`
- Modify: `ia-gui/ui/app.slint`

**Step 1: Create search page layout**

Create `ia-gui/ui/pages/search.slint`:

```slint
import { VerticalBox, HorizontalBox, LineEdit, Button, ComboBox, StandardListView } from "std-widgets.slint";

export struct SearchResultData {
    identifier: string,
    title: string,
    mediatype: string,
    description: string,
}

export component SearchPage inherits Rectangle {
    in property <[SearchResultData]> results: [];
    in property <string> status-text: "";
    in property <bool> searching: false;

    callback search-requested(/* query */ string, /* backend */ string);
    callback result-clicked(/* index */ int);
    callback export-requested();

    VerticalBox {
        padding: 24px;
        spacing: 16px;

        // Search bar
        HorizontalBox {
            spacing: 8px;

            query-input := LineEdit {
                horizontal-stretch: 1;
                placeholder-text: "Search archive.org...";
                accepted => {
                    root.search-requested(self.text, backend-selector.current-value);
                }
            }

            backend-selector := ComboBox {
                width: 120px;
                model: ["Scrape", "Advanced", "Full-Text"];
                current-value: "Scrape";
            }

            Button {
                text: searching ? "Searching..." : "Search";
                enabled: !searching && query-input.text != "";
                clicked => {
                    root.search-requested(query-input.text, backend-selector.current-value);
                }
            }
        }

        // Status line
        if status-text != "" : Text {
            text: status-text;
            font-size: 12px;
            color: #6b7280;
        }

        // Results list
        Flickable {
            vertical-stretch: 1;

            VerticalBox {
                spacing: 1px;

                for result[index] in results : Rectangle {
                    height: 72px;
                    background: touch.has-hover ? #f3f4f6 : transparent;
                    border-radius: 6px;

                    touch := TouchArea {
                        clicked => { root.result-clicked(index); }
                    }

                    HorizontalLayout {
                        padding: 12px;
                        spacing: 12px;

                        // Thumbnail placeholder
                        Rectangle {
                            width: 48px;
                            height: 48px;
                            background: #e5e7eb;
                            border-radius: 4px;

                            Text {
                                text: "📦";
                                font-size: 20px;
                                horizontal-alignment: center;
                                vertical-alignment: center;
                            }
                        }

                        VerticalLayout {
                            spacing: 4px;
                            alignment: center;

                            Text {
                                text: result.title != "" ? result.title : result.identifier;
                                font-size: 14px;
                                font-weight: 600;
                                overflow: elide;
                            }
                            Text {
                                text: result.identifier + " · " + result.mediatype;
                                font-size: 12px;
                                color: #6b7280;
                                overflow: elide;
                            }
                        }
                    }
                }
            }
        }

        // Bottom actions bar
        HorizontalBox {
            spacing: 8px;
            alignment: end;

            Button {
                text: "Export Results";
                enabled: results.length > 0;
                clicked => { root.export-requested(); }
            }
        }
    }
}
```

**Step 2: Wire search page into app.slint**

Replace `ia-gui/ui/app.slint`:

```slint
import { Sidebar, Page } from "components/sidebar.slint";
import { SearchPage, SearchResultData } from "pages/search.slint";

export { Page, SearchResultData }

export component AppWindow inherits Window {
    title: "Internet Archive";
    preferred-width: 1024px;
    preferred-height: 768px;
    min-width: 800px;
    min-height: 600px;

    in-out property <Page> current-page <=> sidebar.current-page;

    // Search state
    in property <[SearchResultData]> search-results: [];
    in property <string> search-status: "";
    in property <bool> search-in-progress: false;

    callback search-requested(string, string);
    callback search-result-clicked(int);
    callback search-export-requested();

    HorizontalLayout {
        sidebar := Sidebar {}

        Rectangle {
            background: white;
            horizontal-stretch: 1;

            if current-page == Page.search : SearchPage {
                results: search-results;
                status-text: search-status;
                searching: search-in-progress;
                search-requested(query, backend) => { root.search-requested(query, backend); }
                result-clicked(index) => { root.search-result-clicked(index); }
                export-requested => { root.search-export-requested(); }
            }
            if current-page == Page.downloads : Text {
                text: "Downloads";
                font-size: 20px;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
            if current-page == Page.lists : Text {
                text: "Lists";
                font-size: 20px;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
            if current-page == Page.metadata : Text {
                text: "Metadata";
                font-size: 20px;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
            if current-page == Page.settings : Text {
                text: "Settings";
                font-size: 20px;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
        }
    }
}
```

**Step 3: Build and verify**

Run: `cargo build -p ia-gui`
Expected: Compiles.

Run: `cargo run -p ia-gui`
Expected: Search page shows with query input, backend selector, and search button. No results yet (backend not wired).

**Step 4: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add search page UI shell"
```

---

### Task 5: Wire Search Backend

**Files:**
- Create: `ia-gui/src/backend/search.rs`
- Modify: `ia-gui/src/backend/mod.rs`
- Modify: `ia-gui/src/main.rs`

**Step 1: Create search backend**

Create `ia-gui/src/backend/search.rs`:

```rust
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
                                app.set_search_status(SharedString::from(
                                    format!("{count} results"),
                                ));
                            }
                            Err(e) => {
                                app.set_search_status(SharedString::from(
                                    format!("Error: {e}"),
                                ));
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
        sorts: vec![],
        count: 50, // Initial page
        timeout: None,
        params: vec![],
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
```

**Step 2: Update backend/mod.rs**

```rust
pub mod search;
pub mod state;

use ia_core::IaClient;
use std::sync::Arc;

pub struct AppBackend {
    pub client: Arc<IaClient>,
}

impl AppBackend {
    pub fn new(client: IaClient) -> Self {
        Self {
            client: Arc::new(client),
        }
    }
}
```

**Step 3: Wire up in main.rs**

Replace `ia-gui/src/main.rs`:

```rust
mod backend;

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("ia_gui=info".parse()?),
        )
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let config = ia_core::IaConfig::load().unwrap_or_default();
    let client = runtime.block_on(async { ia_core::IaClient::from_config(config) })?;

    let app_backend = backend::AppBackend::new(client);
    let app = AppWindow::new()?;

    // Wire up backends
    app_backend.setup_search(&app, runtime.handle());

    app.run()?;
    Ok(())
}
```

**Step 4: Build and verify**

Run: `cargo build -p ia-gui`
Expected: Compiles.

Run: `cargo run -p ia-gui`
Expected: Type a query (e.g., "nasa"), click Search. Results should appear after a moment. Status shows "Searching..." then "N results".

**Step 5: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): wire search backend to ia-core search API"
```

---

### Task 6: Search Thumbnails

**Files:**
- Modify: `ia-gui/ui/pages/search.slint` (add image support)
- Create: `ia-gui/src/backend/thumbnails.rs` (async thumbnail fetcher)
- Modify: `ia-gui/src/backend/mod.rs`
- Modify: `ia-gui/src/backend/search.rs`

**Step 1: Create thumbnail fetcher**

Create `ia-gui/src/backend/thumbnails.rs`:

```rust
use slint::Image;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Cache for downloaded thumbnails.
pub struct ThumbnailCache {
    cache: Arc<Mutex<HashMap<String, Image>>>,
}

impl ThumbnailCache {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Fetch a thumbnail for the given identifier.
    /// Returns cached version if available, otherwise downloads it.
    pub async fn get(
        &self,
        client: &reqwest::Client,
        host: &str,
        protocol: &str,
        identifier: &str,
    ) -> Option<Image> {
        // Check cache first
        {
            let cache = self.cache.lock().unwrap();
            if let Some(img) = cache.get(identifier) {
                return Some(img.clone());
            }
        }

        // Download thumbnail
        let url = format!("{protocol}://{host}/services/img/{identifier}");
        let bytes = client.get(&url).send().await.ok()?.bytes().await.ok()?;

        let image = Image::from_png_data(&bytes)
            .or_else(|_| Image::from_jpeg_data(&bytes))
            .ok()?;

        // Cache it
        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(identifier.to_string(), image.clone());
        }

        Some(image)
    }
}
```

Note: The exact Slint API for loading images from bytes may differ. The implementer should check Slint docs for `Image::load_from_path` or `Image::from_rgba8` and adapt. An alternative approach is to save thumbnails to a temp directory and load via file path.

**Step 2: Update search.slint to support thumbnail images**

In the search result component, change the thumbnail placeholder to support an `image` property on `SearchResultData`. This requires adding an `image` field to the struct or using a separate model. Since Slint structs can't hold `image` directly in `[SearchResultData]` easily, the simpler approach for MVP is to use a placeholder icon and add thumbnail support in a follow-up refinement.

For MVP, keep the emoji placeholder. Mark this for refinement in a later task.

**Step 3: Register thumbnail module**

Update `ia-gui/src/backend/mod.rs`:

```rust
pub mod search;
pub mod state;
pub mod thumbnails;

use ia_core::IaClient;
use std::sync::Arc;

pub struct AppBackend {
    pub client: Arc<IaClient>,
    pub thumbnails: thumbnails::ThumbnailCache,
}

impl AppBackend {
    pub fn new(client: IaClient) -> Self {
        Self {
            client: Arc::new(client),
            thumbnails: thumbnails::ThumbnailCache::new(),
        }
    }
}
```

**Step 4: Build and verify**

Run: `cargo build -p ia-gui`
Expected: Compiles. Thumbnail cache is initialized but not yet displayed (placeholder icons remain).

**Step 5: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add thumbnail cache infrastructure"
```

---

### Task 7: Search Result Actions (Export)

**Files:**
- Create: `ia-gui/src/backend/export.rs`
- Modify: `ia-gui/src/backend/mod.rs`
- Modify: `ia-gui/src/main.rs`

**Step 1: Create export module**

Create `ia-gui/src/backend/export.rs`:

```rust
use crate::SearchResultData;
use anyhow::Result;
use slint::ModelRc;
use std::path::Path;

pub enum ExportFormat {
    Jsonl,
    Csv,
    Identifiers,
}

pub fn export_results(
    results: &ModelRc<SearchResultData>,
    path: &Path,
    format: ExportFormat,
) -> Result<usize> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    let count = results.row_count();

    for i in 0..count {
        let item = results.row_data(i).unwrap();
        match format {
            ExportFormat::Jsonl => {
                let obj = serde_json::json!({
                    "identifier": item.identifier.as_str(),
                    "title": item.title.as_str(),
                    "mediatype": item.mediatype.as_str(),
                });
                serde_json::to_writer(&mut file, &obj)?;
                writeln!(file)?;
            }
            ExportFormat::Csv => {
                if i == 0 {
                    writeln!(file, "identifier,title,mediatype")?;
                }
                writeln!(
                    file,
                    "{},{},{}",
                    item.identifier, item.title, item.mediatype
                )?;
            }
            ExportFormat::Identifiers => {
                writeln!(file, "{}", item.identifier)?;
            }
        }
    }

    Ok(count)
}
```

**Step 2: Update mod.rs**

Add `pub mod export;` to `ia-gui/src/backend/mod.rs`.

**Step 3: Wire export callback in main.rs**

Add after the search setup:

```rust
// Wire export callback
let weak_export = app.as_weak();
app.on_search_export_requested(move || {
    if let Some(app) = weak_export.upgrade() {
        // For MVP, use a default path. Later: file dialog.
        let results = app.get_search_results();
        if results.row_count() > 0 {
            let path = dirs::download_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("ia-search-results.jsonl");
            match backend::export::export_results(
                &results,
                &path,
                backend::export::ExportFormat::Jsonl,
            ) {
                Ok(count) => {
                    app.set_search_status(slint::SharedString::from(format!(
                        "Exported {count} results to {}",
                        path.display()
                    )));
                }
                Err(e) => {
                    app.set_search_status(slint::SharedString::from(format!(
                        "Export failed: {e}"
                    )));
                }
            }
        }
    }
});
```

**Step 4: Write test for export**

Create `ia-gui/src/backend/export.rs` tests at bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use slint::VecModel;

    #[test]
    fn test_export_jsonl() {
        let results = vec![SearchResultData {
            identifier: "test-item".into(),
            title: "Test Item".into(),
            mediatype: "texts".into(),
            description: "A test".into(),
        }];
        let model = ModelRc::new(VecModel::from(results));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");

        let count = export_results(&model, &path, ExportFormat::Jsonl).unwrap();
        assert_eq!(count, 1);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("test-item"));
    }

    #[test]
    fn test_export_identifiers() {
        let results = vec![
            SearchResultData {
                identifier: "item-a".into(),
                title: "A".into(),
                mediatype: "texts".into(),
                description: "".into(),
            },
            SearchResultData {
                identifier: "item-b".into(),
                title: "B".into(),
                mediatype: "audio".into(),
                description: "".into(),
            },
        ];
        let model = ModelRc::new(VecModel::from(results));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");

        let count = export_results(&model, &path, ExportFormat::Identifiers).unwrap();
        assert_eq!(count, 2);

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "item-a\nitem-b\n");
    }
}
```

Add `tempfile = "3"` to `ia-gui/Cargo.toml` dev-dependencies.

**Step 5: Run tests**

Run: `cargo test -p ia-gui`
Expected: Tests pass.

**Step 6: Build and verify full app**

Run: `cargo run -p ia-gui`
Expected: Search for something, click "Export Results" — status shows export path.

**Step 7: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add search result export to JSONL/CSV/identifiers"
```

---

## Milestone 3: Item Detail (Tasks 8-10)

### Task 8: Item Detail Page UI

**Files:**
- Create: `ia-gui/ui/pages/item-detail.slint`
- Modify: `ia-gui/ui/app.slint`

**Step 1: Create item detail page**

Create `ia-gui/ui/pages/item-detail.slint`:

```slint
import { VerticalBox, HorizontalBox, Button, ScrollView } from "std-widgets.slint";

export struct ItemDetailData {
    identifier: string,
    title: string,
    mediatype: string,
    description: string,
    creator: string,
    date: string,
    collection: string,
    files-count: int,
    item-size: string,
    json-text: string,
}

export enum DetailTab {
    details,
    files,
    json,
}

export component ItemDetailPage inherits Rectangle {
    in property <ItemDetailData> item: {
        identifier: "",
        title: "",
        mediatype: "",
        description: "",
        creator: "",
        date: "",
        collection: "",
        files-count: 0,
        item-size: "",
        json-text: "",
    };
    in property <bool> loading: false;
    in-out property <DetailTab> active-tab: DetailTab.details;

    callback back-clicked();
    callback download-clicked(string);
    callback add-to-list-clicked(string);

    VerticalBox {
        padding: 24px;
        spacing: 16px;

        // Back button + identifier
        HorizontalBox {
            spacing: 8px;
            alignment: start;

            Button {
                text: "← Back";
                clicked => { root.back-clicked(); }
            }

            Text {
                text: item.identifier;
                font-size: 14px;
                color: #6b7280;
                vertical-alignment: center;
            }
        }

        // Header with thumbnail placeholder
        HorizontalLayout {
            spacing: 16px;

            Rectangle {
                width: 120px;
                height: 120px;
                background: #e5e7eb;
                border-radius: 8px;

                Text {
                    text: "📦";
                    font-size: 48px;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
            }

            VerticalLayout {
                spacing: 6px;
                alignment: start;

                Text {
                    text: item.title != "" ? item.title : item.identifier;
                    font-size: 22px;
                    font-weight: 700;
                    wrap: word-wrap;
                }
                Text {
                    text: item.mediatype;
                    font-size: 14px;
                    color: #6b7280;
                }
                Text {
                    text: item.files-count > 0 ?
                        item.files-count + " files · " + item.item-size : "";
                    font-size: 14px;
                    color: #6b7280;
                }
            }
        }

        // Tab bar
        HorizontalBox {
            spacing: 0px;
            alignment: start;

            tab-details := Rectangle {
                width: 80px;
                height: 36px;
                border-radius: 4px;
                background: active-tab == DetailTab.details ? #e0e7ff : transparent;

                Text {
                    text: "Details";
                    font-size: 14px;
                    font-weight: active-tab == DetailTab.details ? 600 : 400;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
                TouchArea {
                    clicked => { active-tab = DetailTab.details; }
                }
            }
            tab-files := Rectangle {
                width: 80px;
                height: 36px;
                border-radius: 4px;
                background: active-tab == DetailTab.files ? #e0e7ff : transparent;

                Text {
                    text: "Files";
                    font-size: 14px;
                    font-weight: active-tab == DetailTab.files ? 600 : 400;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
                TouchArea {
                    clicked => { active-tab = DetailTab.files; }
                }
            }
            tab-json := Rectangle {
                width: 80px;
                height: 36px;
                border-radius: 4px;
                background: active-tab == DetailTab.json ? #e0e7ff : transparent;

                Text {
                    text: "JSON";
                    font-size: 14px;
                    font-weight: active-tab == DetailTab.json ? 600 : 400;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
                TouchArea {
                    clicked => { active-tab = DetailTab.json; }
                }
            }
        }

        // Tab content
        if active-tab == DetailTab.details : ScrollView {
            vertical-stretch: 1;

            VerticalBox {
                spacing: 12px;

                if item.description != "" : VerticalBox {
                    spacing: 4px;
                    Text { text: "Description"; font-size: 12px; color: #6b7280; font-weight: 600; }
                    Text { text: item.description; font-size: 14px; wrap: word-wrap; }
                }
                if item.creator != "" : VerticalBox {
                    spacing: 4px;
                    Text { text: "Creator"; font-size: 12px; color: #6b7280; font-weight: 600; }
                    Text { text: item.creator; font-size: 14px; }
                }
                if item.date != "" : VerticalBox {
                    spacing: 4px;
                    Text { text: "Date"; font-size: 12px; color: #6b7280; font-weight: 600; }
                    Text { text: item.date; font-size: 14px; }
                }
                if item.collection != "" : VerticalBox {
                    spacing: 4px;
                    Text { text: "Collections"; font-size: 12px; color: #6b7280; font-weight: 600; }
                    Text { text: item.collection; font-size: 14px; }
                }
            }
        }

        if active-tab == DetailTab.json : ScrollView {
            vertical-stretch: 1;

            Text {
                text: loading ? "Loading..." : item.json-text;
                font-size: 12px;
                font-family: "monospace";
                wrap: word-wrap;
            }
        }

        // TODO: Files tab content will be added in Task 9

        // Actions bar
        HorizontalBox {
            spacing: 8px;
            alignment: end;

            Button {
                text: "Add to List";
                clicked => { root.add-to-list-clicked(item.identifier); }
            }
            Button {
                text: "Download";
                clicked => { root.download-clicked(item.identifier); }
            }
        }
    }
}
```

**Step 2: Update app.slint to include item detail and navigation**

Add to `ia-gui/ui/app.slint`:
- Import `ItemDetailPage` and `ItemDetailData`
- Add `in property <bool> showing-item-detail: false;`
- Add `in property <ItemDetailData> item-detail;`
- Add callbacks: `callback item-detail-back()`, `callback item-download(string)`, `callback item-add-to-list(string)`
- Add conditional rendering: show `ItemDetailPage` when `showing-item-detail` is true (overlaying the current page content)

The exact integration depends on Slint's conditional rendering. The item detail page should overlay or replace the content area when active.

**Step 3: Build and verify**

Run: `cargo build -p ia-gui`
Expected: Compiles. Item detail page exists but isn't navigated to yet (will be wired in Task 9).

**Step 4: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add item detail page UI with details/files/json tabs"
```

---

### Task 9: Wire Item Detail Backend

**Files:**
- Create: `ia-gui/src/backend/metadata.rs`
- Modify: `ia-gui/src/backend/mod.rs`
- Modify: `ia-gui/src/main.rs`

**Step 1: Create metadata backend**

Create `ia-gui/src/backend/metadata.rs`:

```rust
use crate::backend::AppBackend;
use crate::{AppWindow, ItemDetailData};
use ia_core::IaClient;
use slint::{ComponentHandle, SharedString};
use std::sync::Arc;

impl AppBackend {
    pub fn setup_item_detail(&self, app: &AppWindow, runtime: &tokio::runtime::Handle) {
        let client = Arc::clone(&self.client);
        let rt = runtime.clone();
        let weak = app.as_weak();

        app.on_search_result_clicked(move |index| {
            let client = Arc::clone(&client);
            let weak = weak.clone();

            // Get identifier from search results
            let identifier = {
                if let Some(app) = weak.upgrade() {
                    let results = app.get_search_results();
                    if let Some(item) = results.row_data(index as usize) {
                        item.identifier.to_string()
                    } else {
                        return;
                    }
                } else {
                    return;
                }
            };

            // Show loading state
            if let Some(app) = weak.upgrade() {
                app.set_showing_item_detail(true);
            }

            rt.spawn(async move {
                let result = fetch_item_detail(&client, &identifier).await;

                slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak.upgrade() {
                        match result {
                            Ok(detail) => {
                                app.set_item_detail(detail);
                            }
                            Err(e) => {
                                app.set_item_detail(ItemDetailData {
                                    identifier: SharedString::from(&identifier),
                                    title: SharedString::from(format!("Error: {e}")),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                })
                .ok();
            });
        });

        // Back button
        let weak_back = app.as_weak();
        app.on_item_detail_back(move || {
            if let Some(app) = weak_back.upgrade() {
                app.set_showing_item_detail(false);
            }
        });
    }
}

async fn fetch_item_detail(
    client: &IaClient,
    identifier: &str,
) -> anyhow::Result<ItemDetailData> {
    let item = ia_core::metadata::get(client, identifier).await?;
    let json_text = serde_json::to_string_pretty(&item)?;

    let title = item
        .metadata
        .title
        .as_ref()
        .map(|t| t.first().unwrap_or_default())
        .unwrap_or_default();

    let creator = item
        .metadata
        .creator
        .as_ref()
        .map(|c| c.first().unwrap_or_default())
        .unwrap_or_default();

    let collection = item
        .metadata
        .collection
        .as_ref()
        .map(|c| c.to_vec().join(", "))
        .unwrap_or_default();

    let size = item.item_size.unwrap_or(0);
    let size_str = format_bytes(size);

    Ok(ItemDetailData {
        identifier: SharedString::from(identifier),
        title: SharedString::from(title),
        mediatype: SharedString::from(
            item.metadata.mediatype.as_deref().unwrap_or(""),
        ),
        description: SharedString::from(
            item.metadata.description.as_deref().unwrap_or(""),
        ),
        creator: SharedString::from(creator),
        date: SharedString::from(
            item.metadata.date.as_deref().unwrap_or(""),
        ),
        collection: SharedString::from(collection),
        files_count: item.files_count.unwrap_or(0) as i32,
        item_size: SharedString::from(size_str),
        json_text: SharedString::from(json_text),
    })
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;

    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1_048_576), "1.0 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
        assert_eq!(format_bytes(1_099_511_627_776), "1.0 TB");
    }
}
```

**Step 2: Update mod.rs and main.rs**

Add `pub mod metadata;` to `ia-gui/src/backend/mod.rs`.

In `main.rs`, add: `app_backend.setup_item_detail(&app, runtime.handle());`

**Step 3: Run tests**

Run: `cargo test -p ia-gui`
Expected: `format_bytes` tests pass.

**Step 4: Build and verify**

Run: `cargo run -p ia-gui`
Expected: Search for something, click a result → item detail page shows with metadata. Click "← Back" to return.

**Step 5: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): wire item detail with metadata fetch from ia-core"
```

---

### Task 10: Item Detail Files Tab

**Files:**
- Modify: `ia-gui/ui/pages/item-detail.slint` (add files list)
- Modify: `ia-gui/src/backend/metadata.rs` (fetch and expose file list)
- Modify: `ia-gui/ui/app.slint` (add file list model)

**Step 1: Add FileData struct and files list to item-detail.slint**

Add to the top of `item-detail.slint`:

```slint
export struct FileData {
    name: string,
    format: string,
    size: string,
    source: string,
}
```

Add a new `in property <[FileData]> files: [];` to `ItemDetailPage`.

Add the files tab content (under the `if active-tab == DetailTab.json` block):

```slint
if active-tab == DetailTab.files : VerticalBox {
    vertical-stretch: 1;
    spacing: 8px;

    // Column headers
    HorizontalLayout {
        padding-left: 12px;
        padding-right: 12px;
        spacing: 12px;

        Text { text: "Name"; font-size: 12px; font-weight: 600; color: #6b7280; horizontal-stretch: 3; }
        Text { text: "Format"; font-size: 12px; font-weight: 600; color: #6b7280; horizontal-stretch: 1; }
        Text { text: "Size"; font-size: 12px; font-weight: 600; color: #6b7280; horizontal-stretch: 1; }
        Text { text: "Source"; font-size: 12px; font-weight: 600; color: #6b7280; horizontal-stretch: 1; }
    }

    Flickable {
        vertical-stretch: 1;

        VerticalBox {
            spacing: 1px;

            for file[index] in files : Rectangle {
                height: 32px;

                HorizontalLayout {
                    padding-left: 12px;
                    padding-right: 12px;
                    spacing: 12px;

                    Text { text: file.name; font-size: 13px; horizontal-stretch: 3; overflow: elide; vertical-alignment: center; }
                    Text { text: file.format; font-size: 13px; horizontal-stretch: 1; color: #6b7280; vertical-alignment: center; }
                    Text { text: file.size; font-size: 13px; horizontal-stretch: 1; color: #6b7280; vertical-alignment: center; }
                    Text { text: file.source; font-size: 13px; horizontal-stretch: 1; color: #6b7280; vertical-alignment: center; }
                }
            }
        }
    }
}
```

**Step 2: Populate file list in backend**

In `metadata.rs`, after fetching `ItemMetadata`, convert files:

```rust
fn files_to_slint(files: &[ia_core::types::FileMetadata]) -> Vec<FileData> {
    files
        .iter()
        .map(|f| FileData {
            name: SharedString::from(&f.name),
            format: SharedString::from(f.format.as_deref().unwrap_or("")),
            size: SharedString::from(format_bytes(f.size.unwrap_or(0))),
            source: SharedString::from(
                f.source
                    .as_ref()
                    .map(|s| format!("{s:?}").to_lowercase())
                    .unwrap_or_default(),
            ),
        })
        .collect()
}
```

Wire this into the item detail fetch and set on the app via a `set_item_files()` property.

**Step 3: Build and verify**

Run: `cargo run -p ia-gui`
Expected: Click an item → Files tab shows file listing with name, format, size, source columns.

**Step 4: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add files tab to item detail page"
```

---

## Milestone 4: Downloads (Tasks 11-14)

### Task 11: Download Manager Backend

**Files:**
- Create: `ia-gui/src/backend/downloads.rs`
- Modify: `ia-gui/src/backend/mod.rs`

This task creates the download queue manager that tracks active downloads, manages the job log, and bridges ia-core's download functions to the GUI.

**Step 1: Create download manager**

Create `ia-gui/src/backend/downloads.rs`:

```rust
use ia_core::download::{
    download_item, BatchDownloadProgress, DownloadOpts, DownloadProgress, DownloadStatus,
    ItemDownloadResult,
};
use ia_core::files::FileFilter;
use ia_core::joblog::{JoblogEntry, JoblogWriter};
use ia_core::IaClient;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

pub struct DownloadManager {
    pub client: Arc<IaClient>,
    pub jobs: usize,
    pub semaphore: Arc<Semaphore>,
    pub default_destdir: PathBuf,
    pub joblog_path: PathBuf,
    pub active: Arc<Mutex<HashMap<String, DownloadState>>>,
}

pub struct DownloadState {
    pub identifier: String,
    pub files_total: usize,
    pub files_completed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_downloaded: u64,
    pub bytes_total: u64,
    pub status: ItemDownloadStatus,
}

pub enum ItemDownloadStatus {
    Queued,
    Downloading,
    Complete,
    Failed(String),
}

impl DownloadManager {
    pub fn new(client: Arc<IaClient>, jobs: usize, destdir: PathBuf, joblog_path: PathBuf) -> Self {
        Self {
            client,
            jobs,
            semaphore: Arc::new(Semaphore::new(jobs)),
            default_destdir: destdir,
            joblog_path,
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Queue an item for download. Returns immediately; download runs async.
    pub fn queue_download(
        &self,
        identifier: String,
        runtime: &tokio::runtime::Handle,
        on_progress: Arc<dyn Fn(&str, &DownloadState) + Send + Sync>,
        on_complete: Arc<dyn Fn(&str, Result<ItemDownloadResult, String>) + Send + Sync>,
    ) {
        let client = Arc::clone(&self.client);
        let semaphore = Arc::clone(&self.semaphore);
        let destdir = self.default_destdir.clone();
        let active = Arc::clone(&self.active);
        let joblog_path = self.joblog_path.clone();
        let id = identifier.clone();

        // Register in active downloads
        {
            let mut active = active.lock().unwrap();
            active.insert(
                identifier.clone(),
                DownloadState {
                    identifier: identifier.clone(),
                    files_total: 0,
                    files_completed: 0,
                    files_skipped: 0,
                    files_failed: 0,
                    bytes_downloaded: 0,
                    bytes_total: 0,
                    status: ItemDownloadStatus::Queued,
                },
            );
        }

        runtime.spawn(async move {
            let opts = DownloadOpts {
                destdir: destdir.clone(),
                no_directories: false,
                checksum: false,
                retries: 3,
                no_timestamps: false,
                dry_run: false,
                filter: FileFilter::default(),
            };

            let active_ref = Arc::clone(&active);
            let id_ref = id.clone();
            let on_progress_ref = Arc::clone(&on_progress);

            let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
                Some(Arc::new(move |p| {
                    let mut active = active_ref.lock().unwrap();
                    if let Some(state) = active.get_mut(&id_ref) {
                        state.bytes_downloaded = p.bytes_downloaded;
                        if let Some(total) = p.total_bytes {
                            state.bytes_total = total;
                        }
                        match &p.status {
                            DownloadStatus::Starting => {
                                state.status = ItemDownloadStatus::Downloading;
                            }
                            DownloadStatus::Complete => {
                                state.files_completed += 1;
                            }
                            DownloadStatus::Skipped(_) => {
                                state.files_skipped += 1;
                            }
                            DownloadStatus::Failed(_) => {
                                state.files_failed += 1;
                            }
                            _ => {}
                        }
                        on_progress_ref(&id_ref, state);
                    }
                }));

            let result = download_item(&client, &id, &opts, semaphore, progress).await;

            // Write to job log
            if let Ok(writer) = JoblogWriter::open(&joblog_path) {
                if let Ok(ref r) = result {
                    for file_result in &r.files {
                        let entry = JoblogEntry {
                            ts: chrono::Utc::now().to_rfc3339(),
                            op: "download".into(),
                            item: id.clone(),
                            file: file_result.file_name.clone(),
                            status: match &file_result.status {
                                DownloadStatus::Complete => "ok".into(),
                                DownloadStatus::Skipped(_) => "skipped".into(),
                                DownloadStatus::Failed(e) => format!("error: {e}"),
                                _ => "unknown".into(),
                            },
                            bytes: Some(file_result.bytes),
                            elapsed_ms: Some(file_result.elapsed.as_millis() as u64),
                            error: match &file_result.status {
                                DownloadStatus::Failed(e) => Some(e.clone()),
                                _ => None,
                            },
                            retries: None,
                        };
                        let _ = writer.write(&entry);
                    }
                }
            }

            // Update final state and notify
            {
                let mut active = active.lock().unwrap();
                if let Some(state) = active.get_mut(&id) {
                    state.status = match &result {
                        Ok(_) => ItemDownloadStatus::Complete,
                        Err(e) => ItemDownloadStatus::Failed(e.to_string()),
                    };
                }
            }

            let result_mapped = result.map_err(|e| e.to_string());
            on_complete(&id, result_mapped);
        });
    }
}
```

**Step 2: Update mod.rs**

Add `pub mod downloads;` and add `DownloadManager` to `AppBackend`:

```rust
pub struct AppBackend {
    pub client: Arc<IaClient>,
    pub thumbnails: thumbnails::ThumbnailCache,
    pub downloads: downloads::DownloadManager,
}
```

Initialize in `new()` with default destdir from `dirs::download_dir()` and joblog path from `dirs::data_dir()`.

**Step 3: Build and verify**

Run: `cargo build -p ia-gui`
Expected: Compiles.

**Step 4: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add download manager backend with job log integration"
```

---

### Task 12: Downloads Page UI — Active View

**Files:**
- Create: `ia-gui/ui/pages/downloads.slint`
- Modify: `ia-gui/ui/app.slint`
- Modify: `ia-gui/src/main.rs`

**Step 1: Create downloads page with active view**

Create `ia-gui/ui/pages/downloads.slint`:

```slint
import { VerticalBox, HorizontalBox, Button, ProgressIndicator } from "std-widgets.slint";

export enum DownloadsTab {
    active,
    history,
    files,
}

export struct ActiveDownloadData {
    identifier: string,
    files-total: int,
    files-completed: int,
    files-skipped: int,
    files-failed: int,
    bytes-downloaded: int,
    bytes-total: int,
    speed: string,
    status: string,
}

export struct HistoryEntryData {
    identifier: string,
    status: string,
    files: string,
    size: string,
    date: string,
}

export component DownloadsPage inherits Rectangle {
    in-out property <DownloadsTab> active-tab: DownloadsTab.active;
    in property <[ActiveDownloadData]> active-downloads: [];
    in property <[HistoryEntryData]> history: [];
    in property <string> status-text: "";

    callback download-identifier(string);
    callback retry-failed();
    callback clear-history();

    VerticalBox {
        padding: 24px;
        spacing: 16px;

        // Tab bar
        HorizontalBox {
            spacing: 0px;
            alignment: start;

            Rectangle {
                width: 80px;
                height: 36px;
                border-radius: 4px;
                background: active-tab == DownloadsTab.active ? #e0e7ff : transparent;
                Text { text: "Active"; font-size: 14px; horizontal-alignment: center; vertical-alignment: center;
                    font-weight: active-tab == DownloadsTab.active ? 600 : 400; }
                TouchArea { clicked => { active-tab = DownloadsTab.active; } }
            }
            Rectangle {
                width: 80px;
                height: 36px;
                border-radius: 4px;
                background: active-tab == DownloadsTab.history ? #e0e7ff : transparent;
                Text { text: "History"; font-size: 14px; horizontal-alignment: center; vertical-alignment: center;
                    font-weight: active-tab == DownloadsTab.history ? 600 : 400; }
                TouchArea { clicked => { active-tab = DownloadsTab.history; } }
            }
            Rectangle {
                width: 120px;
                height: 36px;
                border-radius: 4px;
                background: active-tab == DownloadsTab.files ? #e0e7ff : transparent;
                Text { text: "Downloaded"; font-size: 14px; horizontal-alignment: center; vertical-alignment: center;
                    font-weight: active-tab == DownloadsTab.files ? 600 : 400; }
                TouchArea { clicked => { active-tab = DownloadsTab.files; } }
            }

            Rectangle { horizontal-stretch: 1; }

            // Manual download input
            HorizontalBox {
                spacing: 8px;
                id-input := Rectangle {
                    // Placeholder for LineEdit - add in implementation
                }
            }
        }

        // Active downloads view
        if active-tab == DownloadsTab.active : Flickable {
            vertical-stretch: 1;

            VerticalBox {
                spacing: 8px;

                if active-downloads.length == 0 : Text {
                    text: "No active downloads";
                    font-size: 14px;
                    color: #9ca3af;
                    horizontal-alignment: center;
                }

                for dl[index] in active-downloads : Rectangle {
                    height: 80px;
                    border-radius: 8px;
                    background: #f9fafb;

                    VerticalBox {
                        padding: 12px;
                        spacing: 4px;

                        HorizontalLayout {
                            Text {
                                text: dl.identifier;
                                font-size: 14px;
                                font-weight: 600;
                                horizontal-stretch: 1;
                            }
                            Text {
                                text: dl.status;
                                font-size: 12px;
                                color: #6b7280;
                            }
                        }

                        ProgressIndicator {
                            progress: dl.bytes-total > 0 ?
                                dl.bytes-downloaded / dl.bytes-total : -1;
                        }

                        HorizontalLayout {
                            Text {
                                text: dl.files-completed + "/" + dl.files-total + " files";
                                font-size: 12px;
                                color: #6b7280;
                            }
                            Rectangle { horizontal-stretch: 1; }
                            Text {
                                text: dl.speed;
                                font-size: 12px;
                                color: #6b7280;
                            }
                        }
                    }
                }
            }
        }

        // History view
        if active-tab == DownloadsTab.history : VerticalBox {
            vertical-stretch: 1;
            spacing: 8px;

            if history.length == 0 : Text {
                text: "No download history";
                font-size: 14px;
                color: #9ca3af;
                horizontal-alignment: center;
                vertical-stretch: 1;
            }

            Flickable {
                vertical-stretch: 1;

                VerticalBox {
                    spacing: 1px;

                    for entry[index] in history : Rectangle {
                        height: 40px;

                        HorizontalLayout {
                            padding-left: 12px;
                            padding-right: 12px;
                            spacing: 16px;

                            Text { text: entry.identifier; font-size: 13px; horizontal-stretch: 2; vertical-alignment: center; overflow: elide; }
                            Text { text: entry.status; font-size: 13px; horizontal-stretch: 1; vertical-alignment: center; }
                            Text { text: entry.files; font-size: 13px; horizontal-stretch: 1; color: #6b7280; vertical-alignment: center; }
                            Text { text: entry.size; font-size: 13px; horizontal-stretch: 1; color: #6b7280; vertical-alignment: center; }
                            Text { text: entry.date; font-size: 13px; horizontal-stretch: 1; color: #6b7280; vertical-alignment: center; }
                        }
                    }
                }
            }

            HorizontalBox {
                spacing: 8px;
                alignment: end;

                Button {
                    text: "Retry Failed";
                    clicked => { root.retry-failed(); }
                }
                Button {
                    text: "Clear";
                    clicked => { root.clear-history(); }
                }
            }
        }

        // Status bar
        if status-text != "" : Text {
            text: status-text;
            font-size: 12px;
            color: #6b7280;
        }
    }
}
```

**Step 2: Wire into app.slint**

Import `DownloadsPage` and add it to the content area for `Page.downloads`.

**Step 3: Build and verify**

Run: `cargo build -p ia-gui`
Expected: Compiles. Downloads page shows with Active/History/Downloaded tabs.

**Step 4: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add downloads page UI with active/history/downloaded tabs"
```

---

### Task 13: Wire Downloads to Item Detail

**Files:**
- Modify: `ia-gui/src/main.rs`
- Modify: `ia-gui/src/backend/downloads.rs`

Wire the "Download" button on the Item Detail page to actually queue a download via the DownloadManager. Wire progress updates back to the Downloads page UI.

This task connects the existing download manager backend to the UI callbacks and updates the active downloads model as progress events come in.

**Step 1: Set up download callback from item detail**

In `main.rs`, wire `on_item_download`:

```rust
let dm = Arc::new(app_backend.downloads);
let dm_ref = Arc::clone(&dm);
let weak_dl = app.as_weak();
let rt_dl = runtime.handle().clone();

app.on_item_download(move |identifier| {
    let id = identifier.to_string();
    let weak = weak_dl.clone();

    let on_progress = Arc::new(move |id: &str, state: &downloads::DownloadState| {
        // Update Slint model via invoke_from_event_loop
        let weak = weak.clone();
        let id = id.to_string();
        slint::invoke_from_event_loop(move || {
            // Update active downloads model
            // (Implementation depends on exact Slint model API)
        }).ok();
    });

    let on_complete = Arc::new(|id: &str, result: Result<_, String>| {
        tracing::info!("Download complete: {id}");
    });

    dm_ref.queue_download(id, &rt_dl, on_progress, on_complete);
});
```

**Step 2: Build and verify**

Run: `cargo run -p ia-gui`
Expected: Search → click item → click "Download" → download starts and active downloads tab shows progress.

**Step 3: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): wire download button to download manager"
```

---

### Task 14: Download History from Job Log

**Files:**
- Create: `ia-gui/src/backend/history.rs`
- Modify: `ia-gui/src/backend/mod.rs`
- Modify: `ia-gui/src/main.rs`

**Step 1: Create history backend**

Create `ia-gui/src/backend/history.rs`:

```rust
use crate::HistoryEntryData;
use ia_core::joblog;
use slint::SharedString;
use std::collections::HashMap;
use std::path::Path;

pub fn load_history(joblog_path: &Path) -> Vec<HistoryEntryData> {
    let entries = match joblog::parse(joblog_path) {
        Ok(entries) => entries,
        Err(_) => return vec![],
    };

    // Group by item
    let mut by_item: HashMap<String, Vec<&joblog::JoblogEntry>> = HashMap::new();
    for entry in &entries {
        by_item.entry(entry.item.clone()).or_default().push(entry);
    }

    let mut history: Vec<HistoryEntryData> = by_item
        .into_iter()
        .map(|(item, entries)| {
            let ok = entries.iter().filter(|e| e.status == "ok").count();
            let skipped = entries.iter().filter(|e| e.status == "skipped").count();
            let errors = entries
                .iter()
                .filter(|e| e.status.starts_with("error"))
                .count();
            let total = ok + skipped + errors;
            let total_bytes: u64 = entries.iter().filter_map(|e| e.bytes).sum();
            let date = entries.last().map(|e| e.ts.clone()).unwrap_or_default();

            let status = if errors > 0 {
                format!("⚠ {errors} errors")
            } else {
                "✓ Done".into()
            };

            HistoryEntryData {
                identifier: SharedString::from(&item),
                status: SharedString::from(status),
                files: SharedString::from(format!("{ok}/{total}")),
                size: SharedString::from(crate::backend::metadata::format_bytes(total_bytes)),
                date: SharedString::from(
                    date.get(..10).unwrap_or(&date),
                ),
            }
        })
        .collect();

    // Most recent first
    history.reverse();
    history
}
```

**Step 2: Wire history loading**

In `main.rs`, when the user navigates to Downloads page or on app start, load history from job log and populate the history model.

**Step 3: Build and verify**

Run: `cargo run -p ia-gui`
Expected: After downloading something, switch to History tab → shows completed downloads from job log.

**Step 4: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add download history from job log"
```

---

## Milestone 5: Lists (Tasks 15-17)

### Task 15: List Persistence Layer

**Files:**
- Create: `ia-gui/src/backend/lists.rs`
- Modify: `ia-gui/src/backend/mod.rs`

**Step 1: Create list manager**

Create `ia-gui/src/backend/lists.rs`:

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
pub struct ItemList {
    pub name: String,
    pub identifiers: Vec<String>,
    pub created: String,
}

pub struct ListManager {
    lists_dir: PathBuf,
}

impl ListManager {
    pub fn new(data_dir: &Path) -> Result<Self> {
        let lists_dir = data_dir.join("lists");
        fs::create_dir_all(&lists_dir)?;
        Ok(Self { lists_dir })
    }

    pub fn all(&self) -> Result<Vec<ItemList>> {
        let mut lists = Vec::new();
        for entry in fs::read_dir(&self.lists_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                if let Ok(list) = self.load_from_path(&path) {
                    lists.push(list);
                }
            }
        }
        lists.sort_by(|a, b| b.created.cmp(&a.created));
        Ok(lists)
    }

    pub fn get(&self, name: &str) -> Result<ItemList> {
        let path = self.path_for(name);
        self.load_from_path(&path)
    }

    pub fn save(&self, list: &ItemList) -> Result<()> {
        let path = self.path_for(&list.name);
        let json = serde_json::to_string_pretty(list)?;
        fs::write(path, json)?;
        Ok(())
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let path = self.path_for(name);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn add_identifiers(&self, name: &str, ids: &[String]) -> Result<()> {
        let mut list = self.get(name).unwrap_or_else(|_| ItemList {
            name: name.to_string(),
            identifiers: Vec::new(),
            created: chrono::Utc::now().to_rfc3339(),
        });
        for id in ids {
            if !list.identifiers.contains(id) {
                list.identifiers.push(id.clone());
            }
        }
        self.save(&list)
    }

    /// Import identifiers from a file (one per line, or JSONL with "identifier" field).
    pub fn import_from_file(&self, name: &str, path: &Path) -> Result<usize> {
        let content = fs::read_to_string(path)?;
        let mut identifiers = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Try JSONL first
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(id) = obj.get("identifier").and_then(|v| v.as_str()) {
                    identifiers.push(id.to_string());
                    continue;
                }
            }
            // Plain text: one identifier per line
            identifiers.push(line.to_string());
        }

        let count = identifiers.len();
        self.add_identifiers(name, &identifiers)?;
        Ok(count)
    }

    fn path_for(&self, name: &str) -> PathBuf {
        let safe_name = name.replace(['/', '\\', '\0'], "_");
        self.lists_dir.join(format!("{safe_name}.json"))
    }

    fn load_from_path(&self, path: &Path) -> Result<ItemList> {
        let content = fs::read_to_string(path)?;
        let list: ItemList = serde_json::from_str(&content)?;
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_load_list() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ListManager::new(dir.path()).unwrap();

        let list = ItemList {
            name: "test-list".into(),
            identifiers: vec!["item-a".into(), "item-b".into()],
            created: "2026-02-21T00:00:00Z".into(),
        };
        mgr.save(&list).unwrap();

        let loaded = mgr.get("test-list").unwrap();
        assert_eq!(loaded.name, "test-list");
        assert_eq!(loaded.identifiers.len(), 2);
    }

    #[test]
    fn test_add_identifiers_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ListManager::new(dir.path()).unwrap();

        mgr.add_identifiers("my-list", &["a".into(), "b".into()]).unwrap();
        mgr.add_identifiers("my-list", &["b".into(), "c".into()]).unwrap();

        let list = mgr.get("my-list").unwrap();
        assert_eq!(list.identifiers, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_import_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ListManager::new(dir.path()).unwrap();

        let file_path = dir.path().join("items.txt");
        fs::write(&file_path, "item-a\nitem-b\nitem-c\n").unwrap();

        let count = mgr.import_from_file("imported", &file_path).unwrap();
        assert_eq!(count, 3);

        let list = mgr.get("imported").unwrap();
        assert_eq!(list.identifiers.len(), 3);
    }

    #[test]
    fn test_import_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ListManager::new(dir.path()).unwrap();

        let file_path = dir.path().join("items.jsonl");
        fs::write(
            &file_path,
            "{\"identifier\":\"item-a\"}\n{\"identifier\":\"item-b\"}\n",
        ).unwrap();

        let count = mgr.import_from_file("imported", &file_path).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_delete_list() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ListManager::new(dir.path()).unwrap();

        mgr.add_identifiers("doomed", &["a".into()]).unwrap();
        assert!(mgr.get("doomed").is_ok());

        mgr.delete("doomed").unwrap();
        assert!(mgr.get("doomed").is_err());
    }
}
```

**Step 2: Add chrono and tempfile to ia-gui Cargo.toml**

```toml
[dependencies]
chrono = "0.4"

[dev-dependencies]
tempfile = "3"
```

**Step 3: Run tests**

Run: `cargo test -p ia-gui`
Expected: All list tests pass.

**Step 4: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add list persistence layer with import/export"
```

---

### Task 16: Lists Page UI

**Files:**
- Create: `ia-gui/ui/pages/lists.slint`
- Modify: `ia-gui/ui/app.slint`
- Modify: `ia-gui/src/main.rs`

Create the Lists page with:
- List overview table (name, item count, created date, action buttons)
- Detail view showing items in selected list (with thumbnails)
- Bottom action bar: Download All, Fetch Metadata, Export, Open in Search
- New List / Import from File buttons

Wire to `ListManager` backend. Use pattern established in search (callbacks up, properties down).

**Step 1: Create lists.slint**

Follow the design doc mockup from `docs/plans/2026-02-21-gui-design.md` (Lists section). Define structs `ListSummaryData` and use `SearchResultData` for items within a selected list.

**Step 2: Wire backend callbacks in main.rs**

Load all lists on app start, refresh when lists change.

**Step 3: Build and verify**

Run: `cargo run -p ia-gui`
Expected: Lists page shows, can create lists, add items from search, view list contents.

**Step 4: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add lists page UI with management operations"
```

---

### Task 17: Cross-Section Integration

**Files:**
- Modify: `ia-gui/src/main.rs`
- Modify: `ia-gui/src/backend/search.rs`
- Modify: `ia-gui/src/backend/downloads.rs`

Wire the connections between sections:
- Search → "Add to List" saves selected search results to a list
- Lists → "Download All" queues all items in list for download
- Item Detail → "Add to List" adds the current item to a list
- Downloads → completed items auto-added to "Downloaded" list

This is mostly callback wiring in `main.rs` connecting existing backends.

**Step 1: Add "Add to List" from search**

Wire `on_search_add_to_list` callback. Show a simple text input or dropdown to pick list name.

**Step 2: Add "Download All" from lists**

Wire list download button to queue each identifier through DownloadManager.

**Step 3: Auto-track downloads**

In the download complete handler, add identifier to "Downloaded" list.

**Step 4: Build and verify**

Run: `cargo run -p ia-gui`
Expected: Full workflow: Search → Add to List → Go to Lists → Download All → Check Downloads active → Check History.

**Step 5: Commit**

```bash
git add ia-gui/
git commit -m "feat(gui): add cross-section integration (lists, search, downloads)"
```

---

## Milestone 6: Metadata & Settings (Tasks 18-20)

### Task 18: Metadata Browse Page

**Files:**
- Create: `ia-gui/ui/pages/metadata.slint`
- Modify: `ia-gui/ui/app.slint`
- Modify: `ia-gui/src/main.rs`

Create the Metadata page with Browse mode:
- Identifier input + Fetch button
- Human-readable summary with thumbnail
- Raw JSON viewer (monospace, scrollable)
- Actions: Copy JSON, Save to File, View Files, Download

Reuse `fetch_item_detail` from `backend/metadata.rs`. The Browse mode is essentially a simplified Item Detail that lives in its own page.

**Step 1-4:** Create `.slint` file, wire callbacks, build, commit.

```bash
git commit -m "feat(gui): add metadata browse page"
```

---

### Task 19: Settings Page

**Files:**
- Create: `ia-gui/ui/pages/settings.slint`
- Create: `ia-gui/src/backend/settings.rs`
- Modify: `ia-gui/ui/app.slint`
- Modify: `ia-gui/src/main.rs`

**Step 1: Create settings backend**

Create `ia-gui/src/backend/settings.rs`:

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
pub struct AppSettings {
    pub host: String,
    pub user_agent_suffix: String,
    pub insecure: bool,
    pub default_destdir: String,
    pub jobs: usize,
    pub always_log: bool,
    pub joblog_path: String,
    pub disk_pool: Vec<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        let download_dir = dirs::download_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .to_string_lossy()
            .into_owned();
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ia");

        Self {
            host: "archive.org".into(),
            user_agent_suffix: String::new(),
            insecure: false,
            default_destdir: download_dir,
            jobs: 5,
            always_log: true,
            joblog_path: data_dir
                .join("joblog.jsonl")
                .to_string_lossy()
                .into_owned(),
            disk_pool: vec![],
        }
    }
}

impl AppSettings {
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("settings.json");
        if path.exists() {
            fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        fs::create_dir_all(data_dir)?;
        let path = data_dir.join("settings.json");
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }
}
```

**Step 2: Create settings.slint**

Form with fields matching `AppSettings`. Save button calls callback to persist.

**Step 3: Wire in main.rs**

Load settings on startup, apply to IaClient config. Save on "Save" button click.

**Step 4: Build, verify, commit**

```bash
git commit -m "feat(gui): add settings page with persistent config"
```

---

### Task 20: Status Bar

**Files:**
- Modify: `ia-gui/ui/app.slint`
- Modify: `ia-gui/src/main.rs`

Add a status bar at the bottom of the window showing:
- Connection status (host)
- Active download count
- List count

Wire from backend state. This is a small polish task.

```bash
git commit -m "feat(gui): add status bar with connection and download info"
```

---

## Milestone 7: Later-Phase Issue Placeholders (Tasks 21-26)

These tasks create GitHub issues for future work that's tracked but not implemented in MVP.

### Task 21: Create GitHub Issues for All Tasks

Create GitHub issues for:

**MVP (implement now):**
- Issue: Scaffold ia-gui crate with Slint (Task 1)
- Issue: Sidebar navigation (Task 2)
- Issue: Async backend infrastructure (Task 3)
- Issue: Search page UI (Task 4)
- Issue: Wire search backend (Task 5)
- Issue: Search thumbnails (Task 6)
- Issue: Search export (Task 7)
- Issue: Item detail page (Task 8)
- Issue: Wire item detail backend (Task 9)
- Issue: Item detail files tab (Task 10)
- Issue: Download manager backend (Task 11)
- Issue: Downloads page UI (Task 12)
- Issue: Wire downloads to item detail (Task 13)
- Issue: Download history (Task 14)
- Issue: List persistence (Task 15)
- Issue: Lists page UI (Task 16)
- Issue: Cross-section integration (Task 17)
- Issue: Metadata browse page (Task 18)
- Issue: Settings page (Task 19)
- Issue: Status bar (Task 20)

**Later phase (track as issues, don't implement yet):**
- Issue: Metadata batch retrieve mode
- Issue: Metadata write operations
- Issue: Upload page
- Issue: Tasks viewer/submitter page
- Issue: Download queue persistence across restarts
- Issue: Thumbnail display in search results (refine from placeholder)
- Issue: File dialog for export/import (replace hardcoded paths)
- Issue: Offline item browsing (enhanced downloaded files view)
- Issue: Keyboard shortcuts and accessibility

Label MVP issues with `gui-mvp` and later issues with `gui-later`.

---

## Summary

| Milestone | Tasks | Description |
|---|---|---|
| 1. Foundation | 1-3 | Scaffold crate, sidebar nav, async backend |
| 2. Search | 4-7 | Search UI, backend, thumbnails, export |
| 3. Item Detail | 8-10 | Detail page, files tab, metadata fetch |
| 4. Downloads | 11-14 | Download manager, active view, history |
| 5. Lists | 15-17 | Persistence, UI, cross-section integration |
| 6. Metadata & Settings | 18-20 | Browse page, settings, status bar |
| 7. Issues | 21 | Create all GitHub issues (MVP + later) |

**Total: 21 implementation tasks + GitHub issue creation.**

Each milestone builds on the previous. The app is usable (search + view items) after Milestone 3, and fully functional for the read-only MVP after Milestone 6.
