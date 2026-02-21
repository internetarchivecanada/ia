pub mod state;

use ia_core::IaClient;
use std::sync::Arc;

/// Shared application state accessible from async tasks.
#[allow(dead_code)]
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
