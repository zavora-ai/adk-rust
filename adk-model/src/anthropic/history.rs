//! Preserve provider-native blocks that ADK text parts cannot represent.

use super::error::ConversionError;
use adk_anthropic::{ContentBlock, Message, TextCitation};
use adk_core::{CitationMetadata, CitationSource, Content, Part};
use serde_json::json;

const KIND: &str = "anthropic_message";

#[cfg(test)]
mod tests;

pub(super) fn preserve(message: &Message) -> Option<Part> {
    let needed = message.stop_reason == Some(adk_anthropic::StopReason::PauseTurn)
        || message.content.iter().any(|block| match block {
            ContentBlock::ServerToolUse(_) | ContentBlock::WebSearchToolResult(_) => true,
            ContentBlock::Text(text) => {
                text.citations.as_ref().is_some_and(|items| !items.is_empty())
            }
            _ => false,
        });
    needed.then(|| Part::ServerToolResponse {
        server_tool_response: json!({"type": KIND, "content": message.content}),
    })
}

pub(super) fn restore(content: &Content) -> Result<Option<Vec<ContentBlock>>, ConversionError> {
    let mut saved = content.parts.iter().filter_map(|part| match part {
        Part::ServerToolResponse { server_tool_response }
            if server_tool_response["type"] == KIND =>
        {
            Some(server_tool_response)
        }
        _ => None,
    });
    let Some(saved_message) = saved.next() else { return Ok(None) };
    if content.role != "model" && content.role != "assistant" || saved.next().is_some() {
        return Err(ConversionError::InvalidHistory("invalid native assistant history".into()));
    }
    let mut blocks: Vec<ContentBlock> = serde_json::from_value(saved_message["content"].clone())
        .map_err(|error| ConversionError::InvalidHistory(error.to_string()))?;
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
        return Err(ConversionError::InvalidHistory("native assistant text has changed".into()));
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
    Ok(Some(blocks))
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
