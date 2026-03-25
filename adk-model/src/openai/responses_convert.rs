//! Bidirectional type conversions between ADK types and OpenAI Responses API types.
//!
//! This module converts `adk_core` request/response types to and from
//! `async-openai` Responses API types (`InputItem`, `OutputItem`, `CreateResponse`, etc.).

use crate::attachment;
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, FinishReason, LlmRequest, LlmResponse, Part,
    UsageMetadata,
};
use async_openai::types::responses::{
    CreateResponse, CreateResponseArgs, EasyInputContent, EasyInputMessage, FunctionCallOutput,
    FunctionCallOutputItemParam, FunctionTool, FunctionToolCall, InputContent, InputImageContent,
    InputItem, InputParam, Item, OutputItem, OutputMessageContent, Reasoning,
    ReasoningEffort as OaiReasoningEffort, ReasoningSummary as OaiReasoningSummary, Response,
    ResponseUsage, Role, Status, SummaryPart, Tool,
};
use std::collections::HashMap;

use super::config::{ReasoningEffort, ReasoningSummary};

/// Convert a list of ADK `Content` items to Responses API `InputItem` list.
pub fn contents_to_input_items(contents: &[Content]) -> Vec<InputItem> {
    contents.iter().flat_map(content_to_input_items).collect()
}

/// Convert a single ADK `Content` to one or more `InputItem`s.
fn content_to_input_items(content: &Content) -> Vec<InputItem> {
    let role = content.role.as_str();
    let mut items = Vec::new();

    for part in &content.parts {
        match part {
            Part::Text { text } => match role {
                "user" => {
                    items.push(InputItem::EasyMessage(EasyInputMessage {
                        role: Role::User,
                        content: EasyInputContent::Text(text.clone()),
                        ..Default::default()
                    }));
                }
                "model" | "assistant" => {
                    items.push(InputItem::EasyMessage(EasyInputMessage {
                        role: Role::Assistant,
                        content: EasyInputContent::Text(text.clone()),
                        ..Default::default()
                    }));
                }
                _ => {
                    // Fallback: treat as user message
                    items.push(InputItem::EasyMessage(EasyInputMessage {
                        role: Role::User,
                        content: EasyInputContent::Text(text.clone()),
                        ..Default::default()
                    }));
                }
            },

            Part::FunctionCall { name, args, id, .. } => {
                let call_id = id.clone().unwrap_or_else(|| format!("call_{name}"));
                let arguments = serde_json::to_string(args).unwrap_or_default();
                items.push(InputItem::Item(Item::FunctionCall(FunctionToolCall {
                    call_id,
                    name: name.clone(),
                    arguments,
                    id: None,
                    status: None,
                })));
            }

            Part::FunctionResponse { function_response, id } => {
                let call_id = id.clone().unwrap_or_else(|| "unknown".to_string());
                let output_text =
                    serde_json::to_string(&function_response.response).unwrap_or_default();
                items.push(InputItem::Item(Item::FunctionCallOutput(
                    FunctionCallOutputItemParam {
                        call_id,
                        output: FunctionCallOutput::Text(output_text),
                        id: None,
                        status: None,
                    },
                )));
            }

            Part::InlineData { mime_type, data } => {
                if mime_type.starts_with("image/") {
                    let data_uri =
                        format!("data:{mime_type};base64,{}", attachment::encode_base64(data));
                    let image_content = InputContent::InputImage(InputImageContent {
                        image_url: Some(data_uri),
                        detail: Default::default(),
                        file_id: None,
                    });
                    // Wrap in an EasyMessage with content list
                    let msg_role = match role {
                        "model" | "assistant" => Role::Assistant,
                        _ => Role::User,
                    };
                    items.push(InputItem::EasyMessage(EasyInputMessage {
                        role: msg_role,
                        content: EasyInputContent::ContentList(vec![image_content]),
                        ..Default::default()
                    }));
                }
                // Non-image inline data is not directly supported by Responses API;
                // skip silently.
            }

            Part::Thinking { thinking, .. } => {
                // Include thinking text as a text message in the input
                let msg_role = match role {
                    "model" | "assistant" => Role::Assistant,
                    _ => Role::User,
                };
                items.push(InputItem::EasyMessage(EasyInputMessage {
                    role: msg_role,
                    content: EasyInputContent::Text(thinking.clone()),
                    ..Default::default()
                }));
            }

            Part::FileData { .. } => {
                // FileData is not directly mapped to Responses API input items.
                // Could be extended in the future.
            }
            // Server-side tool parts are Gemini-specific; skip for OpenAI
            Part::ServerToolCall { .. } | Part::ServerToolResponse { .. } => {}
        }
    }

    items
}

/// Convert ADK tools map to Responses API `Tool` list.
pub fn convert_tools(tools: &HashMap<String, serde_json::Value>) -> Vec<Tool> {
    tools
        .iter()
        .map(|(name, decl)| {
            let description = decl.get("description").and_then(|d| d.as_str()).map(String::from);
            let parameters = decl.get("parameters").cloned();

            Tool::Function(FunctionTool {
                name: name.clone(),
                description,
                parameters,
                strict: None,
            })
        })
        .collect()
}

/// Map our config `ReasoningEffort` to `async_openai`'s `ReasoningEffort`.
fn map_reasoning_effort(effort: &ReasoningEffort) -> OaiReasoningEffort {
    match effort {
        ReasoningEffort::Low => OaiReasoningEffort::Low,
        ReasoningEffort::Medium => OaiReasoningEffort::Medium,
        ReasoningEffort::High => OaiReasoningEffort::High,
    }
}

/// Map our config `ReasoningSummary` to `async_openai`'s `ReasoningSummary`.
fn map_reasoning_summary(summary: &ReasoningSummary) -> OaiReasoningSummary {
    match summary {
        ReasoningSummary::Auto => OaiReasoningSummary::Auto,
        ReasoningSummary::Concise => OaiReasoningSummary::Concise,
        ReasoningSummary::Detailed => OaiReasoningSummary::Detailed,
    }
}

/// Build a `CreateResponse` from an `LlmRequest`.
///
/// Separates system content into `instructions`, converts non-system content
/// to input items, maps tools, and forwards generation config parameters.
pub fn build_create_response(
    model: &str,
    request: &LlmRequest,
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_summary: Option<ReasoningSummary>,
) -> Result<CreateResponse, AdkError> {
    let config = request.config.as_ref();

    // 1. Separate system content from non-system content
    let mut system_texts = Vec::new();
    let mut non_system_contents = Vec::new();

    for content in &request.contents {
        if content.role == "system" {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    system_texts.push(text.clone());
                }
            }
        } else {
            non_system_contents.push(content.clone());
        }
    }

    // 2. Build instructions from system content
    let instructions = if system_texts.is_empty() { None } else { Some(system_texts.join("\n")) };

    // 3. Convert non-system contents to input items
    let input_items = contents_to_input_items(&non_system_contents);
    let input = InputParam::Items(input_items);

    // 4. Convert tools
    let mut tools_vec = convert_tools(&request.tools);

    // 9. Read extensions["openai"]["built_in_tools"] → append to tools array
    let extensions = config.map(|c| &c.extensions);
    let openai_ext = extensions.and_then(|ext| ext.get("openai")).and_then(|v| v.as_object());

    if let Some(built_in_tools) = openai_ext.and_then(|o| o.get("built_in_tools")) {
        if let Some(arr) = built_in_tools.as_array() {
            for tool_value in arr {
                if let Ok(tool) = serde_json::from_value::<Tool>(tool_value.clone()) {
                    tools_vec.push(tool);
                }
            }
        }
    }

    let tools = if tools_vec.is_empty() { None } else { Some(tools_vec) };

    // 5. Forward temperature, top_p, max_output_tokens
    let temperature = config.and_then(|c| c.temperature);
    let top_p = config.and_then(|c| c.top_p);
    let max_output_tokens = config.and_then(|c| c.max_output_tokens).map(|v| v as u32);

    // 6. Build reasoning field
    let mut effective_effort = reasoning_effort;
    let mut effective_summary = reasoning_summary;

    // 8. Read extensions["openai"]["reasoning"] → override config-level reasoning
    if let Some(reasoning_ext) = openai_ext.and_then(|o| o.get("reasoning")) {
        if let Some(effort_str) = reasoning_ext.get("effort").and_then(|v| v.as_str()) {
            effective_effort = match effort_str {
                "low" => Some(ReasoningEffort::Low),
                "medium" => Some(ReasoningEffort::Medium),
                "high" => Some(ReasoningEffort::High),
                _ => effective_effort,
            };
        }
        if let Some(summary_str) = reasoning_ext.get("summary").and_then(|v| v.as_str()) {
            effective_summary = match summary_str {
                "auto" => Some(ReasoningSummary::Auto),
                "concise" => Some(ReasoningSummary::Concise),
                "detailed" => Some(ReasoningSummary::Detailed),
                _ => effective_summary,
            };
        }
    }

    let reasoning = if effective_effort.is_some() || effective_summary.is_some() {
        Some(Reasoning {
            effort: effective_effort.map(|e| map_reasoning_effort(&e)),
            summary: effective_summary.map(|s| map_reasoning_summary(&s)),
        })
    } else {
        None
    };

    // 7. Read extensions["openai"]["previous_response_id"]
    let previous_response_id = openai_ext
        .and_then(|o| o.get("previous_response_id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    // Build the CreateResponse using the builder
    let mut builder = CreateResponseArgs::default();
    builder.model(model.to_string()).input(input);

    if let Some(inst) = instructions {
        builder.instructions(inst);
    }
    if let Some(t) = tools {
        builder.tools(t);
    }
    if let Some(temp) = temperature {
        builder.temperature(temp);
    }
    if let Some(tp) = top_p {
        builder.top_p(tp);
    }
    if let Some(max) = max_output_tokens {
        builder.max_output_tokens(max);
    }
    if let Some(r) = reasoning {
        builder.reasoning(r);
    }
    if let Some(prev_id) = previous_response_id {
        builder.previous_response_id(prev_id);
    }

    builder.build().map_err(|e| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::InvalidInput,
            "model.openai_responses.build_request",
            format!("failed to build CreateResponse: {e}"),
        )
        .with_provider("openai-responses")
    })
}

/// Convert a Responses API `Response` to an ADK `LlmResponse`.
///
/// Iterates `Response.output` items, converts each via [`output_item_to_parts`],
/// collects all parts into a single `Content`, and maps usage, finish reason,
/// and provider metadata.
pub fn from_response(response: &Response) -> LlmResponse {
    let parts: Vec<Part> = response.output.iter().flat_map(output_item_to_parts).collect();

    let content =
        if parts.is_empty() { None } else { Some(Content { role: "model".to_string(), parts }) };

    let usage_metadata = response.usage.as_ref().map(convert_usage);
    let finish_reason = map_finish_reason(response);
    let provider_metadata = build_provider_metadata(response);

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
        provider_metadata,
    }
}

/// Convert a single `OutputItem` to zero or more ADK `Part`s.
///
/// - `Message` → `Part::Text` for each non-empty text content
/// - `Reasoning` → `Part::Thinking` with concatenated summary text
/// - `FunctionCall` → `Part::FunctionCall` with parsed JSON args
/// - Other variants (WebSearchCall, FileSearchCall, etc.) → empty vec (handled in provider_metadata)
fn output_item_to_parts(item: &OutputItem) -> Vec<Part> {
    match item {
        OutputItem::Message(msg) => msg
            .content
            .iter()
            .filter_map(|c| match c {
                OutputMessageContent::OutputText(t) if !t.text.is_empty() => {
                    Some(Part::Text { text: t.text.clone() })
                }
                _ => None,
            })
            .collect(),
        OutputItem::Reasoning(reasoning) => {
            let concatenated: String = reasoning
                .summary
                .iter()
                .map(|sp| match sp {
                    SummaryPart::SummaryText(t) => t.text.as_str(),
                })
                .collect::<Vec<_>>()
                .join("");

            if concatenated.is_empty() {
                vec![]
            } else {
                vec![Part::Thinking { thinking: concatenated, signature: None }]
            }
        }
        OutputItem::FunctionCall(fc) => {
            let args: serde_json::Value =
                serde_json::from_str(&fc.arguments).unwrap_or(serde_json::json!({}));
            vec![Part::FunctionCall {
                name: fc.name.clone(),
                args,
                id: Some(fc.call_id.clone()),
                thought_signature: None,
            }]
        }
        // Built-in tool outputs: emit as ServerToolCall/ServerToolResponse parts
        OutputItem::WebSearchCall(ws) => {
            if let Ok(val) = serde_json::to_value(ws) {
                vec![Part::ServerToolCall { server_tool_call: val }]
            } else {
                vec![]
            }
        }
        OutputItem::FileSearchCall(fs) => {
            if let Ok(val) = serde_json::to_value(fs) {
                vec![Part::ServerToolCall { server_tool_call: val }]
            } else {
                vec![]
            }
        }
        OutputItem::CodeInterpreterCall(ci) => {
            if let Ok(val) = serde_json::to_value(ci) {
                vec![Part::ServerToolCall { server_tool_call: val }]
            } else {
                vec![]
            }
        }
        // Other variants not yet mapped
        _ => vec![],
    }
}

/// Convert Responses API usage to ADK `UsageMetadata`.
fn convert_usage(usage: &ResponseUsage) -> UsageMetadata {
    UsageMetadata {
        prompt_token_count: usage.input_tokens as i32,
        candidates_token_count: usage.output_tokens as i32,
        total_token_count: (usage.input_tokens + usage.output_tokens) as i32,
        thinking_token_count: Some(usage.output_tokens_details.reasoning_tokens as i32),
        ..Default::default()
    }
}

/// Map response status to ADK `FinishReason`.
fn map_finish_reason(response: &Response) -> Option<FinishReason> {
    match &response.status {
        Status::Completed => Some(FinishReason::Stop),
        Status::Incomplete => {
            if let Some(details) = &response.incomplete_details {
                if details.reason.contains("max_output_tokens") {
                    Some(FinishReason::MaxTokens)
                } else if details.reason.contains("content_filter") {
                    Some(FinishReason::Safety)
                } else {
                    Some(FinishReason::Stop)
                }
            } else {
                Some(FinishReason::Stop)
            }
        }
        Status::Failed => Some(FinishReason::Stop),
        _ => None,
    }
}

/// Build `provider_metadata` JSON from the response.
///
/// Always includes `response_id`. Optionally includes `encrypted_content`
/// from reasoning items and `built_in_tool_outputs` from web search,
/// file search, and code interpreter calls.
fn build_provider_metadata(response: &Response) -> Option<serde_json::Value> {
    let mut openai = serde_json::Map::new();
    openai.insert("response_id".to_string(), serde_json::Value::String(response.id.clone()));

    // Collect encrypted_content from reasoning items
    for item in &response.output {
        if let OutputItem::Reasoning(reasoning) = item {
            if let Some(encrypted) = &reasoning.encrypted_content {
                openai.insert(
                    "encrypted_content".to_string(),
                    serde_json::Value::String(encrypted.clone()),
                );
            }
        }
    }

    // Collect built-in tool outputs
    let mut built_in_outputs = Vec::new();
    for item in &response.output {
        match item {
            OutputItem::WebSearchCall(ws) => {
                if let Ok(val) = serde_json::to_value(ws) {
                    built_in_outputs.push(val);
                }
            }
            OutputItem::FileSearchCall(fs) => {
                if let Ok(val) = serde_json::to_value(fs) {
                    built_in_outputs.push(val);
                }
            }
            OutputItem::CodeInterpreterCall(ci) => {
                if let Ok(val) = serde_json::to_value(ci) {
                    built_in_outputs.push(val);
                }
            }
            _ => {}
        }
    }

    if !built_in_outputs.is_empty() {
        openai.insert(
            "built_in_tool_outputs".to_string(),
            serde_json::Value::Array(built_in_outputs),
        );
    }

    Some(serde_json::json!({ "openai": openai }))
}
