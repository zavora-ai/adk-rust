use crate::storage::FileStorage;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared server state
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<RwLock<FileStorage>>,
}

impl AppState {
    pub fn new(storage: FileStorage) -> Self {
        Self { storage: Arc::new(RwLock::new(storage)) }
    }
}
