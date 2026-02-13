//! Type conversions between ADK and Claudius types.

use adk_core::{Content, FinishReason, LlmResponse, Part, UsageMetadata};
use claudius::{
    ContentBlock, Message, MessageCreateParams, MessageParam, MessageRole, Model, StopReason,
    SystemPrompt, TextBlock, ToolParam, ToolResultBlock, ToolResultBlockContent, ToolUnionParam,
    ToolUseBlock,
};
use serde_json::Value;
use std::collections::HashMap;

/// Convert ADK Content to Claudius MessageParam.
pub fn content_to_message(content: &Content) -> MessageParam {
    let role = match content.role.as_str() {
        "user" | "function" | "tool" => MessageRole::User,
        "model" | "assistant" => MessageRole::Assistant,
        _ => MessageRole::User,
    };

    let blocks: Vec<ContentBlock> = content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => {
                if text.is_empty() {
                    None
                } else {
                    Some(ContentBlock::Text(TextBlock::new(text.clone())))
                }
            }
            Part::FunctionCall { name, args, id } => Some(ContentBlock::ToolUse(ToolUseBlock {
                id: id.clone().unwrap_or_else(|| format!("call_{}", name)),
                name: name.clone(),
                input: args.clone(),
                cache_control: None,
            })),
            Part::FunctionResponse { function_response, id } => {
                Some(ContentBlock::ToolResult(ToolResultBlock {
                    tool_use_id: id.clone().unwrap_or_else(|| "unknown".to_string()),
                    content: Some(ToolResultBlockContent::String(
                        serde_json::to_string(&function_response.response).unwrap_or_default(),
                    )),
                    is_error: None,
                    cache_control: None,
                }))
            }
            Part::CodeExecutionResult { .. } => None,
            _ => None,
        })
        .collect();

    // If no blocks, add a placeholder for assistant messages
    let blocks = if blocks.is_empty() && role == MessageRole::Assistant {
        vec![ContentBlock::Text(TextBlock::new(" ".to_string()))]
    } else if blocks.is_empty() {
        vec![ContentBlock::Text(TextBlock::new("".to_string()))]
    } else {
        blocks
    };

    MessageParam::new_with_blocks(blocks, role)
}

/// Convert ADK tools to Claudius ToolUnionParam format.
pub fn convert_tools(tools: &HashMap<String, Value>) -> Vec<ToolUnionParam> {
    tools
        .iter()
        .map(|(name, decl)| {
            let description = decl.get("description").and_then(|d| d.as_str()).map(String::from);

            let input_schema = decl.get("parameters").cloned().unwrap_or(serde_json::json!({
                "type": "object",
                "properties": {}
            }));

            let mut tool_param = ToolParam::new(name.clone(), input_schema);
            if let Some(desc) = description {
                tool_param = tool_param.with_description(desc);
            }

            ToolUnionParam::CustomTool(tool_param)
        })
        .collect()
}

/// Convert Claudius Message to ADK LlmResponse.
pub fn from_anthropic_message(message: &Message) -> LlmResponse {
    let mut parts = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text(text_block) => {
                if !text_block.text.is_empty() {
                    parts.push(Part::Text { text: text_block.text.clone() });
                }
            }
            ContentBlock::ToolUse(tool_use) => {
                parts.push(Part::FunctionCall {
                    name: tool_use.name.clone(),
                    args: tool_use.input.clone(),
                    id: Some(tool_use.id.clone()),
                });
            }
            _ => {}
        }
    }

    let content =
        if parts.is_empty() { None } else { Some(Content { role: "model".to_string(), parts }) };

    let usage_metadata = Some(UsageMetadata {
        prompt_token_count: message.usage.input_tokens,
        candidates_token_count: message.usage.output_tokens,
        total_token_count: (message.usage.input_tokens + message.usage.output_tokens),
    });

    let finish_reason = message.stop_reason.as_ref().map(|sr| match sr {
        StopReason::EndTurn => FinishReason::Stop,
        StopReason::MaxTokens => FinishReason::MaxTokens,
        StopReason::StopSequence => FinishReason::Stop,
        StopReason::ToolUse => FinishReason::Stop,
        _ => FinishReason::Stop,
    });

    LlmResponse {
        content,
        usage_metadata,
        finish_reason,
        citation_metadata: None,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

/// Convert streaming text delta to ADK LlmResponse.
pub fn from_text_delta(text: &str) -> LlmResponse {
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

/// Create final response with tool calls.
pub fn create_tool_call_response(
    tool_calls: Vec<(String, String, Value)>, // (id, name, args)
    finish_reason: Option<FinishReason>,
) -> LlmResponse {
    let parts: Vec<Part> = tool_calls
        .into_iter()
        .map(|(id, name, args)| Part::FunctionCall { name, args, id: Some(id) })
        .collect();

    LlmResponse {
        content: Some(Content { role: "model".to_string(), parts }),
        usage_metadata: None,
        finish_reason,
        citation_metadata: None,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

/// Build MessageCreateParams from LlmRequest.
#[allow(clippy::too_many_arguments)]
pub fn build_message_params(
    model: &str,
    max_tokens: u32,
    messages: Vec<MessageParam>,
    tools: Vec<ToolUnionParam>,
    system_prompt: Option<String>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<i32>,
) -> MessageCreateParams {
    let mut params =
        MessageCreateParams::new(max_tokens, messages, Model::Custom(model.to_string()));

    if !tools.is_empty() {
        params.tools = Some(tools);
    }

    if let Some(sys) = system_prompt {
        params.system = Some(SystemPrompt::from_string(sys));
    }

    if let Some(temp) = temperature {
        params.temperature = Some(temp);
    }

    if let Some(p) = top_p {
        params.top_p = Some(p);
    }

    if let Some(k) = top_k {
        params.top_k = Some(k as u32);
    }

    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_to_message_user() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "Hello".to_string() }],
        };
        let msg = content_to_message(&content);
        assert!(matches!(msg.role, MessageRole::User));
    }

    #[test]
    fn test_content_to_message_assistant() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: "Hi there".to_string() }],
        };
        let msg = content_to_message(&content);
        assert!(matches!(msg.role, MessageRole::Assistant));
    }

    #[test]
    fn test_convert_tools() {
        let mut tools = HashMap::new();
        tools.insert(
            "get_weather".to_string(),
            serde_json::json!({
                "description": "Get weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    }
                }
            }),
        );

        let claude_tools = convert_tools(&tools);
        assert_eq!(claude_tools.len(), 1);
    }
}
