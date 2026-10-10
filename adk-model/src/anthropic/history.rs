//! Preserve provider-native blocks that ADK text parts cannot represent.

use adk_anthropic::{ContentBlock, Message, TextCitation};
use adk_core::{CitationMetadata, CitationSource, Content, Part};
use serde_json::json;

const KIND: &str = "anthropic_message";

#[cfg(test)]
mod tests;

pub(super) fn preserve(message: &Message) -> Option<Part> {
    let needed = message.stop_reason == Some(adk_anthropic::StopReason::PauseTurn)
        || message.content.iter().any(|block| match block {
            // ADK parts have no `redacted_thinking`, and the API rejects a tool-use
            // turn replayed without it.
            ContentBlock::ServerToolUse(_)
            | ContentBlock::WebSearchToolResult(_)
            | ContentBlock::RedactedThinking(_) => true,
            ContentBlock::Text(text) => {
                text.citations.as_ref().is_some_and(|items| !items.is_empty())
            }
            _ => false,
        });
    needed.then(|| Part::ServerToolResponse {
        server_tool_response: json!({"type": KIND, "content": message.content}),
    })
}

/// Returns the provider's native blocks for a preserved assistant turn.
///
/// Returns `None`, so the caller converts the ADK parts instead, when the turn
/// carries no native copy or when the copy no longer matches the parts: a
/// guardrail or callback that rewrote the text must not fail later requests.
pub(super) fn restore(content: &Content) -> Option<Vec<ContentBlock>> {
    let mut saved = content.parts.iter().filter_map(|part| match part {
        Part::ServerToolResponse { server_tool_response }
            if server_tool_response["type"] == KIND =>
        {
            Some(server_tool_response)
        }
        _ => None,
    });
    let saved_message = saved.next()?;
    if content.role != "model" && content.role != "assistant" || saved.next().is_some() {
        tracing::debug!("native anthropic history is ambiguous; converting the adk parts");
        return None;
    }
    let mut blocks: Vec<ContentBlock> = match serde_json::from_value(
        saved_message["content"].clone(),
    ) {
        Ok(blocks) => blocks,
        Err(error) => {
            tracing::debug!(error = %error, "native anthropic history is malformed; converting the adk parts");
            return None;
        }
    };
    let original: String = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    let current: String = content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if current != original {
        tracing::debug!("assistant text changed after the response; converting the adk parts");
        return None;
    }
    // The runner removes interrupted calls from model history. Never resurrect
    // those calls from the provider copy or override a callback's changed input.
    blocks.retain_mut(|block| {
        let ContentBlock::ToolUse(tool) = block else { return true };
        let current = content.parts.iter().find_map(|part| match part {
            Part::FunctionCall { name, args, id: Some(id), .. } if id == &tool.id => {
                Some((name, args))
            }
            _ => None,
        });
        let Some((name, args)) = current else { return false };
        tool.name.clone_from(name);
        tool.input.clone_from(args);
        true
    });
    Some(blocks)
}

pub(super) fn citations(message: &Message) -> Option<CitationMetadata> {
    let mut offset = 0usize;
    let mut sources = Vec::new();
    for block in &message.content {
        let ContentBlock::Text(text) = block else { continue };
        let end = offset + text.text.chars().count();
        for citation in text.citations.iter().flatten() {
            let TextCitation::WebSearchResultLocation(source) = citation else { continue };
            sources.push(CitationSource {
                uri: Some(source.url.clone()),
                title: source.title.clone(),
                start_index: i32::try_from(offset).ok(),
                end_index: i32::try_from(end).ok(),
                license: None,
                publication_date: None,
            });
        }
        offset = end;
    }
    (!sources.is_empty()).then_some(CitationMetadata { citation_sources: sources })
}
