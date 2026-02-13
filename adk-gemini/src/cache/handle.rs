use snafu::ResultExt;
use std::sync::Arc;

use super::model::*;
use super::*;
use crate::client::GeminiClient;

/// Represents a cached content resource, providing methods to manage its lifecycle.
///
/// A  object is a handle to a cached content resource on the Gemini API.
/// It allows you to retrieve, update, or delete the cached content.
pub struct CachedContentHandle {
    /// The unique resource name of the cached content, e.g., .
    pub name: String,
    client: Arc<GeminiClient>,
}

impl CachedContentHandle {
    /// Creates a new CachedContentHandle instance.
    pub(crate) fn new(name: String, client: Arc<GeminiClient>) -> Self {
        Self { name, client }
    }

    /// Returns the unique resource name of the cached content.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Retrieves the cached content configuration by making an API call.
    pub async fn get(&self) -> Result<CachedContent, Error> {
        self.client.get_cached_content_raw(&self.name).await.map_err(Box::new).context(ClientSnafu)
    }

    /// Updates the cached content configuration (typically the TTL).
    pub async fn update(&self, expiration: CacheExpirationRequest) -> Result<CachedContent, Error> {
        self.client
            .update_cached_content(&self.name, expiration)
            .await
            .map_err(Box::new)
            .context(ClientSnafu)
    }

    /// Deletes the cached content resource from the server.
    pub async fn delete(self) -> Result<(), (Self, Error)> {
        match self
            .client
            .delete_cached_content(&self.name)
            .await
            .map_err(Box::new)
            .context(ClientSnafu)
        {
            Ok(response) => Ok(response),
            Err(e) => Err((self, e)),
        }
    }
}
