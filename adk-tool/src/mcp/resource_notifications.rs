//! MCP resource update notification handling.

use std::sync::Arc;

/// Receives resource update notifications from an MCP server.
///
/// Register one handler on an [`McpToolset`](super::McpToolset),
/// [`McpHttpClientBuilder`](super::McpHttpClientBuilder), or
/// [`McpServerManager`](super::McpServerManager). The same handler is retained
/// when ADK-Rust reconnects the underlying transport.
#[async_trait::async_trait]
pub trait ResourceNotificationHandler: Send + Sync {
    /// Handle `notifications/resources/updated` for one concrete resource URI.
    async fn handle_resource_updated(
        &self,
        uri: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Handle `notifications/resources/list_changed`.
    async fn handle_resource_list_changed(
        &self,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

pub(crate) async fn dispatch_resource_updated(
    handler: &Option<Arc<dyn ResourceNotificationHandler>>,
    uri: &str,
) {
    let Some(handler) = handler else {
        return;
    };
    let result = std::panic::AssertUnwindSafe(handler.handle_resource_updated(uri));
    match futures::FutureExt::catch_unwind(result).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, %uri, "resource notification handler returned error");
        }
        Err(_) => {
            tracing::warn!(%uri, "resource notification handler panicked");
        }
    }
}

pub(crate) async fn dispatch_resource_list_changed(
    handler: &Option<Arc<dyn ResourceNotificationHandler>>,
) {
    let Some(handler) = handler else {
        return;
    };
    let result = std::panic::AssertUnwindSafe(handler.handle_resource_list_changed());
    match futures::FutureExt::catch_unwind(result).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "resource list notification handler returned error");
        }
        Err(_) => {
            tracing::warn!("resource list notification handler panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct RecordingHandler {
        updated: AtomicUsize,
        list_changed: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ResourceNotificationHandler for RecordingHandler {
        async fn handle_resource_updated(
            &self,
            _uri: &str,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.updated.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn handle_resource_list_changed(
            &self,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.list_changed.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[tokio::test]
    async fn dispatches_resource_notifications() {
        let concrete = Arc::new(RecordingHandler::default());
        let handler: Arc<dyn ResourceNotificationHandler> = concrete.clone();
        let handler = Some(handler);

        dispatch_resource_updated(&handler, "file:///workspace/Cargo.toml").await;
        dispatch_resource_list_changed(&handler).await;

        assert_eq!(concrete.updated.load(Ordering::Relaxed), 1);
        assert_eq!(concrete.list_changed.load(Ordering::Relaxed), 1);
    }
}
