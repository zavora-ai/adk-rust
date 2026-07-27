use adk_core::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;

/// A trigger event delivered by an [`EventSource`].
///
/// Contains the name of the source that produced it and an arbitrary JSON payload
/// with source-specific metadata (e.g., matched file path, cron tick time, webhook body).
#[derive(Debug, Clone)]
pub struct TriggerEvent {
    /// Human-readable name of the source that produced this event.
    pub source: String,
    /// Source-specific metadata.
    pub payload: Value,
    /// The verified principal this event is attributed to, when the source authenticates.
    ///
    /// `None` for sources with no notion of a caller — a cron tick or a file change. For
    /// [`WebhookTrigger`](super::WebhookTrigger) this is whatever its verifier returned, so
    /// a handler can tell an authorized trigger from an anonymous one instead of treating
    /// every event as equally trusted.
    pub principal: Option<String>,
}

/// A source of trigger events for ambient agents.
///
/// Implementations produce an async stream of [`TriggerEvent`]s. The stream yields
/// events until the source is stopped or the subscription is dropped.
///
/// # Built-in Implementations
///
/// - [`CronTrigger`](super::CronTrigger) — cron schedule
/// - [`WebhookTrigger`](super::WebhookTrigger) — HTTP POST webhook
/// - [`FileWatchTrigger`](super::FileWatchTrigger) — filesystem watcher
#[async_trait]
pub trait EventSource: Send + Sync {
    /// Human-readable name for this event source.
    fn name(&self) -> &str;

    /// Subscribe to the event stream.
    ///
    /// Returns a stream that yields trigger events until the source is stopped
    /// or the subscription is dropped.
    async fn subscribe(&self) -> Result<BoxStream<'static, TriggerEvent>>;
}
