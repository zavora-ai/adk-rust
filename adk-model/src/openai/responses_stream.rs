//! Responses stream output recovery, separate from transport and request policy.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use adk_core::{Content, LlmResponse, Part};
use async_openai::types::responses::{
    MessagePhase, OutputContent, OutputItem, OutputMessageContent, ReasoningItemContent,
    ResponseStreamEvent, SummaryPart,
};

use super::responses_client::wire;
use super::responses_convert;
#[cfg(test)]
use async_openai::types::responses::Response;

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum TextKind {
    Answer,
    Refusal,
    Summary,
    Reasoning,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct TextKey {
    output_index: u32,
    part_index: u32,
    item_id: String,
    kind: TextKind,
}

impl TextKey {
    fn new(item_id: &str, output_index: u32, part_index: u32, kind: TextKind) -> Self {
        Self { item_id: item_id.into(), output_index, part_index, kind }
    }
}

#[derive(Default)]
pub(super) struct StreamState {
    emitted: BTreeMap<TextKey, String>,
    finalized: BTreeMap<TextKey, String>,
    finalized_items: BTreeMap<u32, OutputItem>,
    pending_calls: BTreeMap<u32, Option<String>>,
    commentary: HashSet<(u32, String)>,
    native_tools: bool,
    corrected: bool,
}

impl StreamState {
    pub(super) fn new(native_tools: bool) -> Self {
        Self { native_tools, ..Default::default() }
    }

    /// Records a non-terminal event and returns the text it adds, if any.
    pub(super) fn partial(&mut self, event: ResponseStreamEvent) -> Option<LlmResponse> {
        use ResponseStreamEvent::*;

        let mut parts = Vec::new();
        match event {
            ResponseOutputItemAdded(event) => {
                match &event.item {
                    OutputItem::FunctionCall(call) => {
                        self.pending_calls.insert(event.output_index, Some(call.call_id.clone()));
                    }
                    OutputItem::CustomToolCall(call) => {
                        self.pending_calls.insert(event.output_index, Some(call.call_id.clone()));
                    }
                    _ => {}
                }
                self.record_phase(event.output_index, &event.item);
                return None;
            }
            ResponseFunctionCallArgumentsDelta(event) => {
                self.pending_calls.entry(event.output_index).or_default();
                return None;
            }
            ResponseCustomToolCallInputDelta(event) => {
                self.pending_calls.entry(event.output_index).or_default();
                return None;
            }
            ResponseOutputTextDelta(event) => {
                let key = TextKey::new(
                    &event.item_id,
                    event.output_index,
                    event.content_index,
                    TextKind::Answer,
                );
                self.delta(key, &event.delta, &mut parts);
            }
            ResponseOutputTextDone(event) => {
                let key = TextKey::new(
                    &event.item_id,
                    event.output_index,
                    event.content_index,
                    TextKind::Answer,
                );
                self.done(key, &event.text, &mut parts);
            }
            ResponseRefusalDelta(event) => {
                let key = TextKey::new(
                    &event.item_id,
                    event.output_index,
                    event.content_index,
                    TextKind::Refusal,
                );
                self.delta(key, &event.delta, &mut parts);
            }
            ResponseRefusalDone(event) => {
                let key = TextKey::new(
                    &event.item_id,
                    event.output_index,
                    event.content_index,
                    TextKind::Refusal,
                );
                self.done(key, &event.refusal, &mut parts);
            }
            ResponseContentPartDone(event) => match event.part {
                OutputContent::OutputText(text) => {
                    let key = TextKey::new(
                        &event.item_id,
                        event.output_index,
                        event.content_index,
                        TextKind::Answer,
                    );
                    self.done(key, &text.text, &mut parts);
                }
                OutputContent::Refusal(refusal) => {
                    let key = TextKey::new(
                        &event.item_id,
                        event.output_index,
                        event.content_index,
                        TextKind::Refusal,
                    );
                    self.done(key, &refusal.refusal, &mut parts);
                }
                OutputContent::ReasoningText(_) => return None,
            },
            ResponseReasoningSummaryTextDelta(event) => {
                let key = TextKey::new(
                    &event.item_id,
                    event.output_index,
                    event.summary_index,
                    TextKind::Summary,
                );
                self.delta(key, &event.delta, &mut parts);
            }
            ResponseReasoningSummaryTextDone(event) => {
                let key = TextKey::new(
                    &event.item_id,
                    event.output_index,
                    event.summary_index,
                    TextKind::Summary,
                );
                self.done(key, &event.text, &mut parts);
            }
            ResponseReasoningSummaryPartDone(event) => {
                let SummaryPart::SummaryText(text) = event.part;
                let key = TextKey::new(
                    &event.item_id,
                    event.output_index,
                    event.summary_index,
                    TextKind::Summary,
                );
                self.done(key, &text.text, &mut parts);
            }
            ResponseReasoningTextDelta(event) => {
                let key = TextKey::new(
                    &event.item_id,
                    event.output_index,
                    event.content_index,
                    TextKind::Reasoning,
                );
                self.delta(key, &event.delta, &mut parts);
            }
            ResponseReasoningTextDone(event) => {
                let key = TextKey::new(
                    &event.item_id,
                    event.output_index,
                    event.content_index,
                    TextKind::Reasoning,
                );
                self.done(key, &event.text, &mut parts);
            }
            ResponseOutputItemDone(event) => {
                // A complete done call may replace its whole identity, never mix
                // earlier fragments into that authoritative payload.
                if let Some(pending) = self.pending_calls.get_mut(&event.output_index)
                    && let Some(call_id) = wire::finalized_call(&event.item)
                {
                    *pending = Some(call_id.to_owned());
                }
                self.record_phase(event.output_index, &event.item);
                for (key, text) in Self::item_texts(event.output_index, &event.item) {
                    self.done(key, text, &mut parts);
                }
                self.finalized_items.insert(event.output_index, event.item);
            }
            _ => return None,
        }
        (!parts.is_empty()).then_some(LlmResponse {
            content: Some(Content { role: "model".into(), parts }),
            partial: true,
            ..Default::default()
        })
    }

    #[cfg(test)]
    pub(super) fn complete(&mut self, response: Response) -> LlmResponse {
        self.finish(&wire::Response::from(&response)).unwrap()
    }

    /// Converts the terminal response, keeping only what the stream has not delivered.
    pub(super) fn finish(
        &mut self,
        response: &wire::Response,
    ) -> Result<LlmResponse, adk_core::AdkError> {
        if response.output_missing && self.finalized_items.is_empty() {
            return Err(wire::missing_output());
        }
        let mut response = response.clone();
        let recovered = response.output.is_empty() && !self.finalized_items.is_empty();
        if recovered {
            // Some endpoints omit terminal output after emitting authoritative done items.
            // Recover the whole items, including calls and reasoning.
            response.output = std::mem::take(&mut self.finalized_items).into_values().collect();
        }
        wire::validate(&response)?;
        for (index, expected) in &self.pending_calls {
            fn call_id(item: &OutputItem) -> Option<&str> {
                match item {
                    OutputItem::FunctionCall(call) => Some(call.call_id.as_str()),
                    OutputItem::CustomToolCall(call) => Some(call.call_id.as_str()),
                    _ => None,
                }
            }
            let identity = if let Some(expected) = expected {
                response.output.iter().filter_map(call_id).find(|identity| *identity == expected)
            } else {
                response.output.get(*index as usize).and_then(call_id)
            };
            if identity.is_none()
                || expected.as_deref().zip(identity).is_some_and(|(left, right)| left != right)
            {
                return Err(wire::unfinished());
            }
        }
        let mut full = responses_convert::from_projection(&response);
        let mut final_keys = BTreeSet::new();
        for (output_index, item) in response.output.iter_mut().enumerate() {
            final_keys.extend(Self::item_texts(output_index as u32, item).map(|(key, _)| key));
            self.record_phase(output_index as u32, item);
            self.trim_item(output_index as u32, item);
        }
        let omitted = response.output.is_empty();
        let removed = !omitted && self.emitted.keys().any(|key| !final_keys.contains(key));
        let unfinished = self.emitted.keys().any(|key| !self.finalized.contains_key(key));
        if self.corrected
            || recovered
            || removed
            || unfinished
            || self.native_tools
            || !self.commentary.is_empty()
        {
            // ADK replaces accumulated fragments with this authoritative snapshot.
            if omitted {
                full.content = Some(Content { role: "model".into(), parts: self.snapshot_parts() });
            } else if (self.corrected || removed || unfinished) && full.content.is_none() {
                full.content = Some(Content { role: "model".into(), parts: Vec::new() });
            }
            full.provider_metadata.get_or_insert_with(|| serde_json::json!({}))["content_complete"] =
                serde_json::json!(true);
        } else {
            // Keep metadata/citation offsets from the full response, not its suffixes.
            full.content = responses_convert::output_parts(&response.output);
        }
        Ok(full)
    }

    fn record_phase(&mut self, output_index: u32, item: &OutputItem) {
        if let OutputItem::Message(message) = item
            && message.phase == Some(MessagePhase::Commentary)
        {
            self.commentary.insert((output_index, message.id.clone()));
        }
    }

    fn delta(&mut self, key: TextKey, text: &str, parts: &mut Vec<Part>) {
        if self.emitted.iter().any(|(previous, text)| previous > &key && !text.is_empty()) {
            self.corrected = true;
        }
        self.emitted.entry(key.clone()).or_default().push_str(text);
        if let Some(part) = self.part(&key, text, "in_progress") {
            parts.push(part);
        }
    }

    fn done(&mut self, key: TextKey, text: &str, parts: &mut Vec<Part>) -> String {
        self.finalized.insert(key.clone(), text.into());
        let later_text =
            self.emitted.iter().any(|(previous, text)| previous > &key && !text.is_empty());
        let emitted = self.emitted.entry(key.clone()).or_default();
        let Some(suffix) = text.strip_prefix(emitted.as_str()) else {
            // A final payload can correct earlier text, but it is not another delta.
            self.corrected = true;
            return String::new();
        };
        let suffix = suffix.to_owned();
        if later_text && !suffix.is_empty() {
            // Inserting an earlier part requires replacement, not an appended suffix.
            self.corrected = true;
            return String::new();
        }
        emitted.push_str(&suffix);
        if let Some(part) = self.part(&key, &suffix, "in_progress") {
            parts.push(part);
        }
        suffix
    }

    fn snapshot_parts(&self) -> Vec<Part> {
        // Without terminal output, a done payload supersedes its streamed deltas
        // and an unfinished part keeps the text already delivered to the caller.
        let keys: BTreeSet<&TextKey> = self.emitted.keys().chain(self.finalized.keys()).collect();
        let mut parts: Vec<Result<Part, (u32, &str)>> = Vec::new();
        let mut messages: BTreeMap<(u32, &str), Vec<serde_json::Value>> = BTreeMap::new();
        for key in keys {
            let Some(text) = self.finalized.get(key).or_else(|| self.emitted.get(key)) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            if self.is_commentary(key) {
                let identity = (key.output_index, key.item_id.as_str());
                let contents = messages.entry(identity).or_default();
                if contents.is_empty() {
                    parts.push(Err(identity));
                }
                contents.push(commentary_content(key.kind, text));
            } else if let Some(part) = self.part(key, text, "completed") {
                parts.push(Ok(part));
            }
        }
        parts
            .into_iter()
            .filter_map(|part| match part {
                Ok(part) => Some(part),
                Err(identity) => messages
                    .remove(&identity)
                    .map(|contents| commentary_message(identity.1, "completed", contents)),
            })
            .collect()
    }

    fn item_texts(output_index: u32, item: &OutputItem) -> impl Iterator<Item = (TextKey, &str)> {
        let (message, reasoning) = match item {
            OutputItem::Message(message) => (Some(message), None),
            OutputItem::Reasoning(reasoning) => (None, Some(reasoning)),
            _ => (None, None),
        };
        let message_texts = message.into_iter().flat_map(move |message| {
            message.content.iter().enumerate().map(move |(index, part)| {
                let (kind, text) = match part {
                    OutputMessageContent::OutputText(text) => (TextKind::Answer, &text.text),
                    OutputMessageContent::Refusal(refusal) => (TextKind::Refusal, &refusal.refusal),
                };
                (TextKey::new(&message.id, output_index, index as u32, kind), text.as_str())
            })
        });
        let summary_texts = reasoning.into_iter().flat_map(move |reasoning| {
            let item_id = reasoning.id.as_deref().unwrap_or_default();
            reasoning.summary.iter().enumerate().map(move |(index, summary)| {
                let SummaryPart::SummaryText(text) = summary;
                (
                    TextKey::new(item_id, output_index, index as u32, TextKind::Summary),
                    text.text.as_str(),
                )
            })
        });
        let raw_texts = reasoning
            .into_iter()
            .filter(|reasoning| reasoning.summary.is_empty())
            .flat_map(move |reasoning| {
                let item_id = reasoning.id.as_deref().unwrap_or_default();
                reasoning.content.iter().flatten().enumerate().map(move |(index, content)| {
                    let ReasoningItemContent::ReasoningText(text) = content;
                    (
                        TextKey::new(item_id, output_index, index as u32, TextKind::Reasoning),
                        text.text.as_str(),
                    )
                })
            });
        message_texts.chain(summary_texts).chain(raw_texts)
    }

    /// Replaces each text in `item` with the suffix the stream has not delivered.
    fn trim_item(&mut self, output_index: u32, item: &mut OutputItem) {
        let mut ignored = Vec::new();
        match item {
            OutputItem::Message(message) => {
                for (part_index, content) in message.content.iter_mut().enumerate() {
                    let (kind, text) = match content {
                        OutputMessageContent::OutputText(text) => {
                            (TextKind::Answer, &mut text.text)
                        }
                        OutputMessageContent::Refusal(refusal) => {
                            (TextKind::Refusal, &mut refusal.refusal)
                        }
                    };
                    let key = TextKey::new(&message.id, output_index, part_index as u32, kind);
                    *text = self.done(key, text, &mut ignored);
                }
            }
            OutputItem::Reasoning(reasoning) => {
                let item_id = reasoning.id.as_deref().unwrap_or_default();
                for (part_index, summary) in reasoning.summary.iter_mut().enumerate() {
                    let SummaryPart::SummaryText(text) = summary;
                    let key =
                        TextKey::new(item_id, output_index, part_index as u32, TextKind::Summary);
                    text.text = self.done(key, &text.text, &mut ignored);
                }
                if reasoning.summary.is_empty() {
                    for (part_index, content) in reasoning.content.iter_mut().flatten().enumerate()
                    {
                        let text = match content {
                            ReasoningItemContent::ReasoningText(text) => &mut text.text,
                        };
                        let key = TextKey::new(
                            item_id,
                            output_index,
                            part_index as u32,
                            TextKind::Reasoning,
                        );
                        *text = self.done(key, text, &mut ignored);
                    }
                }
            }
            _ => {}
        }
    }

    fn is_commentary(&self, key: &TextKey) -> bool {
        self.commentary.contains(&(key.output_index, key.item_id.clone()))
    }

    fn part(&self, key: &TextKey, text: &str, message_status: &str) -> Option<Part> {
        if text.is_empty() {
            return None;
        }
        if self.is_commentary(key) {
            let content = commentary_content(key.kind, text);
            Some(commentary_message(&key.item_id, message_status, vec![content]))
        } else if matches!(key.kind, TextKind::Summary | TextKind::Reasoning) {
            Some(Part::Thinking { thinking: text.into(), signature: None })
        } else {
            Some(Part::Text { text: text.into() })
        }
    }
}

fn commentary_content(kind: TextKind, text: &str) -> serde_json::Value {
    if kind == TextKind::Refusal {
        serde_json::json!({"type": "refusal", "refusal": text})
    } else {
        serde_json::json!({"type": "output_text", "text": text, "annotations": []})
    }
}

fn commentary_message(
    item_id: &str,
    message_status: &str,
    content: Vec<serde_json::Value>,
) -> Part {
    Part::ServerToolCall {
        server_tool_call: serde_json::json!({
            "type": "message", "role": "assistant", "id": item_id,
            "phase": "commentary", "status": message_status, "content": content
        }),
    }
}
