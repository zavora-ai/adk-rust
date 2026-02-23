//! Type conversions between ADK core types and ollama-rs types.

use crate::attachment;
use adk_core::{Content, FinishReason, LlmResponse, Part, UsageMetadata};
use ollama_rs::generation::chat::{ChatMessage, ChatMessageResponse};

/// Convert ADK Content to Ollama ChatMessage.
pub fn content_to_chat_message(content: &Content) -> Option<ChatMessage> {
    // Extract text from parts
    let text: String = content
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.clone()),
            Part::Thinking { thinking, .. } => Some(thinking.clone()),
            Part::InlineData { mime_type, data } => {
                Some(attachment::inline_attachment_to_text(mime_type, data))
            }
            Part::FileData { mime_type, file_uri } => {
                Some(attachment::file_attachment_to_text(mime_type, file_uri))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    match content.role.as_str() {
        "user" => Some(ChatMessage::user(text)),
        "model" | "assistant" => Some(ChatMessage::assistant(text)),
        "system" => Some(ChatMessage::system(text)),
        "function" | "tool" => {
            // Handle function responses - combine all responses into one tool message
            let mut response_texts = Vec::new();
            for part in &content.parts {
                if let Part::FunctionResponse { function_response, .. } = part {
                    response_texts.push(format!(
                        "{}: {}",
                        function_response.name, function_response.response
                    ));
                }
            }
            if !response_texts.is_empty() {
                Some(ChatMessage::tool(response_texts.join("\n")))
            } else if !text.is_empty() {
                Some(ChatMessage::tool(text))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Convert Ollama ChatMessageResponse to ADK LlmResponse.
pub fn chat_response_to_llm_response(response: &ChatMessageResponse, partial: bool) -> LlmResponse {
    let mut parts = Vec::new();

    // Extract thinking content if present
    if let Some(thinking) = &response.message.thinking {
        if !thinking.is_empty() {
            parts.push(Part::Thinking { thinking: thinking.clone(), signature: None });
        }
    }

    // Add text content
    if !response.message.content.is_empty() {
        parts.push(Part::Text { text: response.message.content.clone() });
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

    let content =
        if parts.is_empty() { None } else { Some(Content { role: "model".to_string(), parts }) };

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
            role: "model".to_string(),
            parts: vec![Part::Text { text: text.to_string() }],
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
            role: "model".to_string(),
            parts: vec![Part::Thinking { thinking: thinking.to_string(), signature: None }],
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
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::InlineData {
                mime_type: "application/pdf".to_string(),
                data: b"%PDF".to_vec(),
            }],
        };
        let message = content_to_chat_message(&content).expect("message should be created");
        assert!(message.content.contains("application/pdf"));
        assert!(message.content.contains("encoding=\"base64\""));
    }

    #[test]
    fn content_to_chat_message_keeps_file_attachment_payload() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::FileData {
                mime_type: "text/csv".to_string(),
                file_uri: "https://example.com/data.csv".to_string(),
            }],
        };
        let message = content_to_chat_message(&content).expect("message should be created");
        assert!(message.content.contains("text/csv"));
        assert!(message.content.contains("https://example.com/data.csv"));
    }
}
