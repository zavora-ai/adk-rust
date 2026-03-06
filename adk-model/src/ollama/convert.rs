//! Type conversions between ADK core types and ollama-rs types.

use adk_core::{Content, FinishReason, LlmResponse, Part, Role, UsageMetadata};
use bytes::Bytes;
use ollama_rs::generation::chat::{ChatMessage, ChatMessageResponse};

/// Convert ADK Content to Ollama ChatMessage.
pub fn content_to_chat_message(content: &Content) -> Option<ChatMessage> {
    let text = content.collect_text();

    match content.role {
        Role::User => Some(ChatMessage::user(text)),
        Role::Model | Role::System => Some(ChatMessage::assistant(text)),
        Role::Custom(ref s) if s == "assistant" => Some(ChatMessage::assistant(text)),
        Role::Custom(ref s) if s == "system" => Some(ChatMessage::system(text)),
        Role::Custom(ref s) if s == "user" => Some(ChatMessage::user(text)),
        _ => {
            // Check for tool/function role equivalents
            if content.role.is_tool() {
                let mut response_texts = Vec::new();
                for part in &content.parts {
                    if let Part::FunctionResponse { name, response, .. } = part {
                        response_texts.push(format!("{}: {}", name, response));
                    }
                }
                if !response_texts.is_empty() {
                    return Some(ChatMessage::tool(response_texts.join("\n")));
                } else if !text.is_empty() {
                    return Some(ChatMessage::tool(text));
                }
            }
            // Fallback for unknown roles
            if content.role.is_system() {
                Some(ChatMessage::system(text))
            } else if content.role.is_model() {
                Some(ChatMessage::assistant(text))
            } else {
                Some(ChatMessage::user(text))
            }
        }
    }
}

/// Convert Ollama ChatMessageResponse to ADK LlmResponse.
pub fn chat_response_to_llm_response(response: &ChatMessageResponse, partial: bool) -> LlmResponse {
    let mut parts = Vec::new();

    // Extract thinking content if present
    if let Some(thinking) = &response.message.thinking {
        if !thinking.is_empty() {
            parts.push(Part::thinking(thinking.clone()));
        }
    }

    // Add text content
    if !response.message.content.is_empty() {
        parts.push(Part::text(response.message.content.clone()));
    }

    // Handle tool calls if present
    for tool_call in &response.message.tool_calls {
        parts.push(Part::FunctionCall {
            name: tool_call.function.name.clone(),
            args: tool_call.function.arguments.clone(),
            id: None, // Ollama doesn't provide tool call IDs
            thought_signature: None,
        });
    }

    let content = if parts.is_empty() {
        None
    } else {
        Some(Content { role: adk_core::types::Role::Model, parts })
    };

    // Determine finish reason
    let finish_reason = if response.done { Some(FinishReason::Stop) } else { None };

    // Extract usage metadata from final_data if available
    let usage_metadata = response.final_data.as_ref().map(|data| UsageMetadata {
        prompt_token_count: data.prompt_eval_count as i32,
        candidates_token_count: data.eval_count as i32,
        total_token_count: (data.prompt_eval_count + data.eval_count) as i32,
        ..Default::default()
    });

    LlmResponse {
        content,
        usage_metadata,
        finish_reason,
        citation_metadata: None,
        partial,
        turn_complete: response.done,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

/// Create a text delta response for streaming.
pub fn text_delta_response(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: adk_core::types::Role::Model,
            parts: vec![Part::text(text)],
        }),
        usage_metadata: None,
        finish_reason: None,
        citation_metadata: None,
        partial: true,
        turn_complete: false,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

/// Create a thinking delta response for streaming.
pub fn thinking_delta_response(thinking: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: adk_core::types::Role::Model,
            parts: vec![Part::thinking(thinking)],
        }),
        usage_metadata: None,
        finish_reason: None,
        citation_metadata: None,
        partial: true,
        turn_complete: false,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_to_chat_message_keeps_inline_attachment_payload() {
        let content = Content::user()
            .with_inline_data("application/pdf", Bytes::from_static(b"%PDF"))
            .unwrap();
        let message = content_to_chat_message(&content).expect("message should be created");
        assert!(message.content.contains("[Inline Data: application/pdf]"));
    }

    #[test]
    fn content_to_chat_message_keeps_file_attachment_payload() {
        let content = Content::user().with_file_uri("text/csv", "https://example.com/data.csv");
        let message = content_to_chat_message(&content).expect("message should be created");
        assert!(message.content.contains("[File: https://example.com/data.csv]"));
    }
}
