//! Auto-compaction: when an agent's history is compacted during a run, the token estimate
//! that decides it, and the hook an agent calls before each model call.
//!
//! [`IntraCompactionConfig`] sets the threshold: by default a fraction of the model's
//! context window ([`Llm::context_window`]), or [`DEFAULT_TOKEN_THRESHOLD`] when the model
//! reports none. [`HistoryCompactor`] is the hook: the runner installs one on
//! [`RunConfig::history_compactor`](crate::RunConfig::history_compactor) at the start of each
//! run, and `LlmAgent` and `CodeActAgent` call it before every model call, and once more when
//! the model rejects a request as longer than its context window
//! ([`is_context_limit_error`]).
//!
//! The session-level [`EventsCompactionConfig`](crate::EventsCompactionConfig) compacts after
//! every N invocations instead; both summarize through
//! [`BaseEventsSummarizer`](crate::BaseEventsSummarizer).

use std::sync::Arc;

use async_trait::async_trait;

use crate::{AdkError, Content, ErrorCategory, ErrorComponent, Event, InvocationContext, Llm, Part};

/// The threshold, in estimated tokens, when the model reports no context window.
///
/// The smallest context window of a current hosted model is about 128,000 tokens; this stays
/// below it.
pub const DEFAULT_TOKEN_THRESHOLD: u64 = 100_000;

/// The share of the model's context window a request may reach before it is compacted.
///
/// The rest leaves room for the response and for the error of a character-based estimate.
pub const DEFAULT_CONTEXT_WINDOW_FRACTION: f64 = 0.8;

/// When an agent's history is compacted during a run.
///
/// Before each model call, `LlmAgent` and `CodeActAgent` ask the run's
/// [`HistoryCompactor`] to compact their history once the estimated request
/// ([`estimate_content_tokens`]) or the prompt size the provider reported for the previous
/// call passes [`threshold_for`](Self::threshold_for) the model. The runner's compactor
/// summarizes all but the most recent `overlap_event_count` history entries.
///
/// The default is on: the threshold is [`DEFAULT_CONTEXT_WINDOW_FRACTION`] of the model's
/// context window, or [`DEFAULT_TOKEN_THRESHOLD`] when the model reports none. A fixed
/// `token_threshold` takes precedence over the window.
///
/// # Example
///
/// ```rust
/// use adk_core::IntraCompactionConfig;
/// use adk_core::intra_compaction::DEFAULT_TOKEN_THRESHOLD;
///
/// // On by default, at 80% of a 1,000,000-token window.
/// let config = IntraCompactionConfig::default();
/// assert_eq!(config.threshold_for(Some(1_000_000)), 800_000);
/// assert_eq!(config.threshold_for(None), DEFAULT_TOKEN_THRESHOLD);
///
/// // A fixed threshold, whatever the window.
/// let fixed = IntraCompactionConfig::at_tokens(50_000);
/// assert_eq!(fixed.threshold_for(Some(1_000_000)), 50_000);
///
/// assert!(!IntraCompactionConfig::disabled().enabled);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct IntraCompactionConfig {
    /// Whether history is compacted during a run.
    pub enabled: bool,
    /// A fixed threshold in estimated tokens. `None` derives it from the model's context
    /// window.
    pub token_threshold: Option<u64>,
    /// The share of the model's context window a request may reach, when `token_threshold`
    /// is `None` and the model reports its window. Values outside `(0, 1]` use
    /// [`DEFAULT_CONTEXT_WINDOW_FRACTION`].
    pub context_window_fraction: f64,
    /// How many of the most recent history entries are kept as they are.
    pub overlap_event_count: usize,
    /// Characters per token for the estimate (default: 4).
    pub chars_per_token: u32,
}

impl Default for IntraCompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            token_threshold: None,
            context_window_fraction: DEFAULT_CONTEXT_WINDOW_FRACTION,
            overlap_event_count: 10,
            chars_per_token: 4,
        }
    }
}

impl IntraCompactionConfig {
    /// Compaction turned off.
    pub fn disabled() -> Self {
        Self { enabled: false, ..Self::default() }
    }

    /// Compaction once a request passes `threshold` estimated tokens, whatever the model's
    /// window.
    pub fn at_tokens(threshold: u64) -> Self {
        Self { token_threshold: Some(threshold), ..Self::default() }
    }

    /// Compaction once a request passes `fraction` of the model's context window.
    pub fn at_window_fraction(fraction: f64) -> Self {
        Self { context_window_fraction: fraction, ..Self::default() }
    }

    /// Keeps the `count` most recent history entries as they are.
    pub fn with_overlap_event_count(mut self, count: usize) -> Self {
        self.overlap_event_count = count;
        self
    }

    /// The threshold, in estimated tokens, for a model with `context_window` tokens.
    ///
    /// A fixed `token_threshold` wins; otherwise the window times
    /// `context_window_fraction`, or [`DEFAULT_TOKEN_THRESHOLD`] for an unknown window.
    pub fn threshold_for(&self, context_window: Option<u64>) -> u64 {
        if let Some(threshold) = self.token_threshold {
            return threshold;
        }
        let Some(window) = context_window else {
            return DEFAULT_TOKEN_THRESHOLD;
        };
        let fraction = if self.context_window_fraction > 0.0 && self.context_window_fraction <= 1.0
        {
            self.context_window_fraction
        } else {
            DEFAULT_CONTEXT_WINDOW_FRACTION
        };
        (window as f64 * fraction) as u64
    }
}

/// Why an agent asks its run's [`HistoryCompactor`] to compact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTrigger {
    /// Before a model call: compacts when the request has passed the threshold.
    BeforeModelCall,
    /// The model rejected the request as longer than its context window
    /// ([`is_context_limit_error`]): compacts whatever the estimate says, so the call can be
    /// sent once more.
    ContextLimitExceeded,
}

/// What an agent asks its run's [`HistoryCompactor`] to compact.
#[derive(Debug, Clone, Copy)]
pub struct HistoryCompactionRequest<'a> {
    /// Contents sent with every request and never compacted, such as the agent's
    /// instructions. They count toward the estimate.
    pub preamble: &'a [Content],
    /// The conversation the agent continues from, which compaction may shorten.
    pub history: &'a [Content],
    /// The prompt tokens the provider reported for the agent's previous request in this run,
    /// a lower bound on the size of the next one until the history is compacted.
    pub reported_prompt_tokens: Option<u64>,
    /// Why compaction is asked for.
    pub trigger: CompactionTrigger,
}

/// A compacted history.
#[derive(Debug, Clone)]
pub struct CompactedHistory {
    /// What replaces [`HistoryCompactionRequest::history`]: a summary, then the most recent
    /// entries as they were.
    pub history: Vec<Content>,
    /// The event that records the compaction in the session
    /// ([`EventActions::compaction`](crate::EventActions::compaction)), so later runs start
    /// from the summary. The agent yields it. `None` when the compaction is kept to this
    /// agent's working history.
    pub event: Option<Event>,
}

/// Compacts an agent's history before a model call.
///
/// The runner installs one on [`RunConfig::history_compactor`](crate::RunConfig::history_compactor)
/// at the start of each run, from [`IntraCompactionConfig`] and a summarizer or compaction
/// strategy. `LlmAgent` and `CodeActAgent` call it before every model call
/// ([`CompactionTrigger::BeforeModelCall`]) and once after a model rejects a request as too
/// long ([`CompactionTrigger::ContextLimitExceeded`]); an agent that calls a model itself does
/// the same. `Ok(None)` leaves the history as it is.
///
/// A compactor never separates a function response from its call
/// ([`balanced_content_cut`](crate::history::balanced_content_cut)), keeps a call still held
/// for confirmation and everything after it
/// ([`held_call_content_limit`](crate::history::held_call_content_limit)), and makes any model
/// call inside a [`ModelCallScope`](crate::ModelCallScope) for `ctx`, so it counts against the
/// run's budget and spend ledger.
///
/// # Example
///
/// ```rust
/// use adk_core::intra_compaction::{
///     CompactedHistory, HistoryCompactionRequest, HistoryCompactor,
/// };
/// use adk_core::{InvocationContext, Llm, Result, async_trait};
/// use std::sync::Arc;
///
/// /// Keeps the 20 most recent entries once there are more than 40.
/// #[derive(Debug)]
/// struct KeepRecent;
///
/// #[async_trait]
/// impl HistoryCompactor for KeepRecent {
///     async fn compact(
///         &self,
///         _ctx: Arc<dyn InvocationContext>,
///         _model: Arc<dyn Llm>,
///         request: HistoryCompactionRequest<'_>,
///     ) -> Result<Option<CompactedHistory>> {
///         let history = request.history;
///         if history.len() <= 40 {
///             return Ok(None);
///         }
///         let cut = adk_core::history::balanced_content_cut(history, history.len() - 20);
///         let note = adk_core::history::system_notice("Earlier turns were removed.");
///         let kept = std::iter::once(note).chain(history[cut..].iter().cloned()).collect();
///         Ok(Some(CompactedHistory { history: kept, event: None }))
///     }
/// }
/// # let _: Arc<dyn HistoryCompactor> = Arc::new(KeepRecent);
/// ```
#[async_trait]
pub trait HistoryCompactor: Send + Sync + std::fmt::Debug {
    /// Compacts `request.history` for a call to `model` by the agent `ctx` runs, or returns
    /// `Ok(None)` to leave it as it is.
    ///
    /// # Errors
    ///
    /// An error ends the agent's turn with it. The runner's compactor returns none: a summary
    /// that fails leaves the history as it is.
    async fn compact(
        &self,
        ctx: Arc<dyn InvocationContext>,
        model: Arc<dyn Llm>,
        request: HistoryCompactionRequest<'_>,
    ) -> crate::Result<Option<CompactedHistory>>;
}

/// The first line of every compaction summary [`compaction_summary`] builds.
pub const COMPACTION_SUMMARY_HEADING: &str =
    "The earlier conversation was compacted to fit the model's context window. Summary:";

/// The history entry a compaction summary takes: a
/// [system notice](crate::history::system_notice) headed [`COMPACTION_SUMMARY_HEADING`].
///
/// A user-role notice, so a request that starts with it is valid on every provider, and it
/// never stands in for the person's last message.
///
/// # Example
///
/// ```rust
/// use adk_core::history::is_system_notice;
/// use adk_core::intra_compaction::compaction_summary;
///
/// let summary = compaction_summary("The user asked for the Q3 figures; two were found.");
/// assert!(is_system_notice(&summary));
/// ```
pub fn compaction_summary(summary: &str) -> Content {
    crate::history::system_notice(&format!("{COMPACTION_SUMMARY_HEADING}\n{summary}"))
}

/// Whether `err` is a model's refusal of a request longer than its context window.
///
/// Providers report it as a client error (`InvalidInput`) with their own wording: Anthropic's
/// "prompt is too long", OpenAI's `context_length_exceeded` ("maximum context length"),
/// Gemini's "input token count exceeds the maximum number of tokens allowed", and the
/// runner's own `runner.context_overflow`.
///
/// # Example
///
/// ```rust
/// use adk_core::{AdkError, ErrorCategory, ErrorComponent};
/// use adk_core::intra_compaction::is_context_limit_error;
///
/// let too_long = AdkError::new(
///     ErrorComponent::Model,
///     ErrorCategory::InvalidInput,
///     "model.anthropic.api_error",
///     "400: prompt is too long: 1048577 tokens > 1000000 maximum (invalid_request_error)",
/// );
/// assert!(is_context_limit_error(&too_long));
/// assert!(!is_context_limit_error(&AdkError::model("connection refused")));
/// ```
pub fn is_context_limit_error(err: &AdkError) -> bool {
    if err.component != ErrorComponent::Model || err.category != ErrorCategory::InvalidInput {
        return false;
    }
    if err.code == "runner.context_overflow" {
        return true;
    }
    let message = err.message.to_lowercase();
    [
        "prompt is too long",
        "context length",
        "context_length_exceeded",
        "context window",
        "context limit",
        "maximum context",
        "too many tokens",
        "input is too long",
        "input too long",
        "input token count",
        "exceeds the maximum number of tokens",
        "request payload size exceeds",
        "reduce the length of the messages",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

/// Estimate token count from a list of events using a character heuristic.
///
/// Sums the character lengths of all text parts and serialized function call/response
/// actions across all events, then divides by `chars_per_token` (integer division).
///
/// # Arguments
///
/// * `events` - The conversation events to estimate tokens for.
/// * `chars_per_token` - The character-to-token ratio (e.g., 4 means ~4 chars per token).
///
/// # Returns
///
/// Estimated token count. Returns 0 if `chars_per_token` is 0 or events are empty.
///
/// # Example
///
/// ```rust
/// use adk_core::intra_compaction::estimate_tokens;
/// use adk_core::{Event, Content, Part};
///
/// let mut event = Event::new("inv-1");
/// event.set_content(Content::new("user").with_text("Hello, world!"));
/// let tokens = estimate_tokens(&[event], 4);
/// assert_eq!(tokens, 3); // 13 chars / 4 = 3
/// ```
pub fn estimate_tokens(events: &[Event], chars_per_token: u32) -> u64 {
    if chars_per_token == 0 {
        return 0;
    }
    let total_chars: u64 = events.iter().map(|e| estimate_event_chars(e) as u64).sum();
    total_chars / chars_per_token as u64
}

/// Estimate the token count of model contents with the [`estimate_tokens`] heuristic.
///
/// # Example
///
/// ```rust
/// use adk_core::Content;
/// use adk_core::intra_compaction::estimate_content_tokens;
///
/// let history = [Content::new("user").with_text("Hello, world!")];
/// assert_eq!(estimate_content_tokens(&history, 4), 3);
/// ```
pub fn estimate_content_tokens(contents: &[Content], chars_per_token: u32) -> u64 {
    if chars_per_token == 0 {
        return 0;
    }
    let chars: u64 = contents
        .iter()
        .flat_map(|content| &content.parts)
        .map(|part| estimate_part_chars(part) as u64)
        .sum();
    chars / chars_per_token as u64
}

/// Estimate the character count of a single event.
///
/// Counts characters from:
/// - Text parts (text length)
/// - Thinking parts (thinking text length)
/// - Function call parts (serialized args length + name length)
/// - Function response parts (serialized response length + name length)
/// - A compaction event's summary, which stands in for the events it covers
/// - Serialized actions (state_delta as JSON)
fn estimate_event_chars(event: &Event) -> usize {
    let mut chars = 0;

    let summary = event.actions.compaction.as_ref().map(|compaction| &compaction.compacted_content);
    for content in event.llm_response.content.iter().chain(summary) {
        for part in &content.parts {
            chars += estimate_part_chars(part);
        }
    }

    // Count serialized actions (state_delta)
    if !event.actions.state_delta.is_empty() {
        chars += json_len(&event.actions.state_delta);
    }

    chars
}

/// Estimate the character count of a single content part.
fn estimate_part_chars(part: &Part) -> usize {
    match part {
        Part::Text { text } => text.len(),
        Part::Thinking { thinking, .. } => thinking.len(),
        Part::FunctionCall { name, args, .. } => name.len() + json_len(args),
        Part::FunctionResponse { function_response, .. } => {
            function_response.name.len() + json_len(&function_response.response)
        }
        // Binary/file data and server tool calls contribute minimally to text token count
        Part::InlineData { .. } | Part::FileData { .. } => 0,
        Part::ServerToolCall { server_tool_call } => json_len(server_tool_call),
        Part::ServerToolResponse { server_tool_response } => json_len(server_tool_response),
        Part::EmbeddedResource { resource } => json_len(resource),
    }
}

/// The serialized JSON length of `value`, counted without building the string, since the
/// estimate runs before every model call.
fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value).map_or(0, |()| count.0)
}


/// A one-line rendering of `part` for a summarizer prompt; `None` for thinking and for
/// provider-native items (server-side tool calls and results).
///
/// Text is kept as it is. Function calls and responses keep their name and JSON payload,
/// data and file parts their type and reference, and embedded resources their URI, so a
/// summary keeps the facts a tool returned. The session compaction summarizer
/// (`LlmEventSummarizer`) and the runner's `SummarisationCompaction` both render events with
/// it.
///
/// # Example
///
/// ```rust
/// use adk_core::intra_compaction::render_part_for_summary;
/// use adk_core::{FunctionResponseData, Part};
/// use serde_json::json;
///
/// let result = Part::FunctionResponse {
///     function_response: FunctionResponseData::new("lookup", json!({"status": "shipped"})),
///     id: None,
///     annotations: None,
/// };
/// assert_eq!(
///     render_part_for_summary(&result).as_deref(),
///     Some(r#"[function response] lookup: {"status":"shipped"}"#)
/// );
/// let thought = Part::Thinking { thinking: "hmm".into(), signature: None };
/// assert_eq!(render_part_for_summary(&thought), None);
/// let native = Part::ServerToolResponse { server_tool_response: json!({"type": "anthropic_message"}) };
/// assert_eq!(render_part_for_summary(&native), None);
/// ```
pub fn render_part_for_summary(part: &Part) -> Option<String> {
    let json = |value: &serde_json::Value| value.to_string();
    Some(match part {
        Part::Text { text } => text.clone(),
        // Provider-native items are private to the provider that produced them; a payload can
        // hold its reasoning, and the summary is sent to whichever model summarizes.
        Part::Thinking { .. } | Part::ServerToolCall { .. } | Part::ServerToolResponse { .. } => {
            return None;
        }
        Part::FunctionCall { name, args, .. } => format!("[function call] {name}({})", json(args)),
        Part::FunctionResponse { function_response, .. } => {
            let mut rendered = format!(
                "[function response] {}: {}",
                function_response.name,
                json(&function_response.response)
            );
            for data in &function_response.inline_data {
                rendered.push_str(&format!(" [inline data: {}]", data.mime_type));
            }
            for file in &function_response.file_data {
                rendered.push_str(&format!(" [file: {} ({})]", file.file_uri, file.mime_type));
            }
            rendered
        }
        Part::InlineData { mime_type, data, .. } => {
            format!("[inline data: {mime_type}, {} bytes]", data.len())
        }
        Part::FileData { mime_type, file_uri, .. } => format!("[file: {file_uri} ({mime_type})]"),
        Part::EmbeddedResource { resource } => match resource {
            crate::EmbeddedResource::Text(text) => format!("[resource {}] {}", text.uri, text.text),
            crate::EmbeddedResource::Blob(blob) => format!(
                "[resource {}: {}, {} bytes]",
                blob.uri,
                blob.mime_type.as_deref().unwrap_or("application/octet-stream"),
                blob.data.len()
            ),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Content, FunctionResponseData};

    #[test]
    fn the_default_is_on_at_a_fraction_of_the_window() {
        assert_eq!(
            IntraCompactionConfig::default(),
            IntraCompactionConfig {
                enabled: true,
                token_threshold: None,
                context_window_fraction: 0.8,
                overlap_event_count: 10,
                chars_per_token: 4,
            }
        );
    }

    #[test]
    fn a_fixed_threshold_wins_and_a_bad_fraction_falls_back() {
        let config = IntraCompactionConfig::default();
        assert_eq!(config.threshold_for(Some(200_000)), 160_000);
        assert_eq!(config.threshold_for(None), DEFAULT_TOKEN_THRESHOLD);
        assert_eq!(IntraCompactionConfig::at_tokens(5).threshold_for(Some(200_000)), 5);
        for fraction in [0.0, -1.0, 1.5, f64::NAN] {
            let config = IntraCompactionConfig::at_window_fraction(fraction);
            assert_eq!(config.threshold_for(Some(100)), 80, "fraction {fraction}");
        }
        assert_eq!(IntraCompactionConfig::at_window_fraction(0.5).threshold_for(Some(100)), 50);
    }

    #[test]
    fn context_limit_errors_are_told_from_other_client_errors() {
        let model_error = |message: &str| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.api_error",
                message.to_string(),
            )
        };
        for message in [
            "400: prompt is too long: 1048577 tokens > 1000000 maximum (invalid_request_error)",
            "This model's maximum context length is 128000 tokens",
            "context_length_exceeded",
            "input length and `max_tokens` exceed context limit: 190000 + 32000 > 200000",
            "The input token count (1100000) exceeds the maximum number of tokens allowed (1048576).",
        ] {
            assert!(is_context_limit_error(&model_error(message)), "{message}");
        }
        for message in [
            "max_tokens: 200000 > 128000, which is the maximum allowed number of output tokens",
            "invalid x-api-key",
            "tools.0.name: String should match pattern",
        ] {
            assert!(!is_context_limit_error(&model_error(message)), "{message}");
        }
        let unavailable = AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Unavailable,
            "model.api_error",
            "prompt is too long",
        );
        assert!(!is_context_limit_error(&unavailable));
    }

    #[test]
    fn contents_are_estimated_like_events() {
        let call = Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "get_weather".to_string(),
                args: serde_json::json!({"city": "NYC"}),
                id: None,
                thought_signature: None,
            }],
        };
        let mut event = Event::new("inv-1");
        event.set_content(call.clone());
        // "get_weather" (11) + {"city":"NYC"} (14) = 25 chars.
        assert_eq!(estimate_content_tokens(&[call], 1), 25);
        assert_eq!(estimate_tokens(&[event], 1), 25);
    }

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(&[], 4), 0);
    }

    #[test]
    fn test_estimate_tokens_zero_ratio() {
        let mut event = Event::new("inv-1");
        event.set_content(Content::new("user").with_text("Hello"));
        assert_eq!(estimate_tokens(&[event], 0), 0);
    }

    #[test]
    fn test_estimate_tokens_text_only() {
        let mut event = Event::new("inv-1");
        // "Hello" = 5 chars, 5 / 4 = 1
        event.set_content(Content::new("user").with_text("Hello"));
        assert_eq!(estimate_tokens(&[event], 4), 1);
    }

    #[test]
    fn test_estimate_tokens_multiple_events() {
        let mut e1 = Event::new("inv-1");
        e1.set_content(Content::new("user").with_text("Hello")); // 5 chars
        let mut e2 = Event::new("inv-1");
        e2.set_content(Content::new("model").with_text("World!")); // 6 chars
        // Total: 11 chars / 4 = 2
        assert_eq!(estimate_tokens(&[e1, e2], 4), 2);
    }

    #[test]
    fn test_estimate_tokens_with_function_call() {
        let mut event = Event::new("inv-1");
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "get_weather".to_string(),
                args: serde_json::json!({"city": "NYC"}),
                id: None,
                thought_signature: None,
            }],
        });
        let tokens = estimate_tokens(&[event], 4);
        // "get_weather" = 11 chars + {"city":"NYC"} serialized
        assert!(tokens > 0);
    }

    #[test]
    fn a_compaction_summary_counts_toward_the_estimate() {
        let mut event = Event::new("inv-1");
        event.actions.compaction = Some(crate::EventCompaction {
            start_timestamp: chrono::Utc::now(),
            end_timestamp: chrono::Utc::now(),
            compacted_content: Content::new("model").with_text("a".repeat(40)),
        });
        assert_eq!(estimate_tokens(&[event], 4), 10);
    }

    #[test]
    fn test_estimate_tokens_with_function_response() {
        let mut event = Event::new("inv-1");
        event.llm_response.content = Some(Content {
            role: "function".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    "get_weather",
                    serde_json::json!({"temp": 72}),
                ),
                id: None,
                annotations: None,
            }],
        });
        let tokens = estimate_tokens(&[event], 4);
        assert!(tokens > 0);
    }
}
