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
            // A block type newer than this client is replayed as the API sent it.
            ContentBlock::Unknown(_) => true,
            // The signed summary goes back exactly as the API returned it.
            ContentBlock::Compaction(_) => true,
            ContentBlock::Text(text) => {
                text.citations.as_ref().is_some_and(|items| !items.is_empty())
            }
            _ => false,
        });
    // Flagged replay-only: the copy is not a tool result, and it holds the turn's thinking.
    // A compaction's copy is also flagged as one, so the runner can tell it from a reply.
    needed.then(|| {
        let mut copy = json!({
            "type": KIND,
            adk_core::history::REPLAY_ONLY_KEY: true,
            "content": message.content,
        });
        if message.content.iter().any(|block| matches!(block, ContentBlock::Compaction(_))) {
            copy[adk_core::intra_compaction::NATIVE_COMPACTION_KEY] = json!(true);
        }
        Part::ServerToolResponse { server_tool_response: copy }
    })
}

/// Returns the provider's native blocks for a preserved assistant turn.
///
/// Returns `None`, so the caller converts the ADK parts instead, when the turn
/// carries no native copy or when the copy no longer matches the parts: a
/// guardrail or callback that rewrote the text must not fail later requests.
pub(super) fn restore(
    content: &Content,
    forms: super::toolsets::ToolsetForms,
) -> Option<Vec<ContentBlock>> {
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
    // A compaction's summary reaches the ADK parts as text under the summary heading.
    let original: String = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            ContentBlock::Compaction(compaction) => {
                Some(adk_core::intra_compaction::compaction_summary_text(&compaction.content))
            }
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
        let mut rebuilt = super::toolsets::tool_use_from_call(tool.id.clone(), name, args, forms);
        rebuilt.cache_control = tool.cache_control.take();
        *tool = rebuilt;
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
