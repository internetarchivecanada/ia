pub mod search;
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
