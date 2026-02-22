pub mod downloads;
pub mod export;
pub mod metadata;
pub mod search;
pub mod state;
pub mod thumbnails;

use ia_core::IaClient;
use std::sync::Arc;

/// Shared application state accessible from async tasks.
#[allow(dead_code)]
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
