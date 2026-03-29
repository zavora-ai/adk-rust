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
    ApplyPatchToolCallItemParam, ApplyPatchToolCallOutputItemParam, CreateResponse,
    CreateResponseArgs, EasyInputContent, EasyInputMessage, FunctionCallOutput,
    FunctionCallOutputItemParam, FunctionShellCallItemParam, FunctionShellCallOutputItemParam,
    FunctionTool, FunctionToolCall, IncludeEnum, InputContent, InputImageContent, InputItem,
    InputParam, Item, OutputItem, OutputMessageContent, Reasoning,
    ReasoningEffort as OaiReasoningEffort, ReasoningSummary as OaiReasoningSummary, Response,
    ResponseUsage, Role, Status, SummaryPart, Tool, Truncation,
};
use serde::de::DeserializeOwned;
use std::collections::HashMap;

use super::config::{ReasoningEffort, ReasoningSummary};

/// Convert a list of ADK `Content` items to Responses API `InputItem` list.
pub fn contents_to_input_items(contents: &[Content]) -> Vec<InputItem> {
    contents.iter().flat_map(content_to_input_items).collect()
}

/// Returns true when the request includes any OpenAI-native tool declarations.
pub fn request_uses_native_tools(request: &LlmRequest) -> bool {
    request.tools.values().any(|decl| decl.get("x-adk-openai-tool").is_some())
        || request
            .config
            .as_ref()
            .and_then(|config| config.extensions.get("openai"))
            .and_then(|value| value.get("built_in_tools"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|tools| !tools.is_empty())
}

fn request_uses_computer_use_tool(request: &LlmRequest) -> bool {
    request.tools.values().any(|decl| {
        decl.get("x-adk-openai-tool")
            .and_then(|tool| tool.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("computer_use_preview")
    }) || request
        .config
        .as_ref()
        .and_then(|config| config.extensions.get("openai"))
        .and_then(|value| value.get("built_in_tools"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.get("type").and_then(serde_json::Value::as_str) == Some("computer_use_preview")
            })
        })
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
                    crate::tool_result::serialize_tool_result(&function_response.response);
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
            Part::ServerToolCall { server_tool_call } => {
                if let Ok(item) = serde_json::from_value::<Item>(server_tool_call.clone()) {
                    items.push(InputItem::Item(item));
                }
            }
            Part::ServerToolResponse { server_tool_response } => {
                if let Ok(item) = serde_json::from_value::<Item>(server_tool_response.clone()) {
                    items.push(InputItem::Item(item));
                }
            }
        }
    }

    items
}

/// Convert ADK tools map to Responses API `Tool` list.
pub fn convert_tools(tools: &HashMap<String, serde_json::Value>) -> Result<Vec<Tool>, AdkError> {
    tools
        .iter()
        .map(|(name, decl)| {
            if let Some(provider_tool) = decl.get("x-adk-openai-tool") {
                serde_json::from_value::<Tool>(provider_tool.clone()).map_err(|error| {
                    AdkError::new(
                        ErrorComponent::Model,
                        ErrorCategory::InvalidInput,
                        "model.openai_responses.invalid_tool",
                        format!("failed to deserialize OpenAI native tool '{name}': {error}"),
                    )
                    .with_provider("openai-responses")
                })
            } else {
                let description =
                    decl.get("description").and_then(|d| d.as_str()).map(String::from);
                let parameters = decl.get("parameters").cloned();

                Ok(Tool::Function(FunctionTool {
                    name: name.clone(),
                    description,
                    parameters,
                    strict: None,
                }))
            }
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
    let mut tools_vec = convert_tools(&request.tools)?;

    // 9. Read extensions["openai"]["built_in_tools"] → append to tools array
    let extensions = config.map(|c| &c.extensions);
    let openai_ext = extensions.and_then(|ext| ext.get("openai")).and_then(|v| v.as_object());

    if let Some(built_in_tools) = openai_ext.and_then(|o| o.get("built_in_tools")) {
        if let Some(arr) = built_in_tools.as_array() {
            for (index, tool_value) in arr.iter().enumerate() {
                let tool = serde_json::from_value::<Tool>(tool_value.clone()).map_err(|error| {
                    AdkError::new(
                        ErrorComponent::Model,
                        ErrorCategory::InvalidInput,
                        "model.openai_responses.invalid_tool",
                        format!(
                            "failed to deserialize OpenAI built-in tool at index {index}: {error}"
                        ),
                    )
                    .with_provider("openai-responses")
                })?;
                tools_vec.push(tool);
            }
        }
    }

    let tools = if tools_vec.is_empty() { None } else { Some(tools_vec) };

    let include = openai_ext
        .and_then(|o| o.get("include"))
        .map(|value| {
            serde_json::from_value::<Vec<IncludeEnum>>(value.clone()).map_err(|error| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::InvalidInput,
                    "model.openai_responses.invalid_include",
                    format!("failed to deserialize OpenAI include list: {error}"),
                )
                .with_provider("openai-responses")
            })
        })
        .transpose()?;

    let max_tool_calls = openai_ext
        .and_then(|o| o.get("max_tool_calls"))
        .and_then(|value| value.as_u64())
        .map(|value| {
            u32::try_from(value).map_err(|_| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::InvalidInput,
                    "model.openai_responses.invalid_max_tool_calls",
                    format!("OpenAI max_tool_calls '{value}' exceeds u32"),
                )
                .with_provider("openai-responses")
            })
        })
        .transpose()?;

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
    if let Some(include) = include {
        builder.include(include);
    }
    if let Some(t) = tools {
        builder.tools(t);
    }
    if let Some(max_tool_calls) = max_tool_calls {
        builder.max_tool_calls(max_tool_calls);
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
    if request_uses_computer_use_tool(request) {
        builder.truncation(Truncation::Auto);
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
        OutputItem::WebSearchCall(ws) => response_item_part(Item::WebSearchCall(ws.clone()), false),
        OutputItem::FileSearchCall(fs) => {
            response_item_part(Item::FileSearchCall(fs.clone()), false)
        }
        OutputItem::ComputerCall(call) => {
            response_item_part(Item::ComputerCall(call.clone()), false)
        }
        OutputItem::ImageGenerationCall(call) => {
            response_item_part(Item::ImageGenerationCall(call.clone()), false)
        }
        OutputItem::CodeInterpreterCall(call) => {
            response_item_part(Item::CodeInterpreterCall(call.clone()), false)
        }
        OutputItem::LocalShellCall(call) => {
            response_item_part(Item::LocalShellCall(call.clone()), false)
        }
        OutputItem::ShellCall(call) => bridge_response_item::<FunctionShellCallItemParam, _>(call)
            .map(|item| response_item_part(Item::ShellCall(item), false))
            .unwrap_or_default(),
        OutputItem::ShellCallOutput(output) => {
            bridge_response_item::<FunctionShellCallOutputItemParam, _>(output)
                .map(|item| response_item_part(Item::ShellCallOutput(item), true))
                .unwrap_or_default()
        }
        OutputItem::ApplyPatchCall(call) => {
            bridge_response_item::<ApplyPatchToolCallItemParam, _>(call)
                .map(|item| response_item_part(Item::ApplyPatchCall(item), false))
                .unwrap_or_default()
        }
        OutputItem::ApplyPatchCallOutput(output) => {
            bridge_response_item::<ApplyPatchToolCallOutputItemParam, _>(output)
                .map(|item| response_item_part(Item::ApplyPatchCallOutput(item), true))
                .unwrap_or_default()
        }
        OutputItem::McpCall(call) => response_item_part(Item::McpCall(call.clone()), false),
        OutputItem::McpListTools(list) => {
            response_item_part(Item::McpListTools(list.clone()), false)
        }
        OutputItem::McpApprovalRequest(request) => {
            response_item_part(Item::McpApprovalRequest(request.clone()), false)
        }
        OutputItem::CustomToolCall(call) => {
            response_item_part(Item::CustomToolCall(call.clone()), false)
        }
        _ => Vec::new(),
    }
}

fn response_item_part(item: Item, is_output: bool) -> Vec<Part> {
    serde_json::to_value(item)
        .ok()
        .map(|value| {
            if is_output {
                vec![Part::ServerToolResponse { server_tool_response: value }]
            } else {
                vec![Part::ServerToolCall { server_tool_call: value }]
            }
        })
        .unwrap_or_default()
}

fn bridge_response_item<Input, Output>(output: &Output) -> Option<Input>
where
    Input: DeserializeOwned,
    Output: serde::Serialize,
{
    serde_json::to_value(output).ok().and_then(|value| serde_json::from_value(value).ok())
}

fn reasoning_history_parts(response: &Response) -> Vec<serde_json::Value> {
    response
        .output
        .iter()
        .filter_map(|item| match item {
            OutputItem::Reasoning(reasoning) => {
                response_item_part(Item::Reasoning(reasoning.clone()), false).into_iter().next()
            }
            _ => None,
        })
        .filter_map(|part| serde_json::to_value(part).ok())
        .collect()
}

/// Convert Responses API usage to ADK `UsageMetadata`.
fn convert_usage(usage: &ResponseUsage) -> UsageMetadata {
    UsageMetadata {
        prompt_token_count: usage.input_tokens as i32,
        candidates_token_count: usage.output_tokens as i32,
        total_token_count: (usage.input_tokens + usage.output_tokens) as i32,
        cache_read_input_token_count: Some(usage.input_tokens_details.cached_tokens as i32),
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
                built_in_outputs.extend(serde_json::to_value(Item::WebSearchCall(ws.clone())).ok())
            }
            OutputItem::FileSearchCall(fs) => {
                built_in_outputs.extend(serde_json::to_value(Item::FileSearchCall(fs.clone())).ok())
            }
            OutputItem::ComputerCall(call) => {
                built_in_outputs.extend(serde_json::to_value(Item::ComputerCall(call.clone())).ok())
            }
            OutputItem::ImageGenerationCall(call) => built_in_outputs
                .extend(serde_json::to_value(Item::ImageGenerationCall(call.clone())).ok()),
            OutputItem::CodeInterpreterCall(call) => built_in_outputs
                .extend(serde_json::to_value(Item::CodeInterpreterCall(call.clone())).ok()),
            OutputItem::LocalShellCall(call) => built_in_outputs
                .extend(serde_json::to_value(Item::LocalShellCall(call.clone())).ok()),
            OutputItem::ShellCall(call) => built_in_outputs.extend(
                bridge_response_item::<FunctionShellCallItemParam, _>(call)
                    .and_then(|item| serde_json::to_value(Item::ShellCall(item)).ok()),
            ),
            OutputItem::ShellCallOutput(output) => built_in_outputs.extend(
                bridge_response_item::<FunctionShellCallOutputItemParam, _>(output)
                    .and_then(|item| serde_json::to_value(Item::ShellCallOutput(item)).ok()),
            ),
            OutputItem::ApplyPatchCall(call) => built_in_outputs.extend(
                bridge_response_item::<ApplyPatchToolCallItemParam, _>(call)
                    .and_then(|item| serde_json::to_value(Item::ApplyPatchCall(item)).ok()),
            ),
            OutputItem::ApplyPatchCallOutput(output) => built_in_outputs.extend(
                bridge_response_item::<ApplyPatchToolCallOutputItemParam, _>(output)
                    .and_then(|item| serde_json::to_value(Item::ApplyPatchCallOutput(item)).ok()),
            ),
            OutputItem::McpCall(call) => {
                built_in_outputs.extend(serde_json::to_value(Item::McpCall(call.clone())).ok())
            }
            OutputItem::McpListTools(list) => {
                built_in_outputs.extend(serde_json::to_value(Item::McpListTools(list.clone())).ok())
            }
            OutputItem::McpApprovalRequest(request) => built_in_outputs
                .extend(serde_json::to_value(Item::McpApprovalRequest(request.clone())).ok()),
            _ => {}
        }
    }

    if !built_in_outputs.is_empty() {
        openai.insert(
            "built_in_tool_outputs".to_string(),
            serde_json::Value::Array(built_in_outputs),
        );
    }

    let history_parts = reasoning_history_parts(response);
    if !history_parts.is_empty() {
        openai.insert(
            "conversation_history_parts".to_string(),
            serde_json::Value::Array(history_parts),
        );
    }

    Some(serde_json::json!({ "openai": openai }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{GenerateContentConfig, LlmRequest};
    use async_openai::types::responses::{
        WebSearchActionSearch, WebSearchToolCall, WebSearchToolCallAction, WebSearchToolCallStatus,
    };

    #[test]
    fn test_convert_tools_supports_native_openai_tool_declarations() {
        let mut tools = HashMap::new();
        tools.insert(
            "local_shell".to_string(),
            serde_json::json!({
                "x-adk-openai-tool": {
                    "type": "local_shell"
                }
            }),
        );

        let converted = convert_tools(&tools).expect("tool conversion should succeed");
        assert_eq!(converted.len(), 1);
        let value = serde_json::to_value(&converted[0]).expect("tool should serialize");
        assert_eq!(value["type"], "local_shell");
    }

    #[test]
    fn test_server_tool_parts_round_trip_as_openai_items() {
        let parts = output_item_to_parts(&OutputItem::WebSearchCall(WebSearchToolCall {
            action: WebSearchToolCallAction::Search(WebSearchActionSearch {
                query: "rust".to_string(),
                sources: None,
            }),
            id: "ws_123".to_string(),
            status: WebSearchToolCallStatus::Completed,
        }));

        assert!(matches!(parts[0], Part::ServerToolCall { .. }));

        let items = contents_to_input_items(&[Content { role: "model".to_string(), parts }]);

        assert_eq!(items.len(), 1);
        match &items[0] {
            InputItem::Item(Item::WebSearchCall(call)) => {
                assert_eq!(call.id, "ws_123");
                assert_eq!(call.status, WebSearchToolCallStatus::Completed);
            }
            other => panic!("expected web_search_call item, got {other:?}"),
        }
    }

    #[test]
    fn test_server_tool_response_round_trip_as_openai_items() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![Part::ServerToolResponse {
                server_tool_response: serde_json::json!({
                    "type": "shell_call_output",
                    "id": "sh_out_123",
                    "call_id": "call_123",
                    "output": [{
                        "stdout": "ok",
                        "stderr": "",
                        "outcome": {
                            "type": "exit",
                            "exit_code": 0
                        }
                    }],
                    "max_output_length": 1024
                }),
            }],
        };

        let items = contents_to_input_items(&[content]);
        assert_eq!(items.len(), 1);
        match &items[0] {
            InputItem::Item(Item::ShellCallOutput(output)) => {
                assert_eq!(output.call_id, "call_123");
                assert_eq!(output.output.len(), 1);
            }
            other => panic!("expected shell_call_output item, got {other:?}"),
        }
    }

    #[test]
    fn test_build_create_response_rejects_invalid_extension_builtin_tool() {
        let mut extensions = serde_json::Map::new();
        extensions.insert(
            "openai".to_string(),
            serde_json::json!({
                "built_in_tools": [
                    {
                        "type": "not_a_real_tool"
                    }
                ]
            }),
        );

        let request = LlmRequest {
            model: "gpt-5".to_string(),
            contents: vec![],
            config: Some(GenerateContentConfig { extensions, ..Default::default() }),
            tools: HashMap::new(),
        };

        let error = build_create_response("gpt-5", &request, None, None)
            .expect_err("invalid built-in tool should fail");

        assert_eq!(error.code, "model.openai_responses.invalid_tool");
    }

    #[test]
    fn test_request_uses_native_tools_detects_native_declarations() {
        let mut tools = HashMap::new();
        tools.insert(
            "openai_web_search".to_string(),
            serde_json::json!({
                "x-adk-openai-tool": {
                    "type": "web_search_2025_08_26"
                }
            }),
        );

        let request =
            LlmRequest { model: "gpt-5.4".to_string(), contents: vec![], config: None, tools };

        assert!(request_uses_native_tools(&request));
    }

    #[test]
    fn test_build_create_response_sets_auto_truncation_for_computer_use() {
        let mut tools = HashMap::new();
        tools.insert(
            "openai_computer_use".to_string(),
            serde_json::json!({
                "x-adk-openai-tool": {
                    "type": "computer_use_preview",
                    "environment": "browser",
                    "display_width": 1440,
                    "display_height": 900
                }
            }),
        );

        let request = LlmRequest {
            model: "computer-use-preview".to_string(),
            contents: vec![],
            config: None,
            tools,
        };

        let built = build_create_response("computer-use-preview", &request, None, None)
            .expect("request should build");
        assert_eq!(built.truncation, Some(Truncation::Auto));
    }

    #[test]
    fn test_provider_metadata_includes_reasoning_history_parts() {
        let response: Response = serde_json::from_value(serde_json::json!({
            "id": "resp_123",
            "object": "response",
            "created_at": 0,
            "model": "gpt-5.4",
            "status": "completed",
            "output": [
                {
                    "id": "rs_123",
                    "type": "reasoning",
                    "summary": [
                        {
                            "type": "summary_text",
                            "text": "thinking"
                        }
                    ],
                    "encrypted_content": "sealed"
                }
            ],
            "usage": {
                "input_tokens": 1,
                "input_tokens_details": { "cached_tokens": 0 },
                "output_tokens": 1,
                "output_tokens_details": { "reasoning_tokens": 1 },
                "total_tokens": 2
            }
        }))
        .expect("response should deserialize");

        let metadata = build_provider_metadata(&response).expect("metadata should exist");
        let parts = metadata["openai"]["conversation_history_parts"]
            .as_array()
            .expect("history parts should be present");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["server_tool_call"]["type"], "reasoning");
    }
}
