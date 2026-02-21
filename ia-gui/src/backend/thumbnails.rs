use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Cache for downloaded thumbnails, stored on disk in a temp directory.
#[allow(dead_code)]
pub struct ThumbnailCache {
    cache: Arc<Mutex<HashMap<String, PathBuf>>>,
    cache_dir: PathBuf,
}

impl ThumbnailCache {
    pub fn new() -> Self {
        let cache_dir = std::env::temp_dir().join("ia-gui-thumbnails");
        std::fs::create_dir_all(&cache_dir).ok();
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_dir,
        }
    }

    /// Fetch a thumbnail for the given identifier.
    /// Returns the path to the cached file, or None on failure.
    #[allow(dead_code)]
    pub async fn get(
        &self,
        client: &reqwest::Client,
        host: &str,
        protocol: &str,
        identifier: &str,
    ) -> Option<PathBuf> {
        // Check cache first
        {
            let cache = self.cache.lock().unwrap();
            if let Some(path) = cache.get(identifier) {
                if path.exists() {
                    return Some(path.clone());
                }
            }
        }

        // Download thumbnail
        let url = format!("{protocol}://{host}/services/img/{identifier}");
        let bytes = client.get(&url).send().await.ok()?.bytes().await.ok()?;

        if bytes.is_empty() {
            return None;
        }

        // Save to disk
        let path = self.cache_dir.join(format!("{identifier}.jpg"));
        tokio::fs::write(&path, &bytes).await.ok()?;

        // Update cache
        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(identifier.to_string(), path.clone());
        }

        Some(path)
    }
}
