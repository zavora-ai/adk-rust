use crate::{Event, Part};
use std::{borrow::Cow, collections::HashMap};

/// Converts complete response snapshots to text and thinking deltas.
///
/// Create one adapter per live stream for append-only output. Keep the original
/// events for persistence and consumers that support content replacement.
///
/// # Example
///
/// ```
/// use adk_core::{Content, Event, EventTextDeltas};
///
/// let mut deltas = EventTextDeltas::default();
/// let mut chunk = Event::new("invocation");
/// chunk.llm_response.partial = true;
/// chunk.set_content(Content::new("model").with_text("Hello"));
/// deltas.push(&chunk);
/// let mut terminal = chunk.clone();
/// terminal.llm_response.partial = false;
/// terminal.llm_response.provider_metadata = Some(serde_json::json!({"content_complete": true}));
/// terminal.set_content(Content::new("model").with_text("Hello world"));
/// let output = deltas.push(&terminal);
/// assert_eq!(output.content().unwrap().parts[0].text(), Some(" world"));
/// ```
#[derive(Default)]
pub struct EventTextDeltas {
    pending: HashMap<(String, String), EmittedText>,
}

#[derive(Default)]
struct EmittedText {
    text: String,
    thinking: String,
}

impl EventTextDeltas {
    /// Preserves live chunks, tool parts, actions and usage. A complete snapshot
    /// whose text differs from the emitted prefix is returned in full because
    /// append-only output cannot retract earlier content.
    pub fn push<'a>(&mut self, event: &'a Event) -> Cow<'a, Event> {
        if event.tool_progress_stream().is_some() {
            return Cow::Borrowed(event);
        }
        let key = (event.invocation_id.clone(), event.id.clone());
        if event.llm_response.partial {
            if let Some(content) = event.content() {
                for part in &content.parts {
                    match part {
                        Part::Text { text } if !text.is_empty() => {
                            self.pending.entry(key.clone()).or_default().text.push_str(text);
                        }
                        Part::Thinking { thinking, .. } if !thinking.is_empty() => {
                            self.pending
                                .entry(key.clone())
                                .or_default()
                                .thinking
                                .push_str(thinking);
                        }
                        _ => {}
                    }
                }
            }
            return Cow::Borrowed(event);
        }
        let Some(emitted) = self.pending.remove(&key) else {
            return Cow::Borrowed(event);
        };
        let complete = event
            .llm_response
            .provider_metadata
            .as_ref()
            .and_then(|metadata| metadata.get("content_complete"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        let Some(content) = event.content().filter(|_| complete) else {
            return Cow::Borrowed(event);
        };
        let mut full = EmittedText::default();
        for part in &content.parts {
            match part {
                Part::Text { text } => full.text.push_str(text),
                Part::Thinking { thinking, .. } => full.thinking.push_str(thinking),
                _ => {}
            }
        }
        let mut text_prefix =
            if full.text.starts_with(&emitted.text) { emitted.text.len() } else { 0 };
        let mut thinking_prefix =
            if full.thinking.starts_with(&emitted.thinking) { emitted.thinking.len() } else { 0 };
        if text_prefix == 0 && thinking_prefix == 0 {
            return Cow::Borrowed(event);
        }
        let mut delta = event.clone();
        if let Some(content) = &mut delta.llm_response.content {
            for part in &mut content.parts {
                match part {
                    Part::Text { text } => strip_prefix(text, &mut text_prefix),
                    Part::Thinking { thinking, .. } => strip_prefix(thinking, &mut thinking_prefix),
                    _ => {}
                }
            }
        }
        Cow::Owned(delta)
    }
}

fn strip_prefix(value: &mut String, remaining: &mut usize) {
    let consumed = (*remaining).min(value.len());
    value.drain(..consumed);
    *remaining -= consumed;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Content;

    fn response(id: &str, text: &str, partial: bool) -> Event {
        let mut event = Event::with_id(id, "invocation");
        event.llm_response.content = Some(Content::new("model").with_text(text));
        event.llm_response.partial = partial;
        if !partial {
            event.llm_response.provider_metadata =
                Some(serde_json::json!({"content_complete":true}));
        }
        event
    }

    #[test]
    fn preserves_live_chunks_and_emits_only_the_terminal_suffix() {
        let mut deltas = EventTextDeltas::default();
        let first = response("first", "你好", true);
        assert!(matches!(deltas.push(&first), Cow::Borrowed(_)));
        let sibling = response("second", "Other", true);
        deltas.push(&sibling);
        let mut terminal = response("first", "你好，world", false);
        terminal.llm_response.content.as_mut().unwrap().parts.push(Part::FunctionCall {
            name: "search".into(),
            args: serde_json::json!({}),
            id: Some("call".into()),
            thought_signature: None,
        });
        let output = deltas.push(&terminal);
        assert_eq!(output.content().unwrap().parts[0], Part::Text { text: "，world".into() });
        assert_eq!(output.content().unwrap().parts[1], terminal.content().unwrap().parts[1]);
        assert_eq!(terminal.content().unwrap().parts[0], Part::Text { text: "你好，world".into() });
        let sibling = response("second", "Other result", false);
        assert_eq!(
            deltas.push(&sibling).content().unwrap().parts[0],
            Part::Text { text: " result".into() }
        );
        assert!(deltas.pending.is_empty());
    }

    #[test]
    fn retains_unmarked_deltas_and_corrected_snapshots() {
        for marked in [true, false] {
            let mut deltas = EventTextDeltas::default();
            deltas.push(&response("response", "Earlier", true));
            let mut terminal = response("response", "Correction", false);
            if !marked {
                terminal.llm_response.provider_metadata = None;
            }
            assert!(matches!(deltas.push(&terminal), Cow::Borrowed(_)));
            assert!(deltas.pending.is_empty());
        }
    }

    #[test]
    fn preserves_thinking_signatures_and_ignores_tool_progress() {
        let mut deltas = EventTextDeltas::default();
        let mut partial = response("response", "", true);
        partial.llm_response.content = Some(Content::new("model").with_thinking("思考"));
        deltas.push(&partial);
        let mut progress = Event::tool_progress("invocation", "agent", "call", "stdout", "log");
        progress.id = partial.id.clone();
        assert!(matches!(deltas.push(&progress), Cow::Borrowed(_)));
        let mut terminal = response("response", "Answer", false);
        terminal.llm_response.content.as_mut().unwrap().parts.insert(
            0,
            Part::Thinking { thinking: "思考完成".into(), signature: Some("signed".into()) },
        );
        assert_eq!(
            deltas.push(&terminal).content().unwrap().parts[0],
            Part::Thinking { thinking: "完成".into(), signature: Some("signed".into()) }
        );
        assert_eq!(
            terminal.content().unwrap().parts[0],
            Part::Thinking { thinking: "思考完成".into(), signature: Some("signed".into()) }
        );
        assert!(deltas.pending.is_empty());
    }
}
