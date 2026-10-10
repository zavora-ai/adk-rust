use crate::{CallbackContext, Content, LlmRequest, LlmResponse, ReadonlyContext, Result, Tool};
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// Agent callbacks
/// Callback invoked before an agent runs. Return `Ok(Some(content))` to short-circuit.
pub type BeforeAgentCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>>
        + Send
        + Sync,
>;
/// Callback invoked after an agent completes. Return `Ok(Some(content))` to override.
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
/// Callback invoked before a model call. Can modify the request or skip the call entirely.
pub type BeforeModelCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
            LlmRequest,
        ) -> Pin<Box<dyn Future<Output = Result<BeforeModelResult>> + Send>>
        + Send
        + Sync,
>;
/// Callback invoked after a model call. Return `Ok(Some(response))` to override.
pub type AfterModelCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
            LlmResponse,
        ) -> Pin<Box<dyn Future<Output = Result<Option<LlmResponse>>> + Send>>
        + Send
        + Sync,
>;

// Tool callbacks
/// Callback invoked before a tool executes. Return `Ok(Some(content))` to skip execution.
pub type BeforeToolCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>>
        + Send
        + Sync,
>;
/// Callback invoked after a tool executes. Return `Ok(Some(content))` to override the result.
pub type AfterToolCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>>
        + Send
        + Sync,
>;

/// Rich after-tool callback that receives the tool, arguments, and response.
///
/// Aligned with the Python/Go ADK model where `after_tool_callback` receives
/// the full tool execution context: the tool itself, the arguments it was
/// called with, and the response it produced (or error JSON).
///
/// This is the V2 callback surface for first-class tool result handling.
/// Unlike [`AfterToolCallback`] (which only receives `CallbackContext`),
/// this callback can inspect and modify tool results without relying on
/// `ToolOutcome` inspection.
///
/// Return `Ok(None)` to keep the original response, or `Ok(Some(value))`
/// to replace the function response sent to the LLM.
pub type AfterToolCallbackFull = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
            Arc<dyn Tool>,
            serde_json::Value, // args
            serde_json::Value, // tool response (success result or error JSON)
        ) -> Pin<Box<dyn Future<Output = Result<Option<serde_json::Value>>> + Send>>
        + Send
        + Sync,
>;

// Instruction providers - dynamic instruction generation
/// Async function that generates dynamic instructions from context.
pub type InstructionProvider = Box<
    dyn Fn(Arc<dyn ReadonlyContext>) -> Pin<Box<dyn Future<Output = Result<String>> + Send>>
        + Send
        + Sync,
>;
/// Alias for [`InstructionProvider`] used at the global (runner) level.
pub type GlobalInstructionProvider = InstructionProvider;

// ===== Error Callbacks =====

/// Callback invoked when a tool execution fails (after retries are exhausted).
///
/// This is the canonical, framework-level tool-error callback type shared by
/// `adk-agent` (builder registration) and `adk-plugin` (plugin hooks).
///
/// Returns `Ok(Some(value))` to substitute a fallback result as the function
/// response to the LLM, or `Ok(None)` to let the next callback (or the
/// original error) propagate.
pub type OnToolErrorCallback = Box<
    dyn Fn(
            Arc<dyn CallbackContext>,
            Arc<dyn Tool>,
            serde_json::Value, // args
            String,            // error message
        ) -> Pin<Box<dyn Future<Output = Result<Option<serde_json::Value>>> + Send>>
        + Send
        + Sync,
>;

// ===== Invocation Hooks =====

/// Hooks applied to every model and tool call in a runner invocation.
///
/// An agent's own callbacks cover only that agent. Hooks installed on
/// [`RunConfig::invocation_hooks`](crate::RunConfig::invocation_hooks) reach every agent the
/// invocation runs — transfer targets and agents behind an agent tool included — because the
/// `RunConfig` travels with the invocation. The runner installs its plugin manager here.
///
/// An agent runs these hooks ahead of its own callbacks of the same kind, with the same
/// semantics: a hook that returns a value short-circuits the agent's callbacks of that kind, and
/// a hook that returns an error is handled exactly as a failing agent callback.
///
/// Every method defaults to a pass-through, so an implementation overrides only the hooks it
/// needs.
///
/// # Example
///
/// ```rust
/// use adk_core::{CallbackContext, Content, InvocationHooks, Result, RunConfig, async_trait};
/// use std::sync::Arc;
///
/// /// Refuses every call to `delete_file`, whichever agent makes it.
/// #[derive(Debug)]
/// struct NoDeletes;
///
/// #[async_trait]
/// impl InvocationHooks for NoDeletes {
///     async fn before_tool(&self, ctx: Arc<dyn CallbackContext>) -> Result<Option<Content>> {
///         if ctx.tool_name() == Some("delete_file") {
///             return Ok(Some(Content::new("function").with_text("delete_file is disabled")));
///         }
///         Ok(None)
///     }
/// }
///
/// let config = RunConfig::builder().invocation_hook(Arc::new(NoDeletes)).build();
/// assert_eq!(config.invocation_hooks.len(), 1);
/// ```
#[async_trait]
pub trait InvocationHooks: std::fmt::Debug + Send + Sync {
    /// Runs before an agent starts. `Some(content)` skips the agent and emits `content`.
    async fn before_agent(&self, _ctx: Arc<dyn CallbackContext>) -> Result<Option<Content>> {
        Ok(None)
    }

    /// Runs after an agent finishes. `Some(content)` is emitted as a final event.
    async fn after_agent(&self, _ctx: Arc<dyn CallbackContext>) -> Result<Option<Content>> {
        Ok(None)
    }

    /// Runs before each model call. May rewrite the request or skip the call.
    async fn before_model(
        &self,
        _ctx: Arc<dyn CallbackContext>,
        request: LlmRequest,
    ) -> Result<BeforeModelResult> {
        Ok(BeforeModelResult::Continue(request))
    }

    /// Runs on each model response chunk. `Some(response)` replaces the chunk.
    async fn after_model(
        &self,
        _ctx: Arc<dyn CallbackContext>,
        _response: LlmResponse,
    ) -> Result<Option<LlmResponse>> {
        Ok(None)
    }

    /// Runs before each tool call. `ctx.tool_name()` and `ctx.tool_input()` describe the call.
    ///
    /// `Some(content)` skips the tool and reports `content` as its result.
    async fn before_tool(&self, _ctx: Arc<dyn CallbackContext>) -> Result<Option<Content>> {
        Ok(None)
    }

    /// Runs after each tool call. `Some(content)` replaces the tool's response.
    async fn after_tool(&self, _ctx: Arc<dyn CallbackContext>) -> Result<Option<Content>> {
        Ok(None)
    }

    /// Runs when a tool fails after its retries. `Some(value)` replaces the error result.
    async fn on_tool_error(
        &self,
        _ctx: Arc<dyn CallbackContext>,
        _tool: Arc<dyn Tool>,
        _args: serde_json::Value,
        _error: String,
    ) -> Result<Option<serde_json::Value>> {
        Ok(None)
    }
}

// ===== Context Compaction =====

use crate::Event;

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
