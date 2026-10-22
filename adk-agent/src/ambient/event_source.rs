use adk_core::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;

/// A trigger event delivered by an [`EventSource`].
///
/// Contains the name of the source that produced it and an arbitrary JSON payload
/// with source-specific metadata (e.g., matched file path, cron tick time, webhook body).
///
/// It carries no idempotency key: a consumer that deduplicates derives one from the payload,
/// as the `adk-kernel` wake sources do (spec 3.2 §7.4, §13 Q7). The struct is
/// `#[non_exhaustive]`; build it with [`new`](Self::new) and
/// [`with_principal`](Self::with_principal).
///
/// # Example
///
/// ```rust
/// use adk_agent::ambient::TriggerEvent;
///
/// let event = TriggerEvent::new("hook", serde_json::json!({"id": 7})).with_principal("partner");
/// assert_eq!(event.principal.as_deref(), Some("partner"));
/// assert_eq!(TriggerEvent::new("cron", serde_json::Value::Null).principal, None);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
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

impl TriggerEvent {
    /// An event from `source` with `payload` and no principal.
    pub fn new(source: impl Into<String>, payload: Value) -> Self {
        Self { source: source.into(), payload, principal: None }
    }

    /// Attributes the event to `principal`, a subject the source verified.
    #[must_use]
    pub fn with_principal(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }
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
