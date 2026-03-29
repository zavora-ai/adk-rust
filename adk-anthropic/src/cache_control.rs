//! Shared cache_control utilities for request construction.

use crate::types::{
    CacheControlEphemeral, ContentBlock, MessageParam, MessageParamContent, MessageRole,
    SystemPrompt, TextBlock,
};

/// Maximum number of cache control breakpoints allowed by the API.
pub const MAX_CACHE_BREAKPOINTS: usize = 4;

/// Count cache_control markers present in the system prompt.
pub fn count_system_cache_controls(system: &Option<SystemPrompt>) -> usize {
    match system {
        Some(SystemPrompt::Blocks(blocks)) => {
            blocks.iter().filter(|block| block.block.cache_control.is_some()).count()
        }
        _ => 0,
    }
}

/// Remove cache_control markers so the latest N remain in the request.
pub fn prune_cache_controls_in_messages(messages: &mut [MessageParam], keep_latest: usize) {
    if keep_latest == 0 {
        for message in messages.iter_mut() {
            clear_cache_control_from_message(message);
        }
        return;
    }

    let mut cached_positions = Vec::new();
    for (msg_idx, message) in messages.iter().enumerate() {
        if let MessageParamContent::Array(blocks) = &message.content {
            for (block_idx, block) in blocks.iter().enumerate() {
                if block_has_cache_control(block) {
                    cached_positions.push((msg_idx, block_idx));
                }
            }
        }
    }

    if cached_positions.len() <= keep_latest {
        return;
    }

    let drop_count = cached_positions.len() - keep_latest;
    for (msg_idx, block_idx) in cached_positions.into_iter().take(drop_count) {
        if let MessageParamContent::Array(blocks) = &mut messages[msg_idx].content
            && let Some(block) = blocks.get_mut(block_idx)
        {
            clear_cache_control_on_block(block);
        }
    }
}

/// Applies cache_control markers to the last content block of up to N user messages.
///
/// The system prompt uses one cache breakpoint, so we apply markers to the last
/// (MAX_CACHE_BREAKPOINTS - 1) user messages. This function first clears any existing
/// cache_control markers to avoid exceeding the API limit of 4 breakpoints.
pub fn apply_cache_control_to_messages(messages: &mut [MessageParam]) {
    // First, clear all existing cache_control markers from all messages.
    for msg in messages.iter_mut() {
        clear_cache_control_from_message(msg);
    }

    // Find indices of user messages (in reverse order).
    let user_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, msg)| msg.role == MessageRole::User)
        .map(|(idx, _)| idx)
        .rev()
        .take(MAX_CACHE_BREAKPOINTS - 1) // Reserve one breakpoint for system prompt.
        .collect();

    for idx in user_indices {
        apply_cache_control_to_message(&mut messages[idx]);
    }
}

/// Clears cache_control from all content blocks in a message.
fn clear_cache_control_from_message(message: &mut MessageParam) {
    if let MessageParamContent::Array(blocks) = &mut message.content {
        for block in blocks.iter_mut() {
            clear_cache_control_on_block(block);
        }
    }
}

/// Clears cache_control on a content block.
fn clear_cache_control_on_block(block: &mut ContentBlock) {
    match block {
        ContentBlock::Text(text_block) => {
            text_block.cache_control = None;
        }
        ContentBlock::ToolResult(tool_result) => {
            tool_result.cache_control = None;
        }
        ContentBlock::ToolUse(tool_use) => {
            tool_use.cache_control = None;
        }
        ContentBlock::Image(image_block) => {
            image_block.cache_control = None;
        }
        ContentBlock::Document(document_block) => {
            document_block.cache_control = None;
        }
        ContentBlock::ServerToolUse(server_tool_use) => {
            server_tool_use.cache_control = None;
        }
        ContentBlock::WebSearchToolResult(web_search_result) => {
            web_search_result.cache_control = None;
        }
        // Thinking blocks don't support cache_control.
        ContentBlock::Thinking(_)
        | ContentBlock::RedactedThinking(_)
        | ContentBlock::CodeExecutionResult(_)
        | ContentBlock::ProgrammaticToolUse(_) => {}
    }
}

/// Applies cache_control to the last content block of a single message.
pub(crate) fn apply_cache_control_to_message(message: &mut MessageParam) {
    match &mut message.content {
        MessageParamContent::String(text) => {
            // Convert string to a single text block with cache_control.
            let block = ContentBlock::Text(
                TextBlock::new(text.clone()).with_cache_control(CacheControlEphemeral::new()),
            );
            message.content = MessageParamContent::Array(vec![block]);
        }
        MessageParamContent::Array(blocks) => {
            // Find the last cacheable block and add cache_control.
            if let Some(last_block) = blocks.last_mut() {
                set_cache_control_on_block(last_block);
            }
        }
    }
}

/// Sets cache_control on a content block if it supports caching.
fn set_cache_control_on_block(block: &mut ContentBlock) {
    match block {
        ContentBlock::Text(text_block) => {
            text_block.cache_control = Some(CacheControlEphemeral::new());
        }
        ContentBlock::ToolResult(tool_result) => {
            tool_result.cache_control = Some(CacheControlEphemeral::new());
        }
        ContentBlock::ToolUse(tool_use) => {
            tool_use.cache_control = Some(CacheControlEphemeral::new());
        }
        // Other block types don't support cache_control in user messages.
        ContentBlock::Image(_)
        | ContentBlock::Document(_)
        | ContentBlock::ServerToolUse(_)
        | ContentBlock::WebSearchToolResult(_)
        | ContentBlock::Thinking(_)
        | ContentBlock::RedactedThinking(_)
        | ContentBlock::CodeExecutionResult(_)
        | ContentBlock::ProgrammaticToolUse(_) => {}
    }
}

fn block_has_cache_control(block: &ContentBlock) -> bool {
    match block {
        ContentBlock::Text(text_block) => text_block.cache_control.is_some(),
        ContentBlock::ToolResult(tool_result) => tool_result.cache_control.is_some(),
        ContentBlock::ToolUse(tool_use) => tool_use.cache_control.is_some(),
        ContentBlock::Image(image_block) => image_block.cache_control.is_some(),
        ContentBlock::Document(document_block) => document_block.cache_control.is_some(),
        ContentBlock::ServerToolUse(server_tool_use) => server_tool_use.cache_control.is_some(),
        ContentBlock::WebSearchToolResult(web_search_result) => {
            web_search_result.cache_control.is_some()
        }
        ContentBlock::Thinking(_) | ContentBlock::RedactedThinking(_) => false,
        ContentBlock::CodeExecutionResult(_) | ContentBlock::ProgrammaticToolUse(_) => false,
    }
}
