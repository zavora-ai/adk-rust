//! Intra-invocation context compaction configuration and token estimation.
//!
//! This module provides [`IntraCompactionConfig`] for configuring mid-invocation
//! compaction and [`estimate_tokens`](crate::intra_compaction::estimate_tokens) for heuristic token counting. Unlike the
//! existing [`EventsCompactionConfig`](crate::EventsCompactionConfig) which handles
//! post-invocation compaction based on invocation count, intra-invocation compaction
//! monitors token usage *during* an invocation and triggers summarization before
//! each LLM call when the context exceeds a threshold.
//!
//! The actual summarization reuses the existing [`BaseEventsSummarizer`](crate::BaseEventsSummarizer)
//! trait — this module only provides the config and token estimator.

use crate::Event;
use crate::Part;

/// Configuration for intra-invocation context compaction.
///
/// When attached to a runner, the runner checks `estimate_tokens()` before each
/// LLM call and triggers summarization via [`BaseEventsSummarizer`](crate::BaseEventsSummarizer)
/// when the estimated token count exceeds `token_threshold`.
///
/// # Example
///
/// ```rust
/// use adk_core::IntraCompactionConfig;
///
/// let config = IntraCompactionConfig {
///     token_threshold: 50_000,
///     overlap_event_count: 5,
///     chars_per_token: 4,
/// };
/// ```
#[derive(Debug, Clone)]
pub struct IntraCompactionConfig {
    /// Token count threshold that triggers compaction.
    pub token_threshold: u64,
    /// Number of recent events to preserve after compaction for continuity.
    pub overlap_event_count: usize,
    /// Characters-per-token ratio for estimation (default: 4).
    pub chars_per_token: u32,
}

impl Default for IntraCompactionConfig {
    fn default() -> Self {
        Self { token_threshold: 100_000, overlap_event_count: 10, chars_per_token: 4 }
    }
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
    if !event.actions.state_delta.is_empty()
        && let Ok(json) = serde_json::to_string(&event.actions.state_delta)
    {
        chars += json.len();
    }

    chars
}

/// Estimate the character count of a single content part.
fn estimate_part_chars(part: &Part) -> usize {
    if crate::history::is_provider_replay(part) {
        return crate::history::replay_part(part).as_ref().map_or(0, estimate_part_chars);
    }
    match part {
        Part::Text { text } => text.len(),
        Part::Thinking { thinking, .. } => thinking.len(),
        Part::FunctionCall { name, args, .. } => {
            name.len() + serde_json::to_string(args).map_or(0, |s| s.len())
        }
        Part::FunctionResponse { function_response, .. } => {
            function_response.name.len()
                + serde_json::to_string(&function_response.response).map_or(0, |s| s.len())
        }
        // Binary/file data and server tool calls contribute minimally to text token count
        Part::InlineData { .. } | Part::FileData { .. } => 0,
        Part::ServerToolCall { server_tool_call } => {
            serde_json::to_string(server_tool_call).map_or(0, |s| s.len())
        }
        Part::ServerToolResponse { server_tool_response } => {
            serde_json::to_string(server_tool_response).map_or(0, |s| s.len())
        }
        Part::EmbeddedResource { resource } => {
            serde_json::to_string(resource).map_or(0, |s| s.len())
        }
    }
}

/// A one-line rendering of `part` for a summarizer prompt; `None` for thinking.
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
/// ```
pub fn render_part_for_summary(part: &Part) -> Option<String> {
    if crate::history::is_provider_replay(part) {
        return None;
    }
    let json = |value: &serde_json::Value| value.to_string();
    Some(match part {
        Part::Text { text } => text.clone(),
        Part::Thinking { .. } => return None,
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
        Part::ServerToolCall { server_tool_call } => {
            format!("[server tool call] {}", json(server_tool_call))
        }
        Part::ServerToolResponse { server_tool_response } => {
            format!("[server tool response] {}", json(server_tool_response))
        }
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
    fn test_default_config() {
        let config = IntraCompactionConfig::default();
        assert_eq!(config.token_threshold, 100_000);
        assert_eq!(config.overlap_event_count, 10);
        assert_eq!(config.chars_per_token, 4);
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
    fn counts_wrapped_parts() {
        let original = Content::new("model").with_thinking("Private reasoning").with_text("Answer");
        let mut event = Event::new("inv-1");
        event.set_content(original.clone());
        let expected = estimate_tokens(std::slice::from_ref(&event), 4);
        event.set_content(crate::history::with_provider_parts(
            &original,
            Some(&serde_json::json!({"provider_origin":{
                "provider":"provider", "model":"model", "scope":"owner"
            }})),
        ));
        assert_eq!(estimate_tokens(&[event], 4), expected);
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
