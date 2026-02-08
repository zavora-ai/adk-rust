//! LLM-based event summarizer for context compaction.
//!
//! This module provides [`LlmEventSummarizer`], which uses an LLM to summarize
//! a window of conversation events into a single compacted event. This is the
//! Rust equivalent of ADK Python's `LlmEventSummarizer`.

use adk_core::{
    BaseEventsSummarizer, Content, Event, EventActions, EventCompaction, Llm, LlmRequest, Part,
    Result,
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

    fn format_events_for_prompt(events: &[Event]) -> String {
        let mut lines = Vec::new();
        for event in events {
            if let Some(content) = &event.llm_response.content {
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        lines.push(format!("{}: {}", event.author, text));
                    }
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

        let conversation_history = Self::format_events_for_prompt(events);
        let prompt = self.prompt_template.replace("{conversation_history}", &conversation_history);

        let request = LlmRequest {
            model: self.llm.name().to_string(),
            contents: vec![Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: prompt }],
            }],
            tools: Default::default(),
            config: None,
        };

        // Generate summary (non-streaming)
        let mut response_stream = self.llm.generate_content(request, false).await?;

        use futures::StreamExt;
        let mut summary_content: Option<Content> = None;
        while let Some(chunk_result) = response_stream.next().await {
            if let Ok(chunk) = chunk_result {
                if chunk.content.is_some() {
                    summary_content = chunk.content;
                    break;
                }
            }
        }

        let Some(mut summary) = summary_content else {
            return Ok(None);
        };

        // Ensure the compacted content has the role 'model'
        summary.role = "model".to_string();

        let start_timestamp = events.first().map(|e| e.timestamp).unwrap_or_default();
        let end_timestamp = events.last().map(|e| e.timestamp).unwrap_or_default();

        let compaction =
            EventCompaction { start_timestamp, end_timestamp, compacted_content: summary };

        let actions = EventActions {
            compaction: Some(compaction),
            ..Default::default()
        };

        let mut event = Event::new(Event::new("compaction").invocation_id);
        event.author = "system".to_string();
        event.actions = actions;

        Ok(Some(event))
    }
}
