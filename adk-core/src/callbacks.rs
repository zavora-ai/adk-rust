use crate::{CallbackContext, Content, LlmRequest, LlmResponse, ReadonlyContext, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// Agent callbacks
pub type BeforeAgentCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>>
        + Send
        + Sync,
>;
pub type AfterAgentCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>>
        + Send
        + Sync,
>;

/// Result from a BeforeModel callback
#[derive(Debug)]
pub enum BeforeModelResult {
    /// Continue with the (possibly modified) request
    Continue(LlmRequest),
    /// Skip the model call and use this response instead
    Skip(LlmResponse),
}

// Model callbacks
// BeforeModelCallback can modify the request or skip the model call entirely
pub type BeforeModelCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
            LlmRequest,
        ) -> Pin<Box<dyn Future<Output = Result<BeforeModelResult>> + Send>>
        + Send
        + Sync,
>;
pub type AfterModelCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
            LlmResponse,
        ) -> Pin<Box<dyn Future<Output = Result<Option<LlmResponse>>> + Send>>
        + Send
        + Sync,
>;

// Tool callbacks
pub type BeforeToolCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>>
        + Send
        + Sync,
>;
pub type AfterToolCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>>
        + Send
        + Sync,
>;

// Instruction providers - dynamic instruction generation
pub type InstructionProvider = Box<
    dyn Fn(Arc<dyn ReadonlyContext>) -> Pin<Box<dyn Future<Output = Result<String>> + Send>>
        + Send
        + Sync,
>;
pub type GlobalInstructionProvider = InstructionProvider;

// ===== Context Compaction =====

use crate::Event;
use async_trait::async_trait;

/// Trait for summarizing events during context compaction.
///
/// Implementations receive a window of events and produce a single
/// compacted event containing a summary. The runner calls this when
/// the compaction interval is reached.
#[async_trait]
pub trait BaseEventsSummarizer: Send + Sync {
    /// Summarize the given events into a single compacted event.
    /// Returns `None` if no compaction is needed (e.g., empty input).
    async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>>;
}

/// Configuration for automatic context compaction.
///
/// Mirrors ADK Python's `EventsCompactionConfig`:
/// - `compaction_interval`: Number of invocations before triggering compaction
/// - `overlap_size`: Events from the previous window to include in the next summary
/// - `summarizer`: The strategy used to produce summaries
#[derive(Clone)]
pub struct EventsCompactionConfig {
    /// Number of completed invocations that triggers compaction.
    pub compaction_interval: u32,
    /// How many events from the previous compacted window to include
    /// in the next compaction for continuity.
    pub overlap_size: u32,
    /// The summarizer implementation (e.g., LLM-based).
    pub summarizer: Arc<dyn BaseEventsSummarizer>,
}
