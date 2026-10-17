//! LLM-based event summarizer for context compaction.
//!
//! This module provides [`LlmEventSummarizer`], which uses an LLM to summarize
//! a window of conversation events into a single compacted event. This is the
//! Rust equivalent of ADK Python's `LlmEventSummarizer`.

use adk_core::intra_compaction::render_part_for_summary as render_part;
use adk_core::{
    AdkError, BaseEventsSummarizer, Content, ErrorCategory, ErrorComponent, Event, EventActions,
    EventCompaction, FinishReason, Llm, LlmRequest, Part, Result,
};
use async_trait::async_trait;
use std::sync::Arc;

const DEFAULT_PROMPT_TEMPLATE: &str = "\
The following is a conversation history between a user and an AI agent. \
Please summarize the conversation, focusing on key information and decisions made, \
as well as any unresolved questions or tasks. \
The summary should be concise and capture the essence of the interaction.\n\n\
{conversation_history}";

/// An LLM-based event summarizer for sliding window compaction.
///
/// When called with a list of events, this formats the events, generates a
/// summary using an LLM, and returns a new [`Event`] containing the summary
/// within an [`EventCompaction`].
///
/// The summarizer's input is the window as the model sees it: the most recent earlier
/// summary in the window (an event carrying [`EventActions::compaction`]) replaces the events
/// it covers, so its content carries into the new summary, and every later event contributes
/// its text, function calls and responses, file and inline data references, and embedded
/// resources. Thinking parts and streamed partial events are left out.
///
/// A summary replaces history only when it is complete and smaller. A stream error fails
/// the summary, and so does a model that stops for any reason but `Stop` (for example
/// `MaxTokens`), with `model.summary_incomplete`. An empty summary, or one at least as long
/// as the rendered window it would replace, returns `Ok(None)`, so the runner keeps the
/// history as it is.
///
/// The summary call goes through [`adk_core::generate_metered`], so when the
/// runner compacts a session it counts against the run's budget and spend
/// ledger, and a call the budget refuses is never sent.
pub struct LlmEventSummarizer {
    llm: Arc<dyn Llm>,
    prompt_template: String,
}

impl LlmEventSummarizer {
    /// Create a new summarizer using the given LLM.
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self { llm, prompt_template: DEFAULT_PROMPT_TEMPLATE.to_string() }
    }

    /// Create a new summarizer with a custom prompt template.
    /// The template must contain `{conversation_history}` as a placeholder.
    pub fn with_prompt_template(mut self, template: impl Into<String>) -> Self {
        self.prompt_template = template.into();
        self
    }

    /// Renders the window the summary replaces: the latest earlier summary, then each event
    /// after the span that summary covers.
    fn format_events_for_prompt(events: &[Event], earlier: Option<&EventCompaction>) -> String {
        let mut lines: Vec<String> = earlier
            .into_iter()
            .flat_map(|compaction| compaction.compacted_content.parts.iter())
            .filter_map(render_part)
            .map(|summary| format!("summary of earlier conversation: {summary}"))
            .collect();
        for event in events {
            let covered = earlier.is_some_and(|c| event.timestamp <= c.end_timestamp);
            if event.actions.compaction.is_some() || event.llm_response.partial || covered {
                continue;
            }
            if let Some(content) = &event.llm_response.content {
                for rendered in content.parts.iter().filter_map(render_part) {
                    lines.push(format!("{}: {rendered}", event.author));
                }
            }
        }
        lines.join("\n")
    }
}

#[async_trait]
impl BaseEventsSummarizer for LlmEventSummarizer {
    async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>> {
        if events.is_empty() {
            return Ok(None);
        }

        let earlier = events.iter().rev().find_map(|event| event.actions.compaction.as_ref());
        let conversation_history = Self::format_events_for_prompt(events, earlier);
        let prompt = self.prompt_template.replace("{conversation_history}", &conversation_history);

        let request = LlmRequest {
            model: self.llm.name().to_string(),
            contents: vec![Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: prompt }],
            }],
            tools: Default::default(),
            config: None,
            previous_response_id: None,
        };

        // Counts against the run's budget and spend ledger when the runner made a scope current.
        let mut response_stream =
            adk_core::generate_metered(self.llm.as_ref(), request, false).await?;

        use futures::StreamExt;
        let mut summary_text = String::new();
        let mut finish_reason = None;
        // Drained to the end so the call's usage, which may follow the content, is metered.
        while let Some(chunk) = response_stream.next().await {
            let chunk = chunk?;
            for part in chunk.content.iter().flat_map(|content| content.parts.iter()) {
                if let Part::Text { text } = part {
                    summary_text.push_str(text);
                }
            }
            finish_reason = chunk.finish_reason.or(finish_reason);
        }

        // A summary the model cut short would replace history with part of what it said.
        if let Some(
            reason @ (FinishReason::MaxTokens
            | FinishReason::Safety
            | FinishReason::Recitation
            | FinishReason::Other),
        ) = finish_reason
        {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.summary_incomplete",
                format!(
                    "the summarizer model stopped before finishing its summary ({reason:?}); \
                     the history is kept as it is. Raise the summarizer's output limit or \
                     shorten the compaction window"
                ),
            ));
        }
        if summary_text.trim().is_empty() {
            return Ok(None);
        }
        // A summary at least as long as what it replaces would not shrink the request.
        if summary_text.len() >= conversation_history.len() {
            tracing::debug!(
                summary.chars = summary_text.len(),
                window.chars = conversation_history.len(),
                "summary is not smaller than the history it replaces; skipping compaction"
            );
            return Ok(None);
        }
        let summary = Content::new("model").with_text(summary_text);

        // A new summary also stands in for the span the earlier one covered.
        let start_timestamp = events
            .first()
            .map(|e| e.timestamp)
            .into_iter()
            .chain(earlier.map(|compaction| compaction.start_timestamp))
            .min()
            .unwrap_or_default();
        let end_timestamp = events.last().map(|e| e.timestamp).unwrap_or_default();

        let compaction =
            EventCompaction { start_timestamp, end_timestamp, compacted_content: summary };

        let actions = EventActions { compaction: Some(compaction), ..Default::default() };

        let mut event = Event::new(Event::new("compaction").invocation_id);
        event.author = "system".to_string();
        event.actions = actions;

        Ok(Some(event))
    }
}
