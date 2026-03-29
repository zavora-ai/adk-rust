//! # adk-anthropic
//!
//! Dedicated Anthropic API client for ADK-Rust.
//!
//! This crate provides the HTTP client, type system, SSE streaming, error handling,
//! and backoff logic for interacting with the Anthropic API. Agent framework,
//! CLI tools, chat session management, and observability are handled by other
//! ADK crates.

mod accumulating_stream;
mod backoff;
mod cache_control;
mod client;
mod client_logger;
mod error;
mod json_schema;
mod observability;
pub mod pricing;
mod sse;
mod tool_search;
mod types;

pub use accumulating_stream::AccumulatingStream;
pub use backoff::ExponentialBackoff;
pub use cache_control::{
    MAX_CACHE_BREAKPOINTS, apply_cache_control_to_messages, count_system_cache_controls,
    prune_cache_controls_in_messages,
};
pub use client::{Anthropic, LoggingStream};
pub use client_logger::ClientLogger;
pub use error::{Error, Result};
pub use json_schema::JsonSchema;
pub use tool_search::ToolSearchConfig;
pub use types::*;

/// Pushes a message to the messages vector, or merges it with the last message if they have the same role.
pub fn push_or_merge_message(messages: &mut Vec<MessageParam>, to_push: MessageParam) {
    if let Some(last) = messages.last_mut() {
        if last.role != to_push.role {
            messages.push(to_push);
        } else {
            merge_message_content(&mut last.content, to_push.content);
        }
    } else {
        messages.push(to_push);
    }
}

/// Merges new message content into existing message content.
pub fn merge_message_content(existing: &mut MessageParamContent, new: MessageParamContent) {
    match (&mut *existing, new) {
        (MessageParamContent::Array(existing_blocks), MessageParamContent::Array(new_blocks)) => {
            existing_blocks.extend(new_blocks);
        }
        (MessageParamContent::Array(existing_blocks), MessageParamContent::String(new_string)) => {
            existing_blocks.push(ContentBlock::Text(crate::TextBlock::new(new_string)));
        }
        (MessageParamContent::String(existing_string), MessageParamContent::Array(new_blocks)) => {
            let mut combined =
                vec![ContentBlock::Text(crate::TextBlock::new(existing_string.clone()))];
            combined.extend(new_blocks);
            *existing = MessageParamContent::Array(combined);
        }
        (MessageParamContent::String(existing_string), MessageParamContent::String(new_string)) => {
            existing_string.push_str(&new_string);
        }
    }
}
