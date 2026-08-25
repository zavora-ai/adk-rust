//! Conversion layer between ADK request/response types and the Gemini
//! Interactions API (Beta) wire types.
//!
//! This module mirrors the `generateContent` request-building logic in
//! [`crate::gemini::client`] but targets the Interactions API's stateful,
//! step-based contract. The request half
//! ([`build_request`](crate::gemini::interactions_convert::build_request)) maps an
//! [`adk_core::LlmRequest`] onto an
//! [`adk_gemini::interactions::CreateInteractionRequest`]; the response half and
//! the tool-mixing validation are layered on top by sibling tasks.
//!
//! Mapping summary (ADK → Interactions):
//!
//! | ADK source | Interactions |
//! |------------|--------------|
//! | system `Content` | `system_instruction` |
//! | user/model `Content` | transcript `Input` (or latest turn when stateful) |
//! | `Part::InlineData`/`FileData` | image/audio/document/video `Content` |
//! | `Part::FunctionResponse{id,..}` | `function_result` `Step{call_id,..}` |
//! | `tools` | `Tool::Function{..}` |
//! | `config.response_schema` | `ResponseFormat::json_schema(schema)` |
//! | thinking level | `generation_config.thinking_level` |
//! | `LlmRequest.previous_response_id` | `previous_interaction_id` (stateful) |
//!
//! This module is only compiled when the `gemini-interactions` feature is
//! enabled.

use std::collections::HashMap;

use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, FinishReason, GenerateContentConfig,
    LlmRequest, LlmResponse, Part, UsageMetadata,
};
use adk_gemini::ThinkingLevel;
use adk_gemini::interactions::{
    AudioContent, Content as IxContent, CreateInteractionRequest, DocumentContent,
    GenerationConfig, ImageContent, Input, Interaction, InteractionSseEvent, InteractionStatus,
    InteractionStreamError, ResponseFormat, Step, StepDelta, Tool, Usage, VideoContent,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::Value;

use super::interactions_target::InteractionTarget;

/// The declaration key that marks a built-in (server-side) Gemini tool.
///
/// Built-in tool wrappers in `adk-tool` (e.g. `GoogleSearchTool`,
/// `UrlContextTool`, `GeminiCodeExecutionTool`) attach this key to their
/// [`Tool::declaration()`](adk_core::Tool::declaration) JSON, with a nested
/// object naming the specific built-in (e.g. `{"google_search": {}}`). Custom
/// function tools — including bypass-converted built-ins — never carry this key;
/// they declare a `parameters` schema instead. The tool-mixing validation in
/// [`build_request`] uses this marker to classify each declaration.
const BUILTIN_TOOL_MARKER: &str = "x-adk-gemini-tool";

/// Maps a completed (non-streaming) [`Interaction`] back into an [`LlmResponse`].
///
/// This is the response half of the Interactions transport: it walks the
/// interaction's [`Step`] timeline, assembling the model turn's [`Part`]s, then
/// folds in usage, the server-assigned interaction id, and a status-derived
/// finish reason. The mapping follows the design's "Response: Interactions → ADK"
/// table:
///
/// - **`model_output`** content blocks become [`Part::Text`] (text),
///   [`Part::InlineData`] (inline base64 image/audio/document/video), or
///   [`Part::FileData`] (URI-referenced media).
/// - **`thought`** steps become a single [`Part::Thinking`] carrying the
///   concatenated summary text and the step signature.
/// - **`function_call`** steps become [`Part::FunctionCall`] (these drive the
///   runner's tool loop).
/// - **server-side tool steps** ([`Step::Other`]) are *not* placed in the
///   content; their raw JSON is collected into
///   `provider_metadata["gemini"]["interaction_steps"]`.
/// - **`user_input`** / **`function_result`** steps are skipped — they are
///   request-side artefacts and unusual in a response.
///
/// The interaction's [`usage`](Interaction::usage) maps to
/// [`UsageMetadata`], its [`id`](Interaction::id) to
/// [`LlmResponse::interaction_id`] (when non-empty), and its
/// [`status`](Interaction::status) to [`finish_reason`](LlmResponse::finish_reason)
/// / `turn_complete` / `partial`:
///
/// | Status | `finish_reason` | `turn_complete` | `partial` |
/// |--------|-----------------|-----------------|-----------|
/// | `Completed` | `Some(Stop)` | `true` | `false` |
/// | `RequiresAction` | `None` | `false` | `false` |
/// | `Incomplete` | `Some(MaxTokens)` | `true` | `false` |
/// | `Failed` / `BudgetExceeded` / `Cancelled` | `Some(Other)` | `true` | `false` |
/// | `InProgress` | `None` | `false` | `true` |
///
/// `Failed` / `BudgetExceeded` are left *total* here — they are mapped to a
/// finish reason rather than an error; the client transport (task 7) decides
/// whether to surface them as `Err`.
///
/// # Arguments
///
/// * `interaction` - The interaction resource to convert.
///
/// # Returns
///
/// An [`LlmResponse`] with `content` set to a `"model"` [`Content`] when any
/// parts were produced (or `None` when the timeline yields no content parts).
pub fn to_llm_response(interaction: &Interaction) -> LlmResponse {
    let mut parts: Vec<Part> = Vec::new();
    let mut server_steps: Vec<Value> = Vec::new();

    for step in &interaction.steps {
        match step {
            Step::ModelOutput { content } => {
                for block in content {
                    if let Some(part) = content_to_part(block) {
                        parts.push(part);
                    }
                }
            }
            Step::Thought { signature, summary } => {
                let thinking =
                    summary.iter().filter_map(IxContent::as_text).collect::<Vec<_>>().join("");
                parts.push(Part::Thinking { thinking, signature: signature.clone() });
            }
            Step::FunctionCall { id, name, arguments, signature } => {
                parts.push(Part::FunctionCall {
                    name: name.clone(),
                    args: arguments.clone(),
                    id: Some(id.clone()),
                    thought_signature: signature.clone(),
                });
            }
            Step::Other(value) => server_steps.push(value.clone()),
            // `user_input` / `function_result` are request-side; skip them.
            Step::UserInput { .. } | Step::FunctionResult { .. } => {}
        }
    }

    let content =
        if parts.is_empty() { None } else { Some(Content { role: "model".to_string(), parts }) };

    let interaction_id =
        if interaction.id.is_empty() { None } else { Some(interaction.id.clone()) };

    let usage_metadata = interaction.usage.as_ref().map(usage_to_metadata);

    let provider_metadata = if server_steps.is_empty() {
        None
    } else {
        Some(serde_json::json!({
            "gemini": {
                "interaction_steps": server_steps,
                "status": status_label(interaction.status),
            }
        }))
    };

    let (finish_reason, mut turn_complete, partial) = status_to_completion(interaction.status);
    if content.as_ref().is_some_and(Content::has_function_calls) {
        turn_complete = false;
    }

    LlmResponse {
        content,
        usage_metadata,
        finish_reason,
        partial,
        turn_complete,
        provider_metadata,
        interaction_id,
        ..Default::default()
    }
}

/// Converts an Interactions response [`IxContent`] block into an ADK [`Part`].
///
/// Inline (base64) media decodes to bytes and becomes [`Part::InlineData`];
/// URI-referenced media becomes [`Part::FileData`]. A block carrying neither
/// `data` nor `uri` yields `None`.
fn content_to_part(block: &IxContent) -> Option<Part> {
    match block {
        IxContent::Text(text) => Some(Part::Text { text: text.text.clone() }),
        IxContent::Image(ImageContent { data, mime_type, uri, .. }) => {
            media_part(data.as_deref(), mime_type.as_deref(), uri.as_deref(), "image/png")
        }
        IxContent::Audio(AudioContent { data, mime_type, uri, .. }) => {
            media_part(data.as_deref(), mime_type.as_deref(), uri.as_deref(), "audio/wav")
        }
        IxContent::Document(DocumentContent { data, mime_type, uri }) => {
            media_part(data.as_deref(), mime_type.as_deref(), uri.as_deref(), "application/pdf")
        }
        IxContent::Video(VideoContent { data, mime_type, uri, .. }) => {
            media_part(data.as_deref(), mime_type.as_deref(), uri.as_deref(), "video/mp4")
        }
    }
}

/// Builds a media [`Part`] from an inline-or-URI content block.
///
/// Inline base64 `data` decodes to [`Part::InlineData`]; a `uri` (with no inline
/// data) becomes [`Part::FileData`]. `default_mime` supplies a MIME type when the
/// block omits one. Returns `None` when the block carries neither payload, or
/// when the inline data fails to decode.
fn media_part(
    data: Option<&str>,
    mime_type: Option<&str>,
    uri: Option<&str>,
    default_mime: &str,
) -> Option<Part> {
    let mime = mime_type.unwrap_or(default_mime).to_string();
    if let Some(encoded) = data {
        let bytes = BASE64_STANDARD.decode(encoded).ok()?;
        Some(Part::InlineData {
            mime_type: mime,
            data: bytes,
            uri: uri.map(str::to_string),
            annotations: None,
        })
    } else {
        uri.map(|file_uri| Part::FileData {
            mime_type: mime,
            file_uri: file_uri.to_string(),
            annotations: None,
        })
    }
}

/// Maps an Interactions [`Usage`] into ADK [`UsageMetadata`].
///
/// `total_input_tokens`/`total_output_tokens`/`total_tokens` map to
/// `prompt_token_count`/`candidates_token_count`/`total_token_count`
/// (saturating `i64` → `i32`). `total_thought_tokens` and `total_cached_tokens`
/// populate `thinking_token_count` / `cache_read_input_token_count` only when
/// positive.
fn usage_to_metadata(usage: &Usage) -> UsageMetadata {
    UsageMetadata {
        prompt_token_count: clamp_i64(usage.total_input_tokens),
        candidates_token_count: clamp_i64(usage.total_output_tokens),
        total_token_count: clamp_i64(usage.total_tokens),
        thinking_token_count: (usage.total_thought_tokens > 0)
            .then(|| clamp_i64(usage.total_thought_tokens)),
        cache_read_input_token_count: (usage.total_cached_tokens > 0)
            .then(|| clamp_i64(usage.total_cached_tokens)),
        ..Default::default()
    }
}

/// Saturating conversion from the wire `i64` token counts to ADK's `i32`.
fn clamp_i64(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// Maps an [`InteractionStatus`] to `(finish_reason, turn_complete, partial)`.
fn status_to_completion(status: InteractionStatus) -> (Option<FinishReason>, bool, bool) {
    match status {
        InteractionStatus::Completed => (Some(FinishReason::Stop), true, false),
        InteractionStatus::RequiresAction => (None, false, false),
        InteractionStatus::Incomplete => (Some(FinishReason::MaxTokens), true, false),
        InteractionStatus::Failed
        | InteractionStatus::BudgetExceeded
        | InteractionStatus::Cancelled => (Some(FinishReason::Other), true, false),
        InteractionStatus::InProgress => (None, false, true),
    }
}

/// Returns the snake_case wire label for an [`InteractionStatus`], used in
/// `provider_metadata`.
fn status_label(status: InteractionStatus) -> &'static str {
    match status {
        InteractionStatus::InProgress => "in_progress",
        InteractionStatus::RequiresAction => "requires_action",
        InteractionStatus::Completed => "completed",
        InteractionStatus::Failed => "failed",
        InteractionStatus::Cancelled => "cancelled",
        InteractionStatus::Incomplete => "incomplete",
        InteractionStatus::BudgetExceeded => "budget_exceeded",
    }
}

/// Builds a [`CreateInteractionRequest`] from an [`LlmRequest`].
///
/// The conversion follows the Interactions transport's mapping rules:
///
/// - **System instruction.** All `Content` entries with role `"system"` have
///   their text parts concatenated (newline-joined) into `system_instruction`.
/// - **Input.** When stateful continuation applies (see below) only the most
///   recent user turn is sent as [`Input::Content`]; otherwise the full
///   transcript is sent. A lone user turn is sent as [`Input::Content`], while a
///   multi-turn transcript is mapped to [`Input::Steps`] (`user_input`,
///   `model_output`, `function_call`, and `function_result` steps).
/// - **Tools.** Each declaration in [`LlmRequest::tools`] is mapped to a
///   [`Tool::Function`]. (Tool-mixing validation is layered on by a later task.)
/// - **Response format.** `config.response_schema` becomes
///   [`ResponseFormat::json_schema`].
/// - **Generation config.** `thinking_level` plus common sampling parameters
///   (`max_output_tokens`, `temperature`, `top_p`, `seed`, `stop_sequences`) are
///   forwarded into [`GenerationConfig`].
/// - **Continuity.** When `stateful` and `store` are both `true` and the request
///   carries a `previous_response_id`, it is set as `previous_interaction_id`.
///
/// # Arguments
///
/// * `request` - The ADK request to convert.
/// * `target` - The validated Interactions destination. A
///   [`InteractionTarget::Model`] sets the request `model` field; a
///   [`InteractionTarget::Agent`] sets the `agent` field.
/// * `thinking_level` - The resolved thinking level (from the model's thinking
///   config), forwarded to `generation_config.thinking_level` when present.
/// * `stateful` - Whether stateful continuation is enabled. When `false`, the
///   full transcript is always sent.
/// * `store` - Whether the interaction is stored server-side. Set on the
///   request as `store`; when `false`, stateful continuation is disabled (per
///   the API's incompatibility rules).
///
/// # Returns
///
/// A [`CreateInteractionRequest`] ready to send. `background` is intentionally
/// left unset for the client transport (task 7) to populate.
///
/// # Errors
///
/// Returns an [`adk_core::Result`] error with category
/// [`ErrorCategory::InvalidInput`] and provider `"gemini"` when the resolved
/// tool set mixes a built-in (server-side) tool with a custom function tool.
/// The Interactions API forbids this combination; the error message names the
/// offending built-in tool(s) and points the developer at
/// `with_bypass_multi_tools_limit`. A uniform tool set (all function tools, all
/// built-in tools, or no tools) builds successfully.
pub fn build_request(
    request: &LlmRequest,
    target: &InteractionTarget,
    thinking_level: Option<ThinkingLevel>,
    stateful: bool,
    store: bool,
) -> adk_core::Result<CreateInteractionRequest> {
    // 1. Separate system content from the conversation transcript.
    let mut system_texts: Vec<String> = Vec::new();
    let mut transcript: Vec<&Content> = Vec::new();
    for content in &request.contents {
        if content.role == "system" {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    system_texts.push(text.clone());
                }
            }
        } else {
            transcript.push(content);
        }
    }
    let system_instruction =
        if system_texts.is_empty() { None } else { Some(system_texts.join("\n")) };

    // 2. Determine whether stateful continuation applies. Per Requirement 3.6,
    //    `store == false` disables continuation regardless of `stateful`.
    let continuation_id = if stateful && store && request.previous_response_id.is_some() {
        request.previous_response_id.clone()
    } else {
        None
    };

    // 3. Build the input. When chaining statefully, send only the latest user
    //    turn (Property 4: stateful minimization); otherwise send the full
    //    transcript.
    let input = if continuation_id.is_some() {
        let latest_user = transcript.iter().rev().find(|c| c.role == "user");
        match latest_user {
            Some(content) => Input::Content(content_parts(content)),
            None => Input::Content(Vec::new()),
        }
    } else {
        build_transcript_input(&transcript)
    };

    // 4. Validate the tool set and map it to uniform `Tool` declarations. The
    //    Interactions API forbids mixing built-in (server-side) tools with
    //    custom function tools in a single request (Requirement 6.5/6.6).
    let tools = build_tools(&request.tools)?;

    // 5. Response schema → structured JSON `ResponseFormat`.
    let response_format = request
        .config
        .as_ref()
        .and_then(|config| config.response_schema.clone())
        .map(ResponseFormat::json_schema);

    // 6. Generation config (thinking level + common sampling parameters).
    let generation_config = build_generation_config(request.config.as_ref(), thinking_level);

    // 7. Resolve the model/agent target field.
    let (model, agent) = match target {
        InteractionTarget::Model(id) => (Some(id.clone()), None),
        InteractionTarget::Agent(id) => (None, Some(id.clone())),
    };

    Ok(CreateInteractionRequest {
        model,
        agent,
        input,
        system_instruction,
        tools,
        response_format,
        store: Some(store),
        generation_config,
        previous_interaction_id: continuation_id,
        // `background` is the client transport's responsibility (task 7).
        ..Default::default()
    })
}

/// Builds the `input` field from a full conversation transcript.
///
/// A single user turn is sent as [`Input::Content`]; any multi-turn transcript
/// (or one that contains model/function turns) is sent as [`Input::Steps`].
fn build_transcript_input(transcript: &[&Content]) -> Input {
    if transcript.len() == 1 && transcript[0].role == "user" {
        return Input::Content(content_parts(transcript[0]));
    }

    let mut steps: Vec<Step> = Vec::new();
    for content in transcript {
        append_steps(content, &mut steps);
    }

    if steps.is_empty() { Input::Content(Vec::new()) } else { Input::Steps(steps) }
}

/// Appends the [`Step`]s produced by a single ADK [`Content`] to `steps`.
///
/// User content becomes a single `user_input` step; model content becomes
/// `function_call` steps (for tool calls) plus a `model_output` step (for the
/// remaining content); function content becomes one `function_result` step per
/// [`Part::FunctionResponse`].
fn append_steps(content: &Content, steps: &mut Vec<Step>) {
    match content.role.as_str() {
        "user" => {
            let blocks = content_parts(content);
            if !blocks.is_empty() {
                steps.push(Step::UserInput { content: blocks });
            }
        }
        "model" => {
            let mut output_blocks: Vec<IxContent> = Vec::new();
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, args, id, thought_signature } => {
                        steps.push(Step::FunctionCall {
                            id: id.clone().unwrap_or_default(),
                            name: name.clone(),
                            arguments: args.clone(),
                            signature: thought_signature.clone(),
                        });
                    }
                    other => {
                        if let Some(block) = part_to_content(other) {
                            output_blocks.push(block);
                        }
                    }
                }
            }
            if !output_blocks.is_empty() {
                steps.push(Step::ModelOutput { content: output_blocks });
            }
        }
        "function" => {
            for part in &content.parts {
                if let Part::FunctionResponse { function_response, id, .. } = part {
                    steps.push(Step::FunctionResult {
                        call_id: id.clone().unwrap_or_default(),
                        name: Some(function_response.name.clone()),
                        result: function_response.response.clone(),
                        is_error: None,
                        signature: None,
                    });
                }
            }
        }
        _ => {}
    }
}

/// Converts the parts of a [`Content`] into Interactions content blocks,
/// skipping parts that have no input representation (thinking traces, tool
/// calls/responses, server-tool parts).
fn content_parts(content: &Content) -> Vec<IxContent> {
    content.parts.iter().filter_map(part_to_content).collect()
}

/// Converts a single ADK [`Part`] into an Interactions [`IxContent`] block.
///
/// Returns `None` for parts that are not input content (thinking, function
/// calls/responses, server-tool parts) — these are represented as [`Step`]s or
/// omitted.
fn part_to_content(part: &Part) -> Option<IxContent> {
    match part {
        Part::Text { text } => Some(IxContent::text(text.clone())),
        Part::InlineData { mime_type, data, .. } => {
            let encoded = crate::attachment::encode_base64(data);
            Some(inline_content(mime_type, encoded))
        }
        Part::FileData { mime_type, file_uri, .. } => Some(uri_content(mime_type, file_uri)),
        _ => None,
    }
}

/// Builds an inline (base64) content block, choosing the variant from the MIME
/// type's top-level category.
fn inline_content(mime_type: &str, data: String) -> IxContent {
    if mime_type.starts_with("image/") {
        IxContent::image(data, mime_type)
    } else if mime_type.starts_with("audio/") {
        IxContent::audio(data, mime_type)
    } else if mime_type.starts_with("video/") {
        IxContent::Video(VideoContent {
            data: Some(data),
            mime_type: Some(mime_type.to_string()),
            uri: None,
            resolution: None,
        })
    } else {
        IxContent::document(data, mime_type)
    }
}

/// Builds a URI-referenced content block, choosing the variant from the MIME
/// type's top-level category.
fn uri_content(mime_type: &str, uri: &str) -> IxContent {
    if mime_type.starts_with("image/") {
        IxContent::Image(ImageContent {
            data: None,
            mime_type: Some(mime_type.to_string()),
            uri: Some(uri.to_string()),
            resolution: None,
        })
    } else if mime_type.starts_with("audio/") {
        IxContent::Audio(AudioContent {
            data: None,
            mime_type: Some(mime_type.to_string()),
            uri: Some(uri.to_string()),
            sample_rate: None,
            channels: None,
        })
    } else if mime_type.starts_with("video/") {
        IxContent::video_uri(uri)
    } else {
        IxContent::Document(DocumentContent {
            data: None,
            mime_type: Some(mime_type.to_string()),
            uri: Some(uri.to_string()),
        })
    }
}

/// Classifies a tool declaration as built-in (server-side) when it carries the
/// [`BUILTIN_TOOL_MARKER`] key.
///
/// Custom function tools — including bypass-converted built-ins — declare a
/// `parameters` schema and never carry the marker, so this returns `false` for
/// them.
fn is_builtin_declaration(decl: &Value) -> bool {
    decl.get(BUILTIN_TOOL_MARKER).is_some()
}

/// Validates the resolved tool set and maps it to Interactions [`Tool`] entries.
///
/// The Interactions API forbids mixing built-in (server-side) tools with custom
/// function tools in a single request. This function partitions the
/// declarations by [`is_builtin_declaration`] and:
///
/// - **errors** with [`ErrorCategory::InvalidInput`] when the set contains *both*
///   at least one built-in tool and at least one function tool (Requirement
///   6.5), naming the offending built-in tool(s) and pointing the developer at
///   `with_bypass_multi_tools_limit`;
/// - **succeeds** when the set is uniform — all function tools (Requirement 6.6),
///   all built-in tools, or empty.
///
/// Function declarations become [`Tool::Function`]. Built-in declarations (only
/// reachable when the set is built-in-only) are mapped to their dedicated
/// [`Tool`] variant when recognised (`google_search`, `code_execution`,
/// `url_context`) and otherwise passed through verbatim as [`Tool::Other`].
///
/// Declarations are processed in name-sorted order so the produced request is
/// deterministic (the source is a [`HashMap`], whose iteration order is
/// unspecified).
fn build_tools(tools: &HashMap<String, Value>) -> adk_core::Result<Vec<Tool>> {
    let mut entries: Vec<(&String, &Value)> = tools.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));

    let mut builtin_names: Vec<String> = Vec::new();
    let mut function_present = false;
    for (key, decl) in &entries {
        if is_builtin_declaration(decl) {
            let name = decl.get("name").and_then(Value::as_str).unwrap_or(key).to_string();
            builtin_names.push(name);
        } else {
            function_present = true;
        }
    }

    // The genuine mix — at least one built-in AND at least one function tool —
    // is the only case the API rejects (Requirement 6.5).
    if !builtin_names.is_empty() && function_present {
        return Err(mixed_tools_error(&builtin_names));
    }

    let mapped = entries.into_iter().map(|(key, decl)| map_tool(key, decl)).collect();
    Ok(mapped)
}

/// Maps a single declaration to an Interactions [`Tool`].
///
/// Function declarations (no built-in marker) become [`Tool::Function`].
/// Built-in declarations map to their dedicated variant when recognised, falling
/// back to [`Tool::Other`] with the raw declaration for any other built-in.
fn map_tool(key: &str, decl: &Value) -> Tool {
    if let Some(builtin) = decl.get(BUILTIN_TOOL_MARKER) {
        return map_builtin_tool(builtin, decl);
    }
    let name = decl.get("name").and_then(Value::as_str).unwrap_or(key).to_string();
    let description = decl.get("description").and_then(Value::as_str).map(ToString::to_string);
    let parameters = decl.get("parameters").cloned();
    Tool::Function { name, description, parameters }
}

/// Maps a built-in tool declaration to its Interactions [`Tool`] variant.
///
/// The `marker` is the value of the [`BUILTIN_TOOL_MARKER`] key, a nested object
/// naming the specific built-in (e.g. `{"google_search": {}}`). Recognised
/// built-ins map to their dedicated variant; anything else is passed through as
/// [`Tool::Other`] carrying the raw declaration so forward-compatibility is
/// preserved.
fn map_builtin_tool(marker: &Value, decl: &Value) -> Tool {
    if marker.get("google_search").is_some() {
        Tool::GoogleSearch { search_types: Vec::new() }
    } else if marker.get("code_execution").is_some() {
        Tool::CodeExecution
    } else if marker.get("url_context").is_some() {
        Tool::UrlContext
    } else {
        Tool::Other(decl.clone())
    }
}

/// Builds the `InvalidInput` error returned when a request mixes built-in and
/// function tools.
///
/// The message names the offending built-in tool(s) and points the developer at
/// `with_bypass_multi_tools_limit`, mirroring ADK-Python's guidance.
fn mixed_tools_error(builtin_names: &[String]) -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::InvalidInput,
        "model.gemini.interactions.mixed_tools",
        format!(
            "the Interactions API cannot mix built-in tool(s) [{}] with custom function tools in \
             one request. Convert each built-in tool with `with_bypass_multi_tools_limit` so the \
             tool set is uniform.",
            builtin_names.join(", "),
        ),
    )
    .with_provider("gemini")
}

/// Builds the Interactions [`GenerationConfig`] from the ADK generation config
/// and the resolved thinking level.
///
/// Returns `None` when there is nothing to configure, leaving the request's
/// `generation_config` unset.
fn build_generation_config(
    config: Option<&GenerateContentConfig>,
    thinking_level: Option<ThinkingLevel>,
) -> Option<GenerationConfig> {
    let mut gen_config = GenerationConfig::default();
    let mut populated = false;

    if let Some(config) = config {
        if let Some(max_output_tokens) = config.max_output_tokens {
            gen_config.max_output_tokens = Some(max_output_tokens);
            populated = true;
        }
        if let Some(temperature) = config.temperature {
            gen_config.temperature = Some(temperature);
            populated = true;
        }
        if let Some(top_p) = config.top_p {
            gen_config.top_p = Some(top_p);
            populated = true;
        }
        if let Some(seed) = config.seed {
            gen_config.seed = Some(seed);
            populated = true;
        }
        if !config.stop_sequences.is_empty() {
            gen_config.stop_sequences = config.stop_sequences.clone();
            populated = true;
        }
    }

    if let Some(level) = thinking_level {
        gen_config.thinking_level = Some(level);
        populated = true;
    }

    if populated { Some(gen_config) } else { None }
}

// ══════════════════════════════════════════════════════════════════════
// Streaming: SSE event → LlmResponse chunk
// ══════════════════════════════════════════════════════════════════════

/// Accumulator threaded across an interaction SSE stream.
///
/// The Interactions API streams a turn as a sequence of
/// [`InteractionSseEvent`]s rather than a single response. Text and function-call
/// arguments arrive incrementally and the terminal `interaction.completed` event
/// carries (typically) empty steps — so the final response must be assembled by
/// folding the stream. This accumulator carries the cross-event state the fold
/// needs: the server-assigned interaction id and the in-flight function-call
/// builders keyed by step index.
///
/// The fold itself lives in [`sse_event_to_chunk`], which is a pure function of
/// `(event, &mut SseAccumulator)` so it can be unit-tested without a network
/// round-trip (the streaming transport in `client` simply drives it over the
/// real SSE stream).
#[derive(Debug, Default)]
pub(crate) struct SseAccumulator {
    /// The server-assigned interaction id, recorded from `interaction.created`,
    /// `interaction.status_update`, or `interaction.completed`. Stamped onto
    /// every emitted chunk so the agent's continuity plumbing can chain turns.
    interaction_id: Option<String>,
    /// In-flight function-call deltas keyed by step index. The Interactions API
    /// streams `arguments_delta` fragments (partial JSON) that must be
    /// concatenated; `name`/`id` arrive on the first delta.
    function_calls: HashMap<i64, FunctionCallBuilder>,
}

/// A function call being assembled from streamed `step.delta` fragments.
#[derive(Debug, Default)]
struct FunctionCallBuilder {
    name: Option<String>,
    id: Option<String>,
    arguments: String,
}

impl SseAccumulator {
    /// Creates an empty accumulator for a fresh interaction stream.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns the recorded interaction id, if any has been seen yet.
    ///
    /// Used by the streaming transport's tests to assert id propagation; the
    /// fold functions read the field directly.
    #[cfg(test)]
    pub(crate) fn interaction_id(&self) -> Option<&str> {
        self.interaction_id.as_deref()
    }
}

/// Folds a single [`InteractionSseEvent`] into the running [`SseAccumulator`],
/// optionally producing an [`LlmResponse`] chunk to yield.
///
/// This is the streaming counterpart to [`to_llm_response`]: where that maps a
/// *completed* [`Interaction`] in one shot, this maps the SSE event timeline
/// incrementally, following the design's "Streaming event → LlmResponse chunk"
/// table:
///
/// | SSE event | Result |
/// |-----------|--------|
/// | `interaction.created` | record id; no chunk |
/// | `interaction.status_update` | record id; no chunk |
/// | `step.start` / `step.stop` | no chunk (function calls flush on completion) |
/// | `step.delta` text | partial [`Part::Text`] chunk (`partial = true`) |
/// | `step.delta` function_call | accumulate args; no chunk |
/// | `interaction.completed` | final chunk; tool-call turns remain open for Runner execution |
/// | `error` | `Some(Err(AdkError))` |
/// | `Other` | ignored (forward-compatible) |
///
/// Text deltas are emitted immediately as partial chunks so the runner can
/// stream tokens to the client. Function-call argument fragments are *not*
/// emitted per-delta — a partial JSON arguments string is not a usable
/// `FunctionCall` — they are accumulated and flushed as complete
/// [`Part::FunctionCall`]s on `interaction.completed`. Each chunk carries the
/// recorded `interaction_id` so the agent's continuity plumbing works
/// identically to the non-streaming path.
///
/// # Arguments
///
/// * `event` - The SSE event to fold.
/// * `acc` - The cross-event accumulator (interaction id + in-flight calls).
///
/// # Returns
///
/// `Some(Ok(chunk))` to yield a response chunk, `Some(Err(..))` for a terminal
/// `error` event, or `None` for events that only update accumulator state.
pub(crate) fn sse_event_to_chunk(
    event: InteractionSseEvent,
    acc: &mut SseAccumulator,
) -> Option<adk_core::Result<LlmResponse>> {
    match event {
        InteractionSseEvent::InteractionCreated { interaction, .. } => {
            record_id(acc, interaction.id);
            None
        }
        InteractionSseEvent::InteractionStatusUpdate { interaction_id, .. } => {
            record_id(acc, interaction_id);
            None
        }
        InteractionSseEvent::StepDelta { index, delta, .. } => {
            sse_step_delta_to_chunk(index, delta, acc)
        }
        InteractionSseEvent::InteractionCompleted { interaction, .. } => {
            Some(Ok(complete_chunk(interaction, acc)))
        }
        InteractionSseEvent::Error { error, .. } => Some(Err(stream_error_to_adk(&error))),
        // `step.start` / `step.stop` carry no content for the chunk stream; the
        // accumulated function calls flush on completion. `Other` is ignored for
        // forward-compatibility with future event types.
        InteractionSseEvent::StepStart { .. }
        | InteractionSseEvent::StepStop { .. }
        | InteractionSseEvent::Other(_) => None,
    }
}

/// Records a non-empty interaction id onto the accumulator.
fn record_id(acc: &mut SseAccumulator, id: String) {
    if !id.is_empty() {
        acc.interaction_id = Some(id);
    }
}

/// Maps a `step.delta` to a chunk (text) or accumulates it (function call).
fn sse_step_delta_to_chunk(
    index: i64,
    delta: StepDelta,
    acc: &mut SseAccumulator,
) -> Option<adk_core::Result<LlmResponse>> {
    match delta {
        StepDelta::Text { text } => Some(Ok(partial_text_chunk(text, acc.interaction_id.clone()))),
        StepDelta::FunctionCall { arguments_delta, name, id } => {
            let builder = acc.function_calls.entry(index).or_default();
            if let Some(name) = name {
                builder.name = Some(name);
            }
            if let Some(id) = id {
                builder.id = Some(id);
            }
            if let Some(fragment) = arguments_delta {
                builder.arguments.push_str(&fragment);
            }
            None
        }
        // A delta type this crate version does not model: ignore it.
        StepDelta::Other(_) => None,
    }
}

/// Builds a partial text chunk (`partial = true`, `turn_complete = false`).
fn partial_text_chunk(text: String, interaction_id: Option<String>) -> LlmResponse {
    LlmResponse {
        content: Some(Content { role: "model".to_string(), parts: vec![Part::Text { text }] }),
        partial: true,
        turn_complete: false,
        interaction_id,
        ..Default::default()
    }
}

/// Builds the terminal chunk for an `interaction.completed` event.
///
/// Flushes any accumulated function calls into [`Part::FunctionCall`]s (the tool
/// loop needs complete calls), folds in usage/finish-reason from the completed
/// interaction, and stamps the interaction id. When the completed interaction
/// carries its own steps (some servers populate them), those parts are taken via
/// [`to_llm_response`] and the streamed function calls are appended.
fn complete_chunk(interaction: Interaction, acc: &mut SseAccumulator) -> LlmResponse {
    record_id(acc, interaction.id.clone());

    // Start from whatever the completed interaction carries (usually empty
    // steps, but be faithful when populated), then append the flushed
    // function calls accumulated from the delta stream.
    let mut response = to_llm_response(&interaction);

    let flushed = flush_function_calls(acc);
    if !flushed.is_empty() {
        match response.content {
            Some(ref mut content) => content.parts.extend(flushed),
            None => {
                response.content = Some(Content { role: "model".to_string(), parts: flushed });
            }
        }
    }

    // The completed event is the final protocol chunk, but a tool-call turn
    // remains open so the Runner can execute the requested tools.
    response.partial = false;
    response.turn_complete =
        response.content.as_ref().is_none_or(|content| !content.has_function_calls());
    if response.interaction_id.is_none() {
        response.interaction_id = acc.interaction_id.clone();
    }
    if response.finish_reason.is_none() {
        response.finish_reason = Some(FinishReason::Stop);
    }
    response
}

/// Drains the accumulated function-call builders into [`Part::FunctionCall`]s.
///
/// Builders are flushed in step-index order so the produced parts are
/// deterministic. Each accumulated `arguments` JSON string is parsed into a
/// [`Value`]; an empty or unparseable string falls back to an empty object so
/// the tool loop always receives a structured arguments value.
fn flush_function_calls(acc: &mut SseAccumulator) -> Vec<Part> {
    let mut entries: Vec<(i64, FunctionCallBuilder)> = acc.function_calls.drain().collect();
    entries.sort_by_key(|(index, _)| *index);
    entries
        .into_iter()
        .filter_map(|(_, builder)| {
            let name = builder.name?;
            let args = parse_arguments(&builder.arguments);
            Some(Part::FunctionCall { name, args, id: builder.id, thought_signature: None })
        })
        .collect()
}

/// Parses an accumulated function-call arguments string into a [`Value`].
///
/// Returns an empty JSON object when the string is empty or fails to parse, so
/// the tool loop always receives a well-formed arguments value.
fn parse_arguments(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

/// Maps a streamed [`InteractionStreamError`] to an [`AdkError`].
///
/// The error is attributed to the `"gemini"` provider with category
/// [`ErrorCategory::Internal`]; the wire `code` (when present) is folded into
/// the human-readable message so callers can distinguish stream error types.
fn stream_error_to_adk(error: &InteractionStreamError) -> AdkError {
    let base = if error.message.is_empty() {
        "the Gemini interaction stream reported an error".to_string()
    } else {
        error.message.clone()
    };
    let message = match &error.code {
        Some(code) if !code.is_empty() => format!("{base} (code: {code})"),
        _ => base,
    };
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Internal,
        "model.gemini.interactions.stream_error",
        message,
    )
    .with_provider("gemini")
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::FunctionResponseData;

    fn request_with(contents: Vec<Content>) -> LlmRequest {
        LlmRequest {
            model: "gemini-2.5-flash".to_string(),
            contents,
            config: None,
            tools: HashMap::new(),
            previous_response_id: None,
        }
    }

    fn model_target() -> InteractionTarget {
        InteractionTarget::Model("gemini-2.5-flash".to_string())
    }

    #[test]
    fn single_turn_maps_contents_to_input_and_sets_model_target() {
        let request = request_with(vec![Content::new("user").with_text("Hello there")]);

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        assert_eq!(built.model.as_deref(), Some("gemini-2.5-flash"));
        assert_eq!(built.agent, None);
        match built.input {
            Input::Content(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].as_text(), Some("Hello there"));
            }
            other => panic!("expected Input::Content, got {other:?}"),
        }
    }

    #[test]
    fn agent_target_sets_agent_field() {
        let request = request_with(vec![Content::new("user").with_text("Research this")]);
        let target = InteractionTarget::Agent("deep-research-preview-04-2026".to_string());

        let built = build_request(&request, &target, None, true, true)
            .expect("build_request should succeed");

        assert_eq!(built.agent.as_deref(), Some("deep-research-preview-04-2026"));
        assert_eq!(built.model, None);
    }

    #[test]
    fn system_content_maps_to_system_instruction() {
        let request = request_with(vec![
            Content::new("system").with_text("You are helpful."),
            Content::new("user").with_text("Hi"),
        ]);

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        assert_eq!(built.system_instruction.as_deref(), Some("You are helpful."));
        // The system turn must not leak into the input.
        match built.input {
            Input::Content(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].as_text(), Some("Hi"));
            }
            other => panic!("expected Input::Content, got {other:?}"),
        }
    }

    #[test]
    fn previous_response_id_sets_continuation_and_sends_latest_turn_only() {
        // A multi-turn transcript with a prior interaction id and stateful=true.
        let mut request = request_with(vec![
            Content::new("user").with_text("First question"),
            Content::new("model").with_text("First answer"),
            Content::new("user").with_text("Second question"),
        ]);
        request.previous_response_id = Some("interaction-123".to_string());

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        // Continuation id is set.
        assert_eq!(built.previous_interaction_id.as_deref(), Some("interaction-123"));

        // Only the latest user turn is sent (Property 4: stateful minimization).
        match built.input {
            Input::Content(blocks) => {
                assert_eq!(blocks.len(), 1, "stateful continuation must send only the latest turn");
                assert_eq!(blocks[0].as_text(), Some("Second question"));
            }
            other => panic!("expected Input::Content with the latest turn, got {other:?}"),
        }
    }

    #[test]
    fn stateless_when_store_false_disables_continuation() {
        let mut request = request_with(vec![Content::new("user").with_text("Only question")]);
        request.previous_response_id = Some("interaction-123".to_string());

        // store=false must disable stateful continuation and send the transcript.
        let built = build_request(&request, &model_target(), None, true, false)
            .expect("build_request should succeed");

        assert_eq!(built.previous_interaction_id, None);
        assert_eq!(built.store, Some(false));
        match built.input {
            Input::Content(blocks) => assert_eq!(blocks[0].as_text(), Some("Only question")),
            other => panic!("expected Input::Content, got {other:?}"),
        }
    }

    #[test]
    fn multi_turn_transcript_maps_to_steps_when_not_chaining() {
        let request = request_with(vec![
            Content::new("user").with_text("Question one"),
            Content::new("model").with_text("Answer one"),
            Content::new("user").with_text("Question two"),
        ]);

        // No previous_response_id → full transcript as steps.
        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        match built.input {
            Input::Steps(steps) => {
                assert_eq!(steps.len(), 3);
                assert!(matches!(steps[0], Step::UserInput { .. }));
                assert!(matches!(steps[1], Step::ModelOutput { .. }));
                assert!(matches!(steps[2], Step::UserInput { .. }));
            }
            other => panic!("expected Input::Steps, got {other:?}"),
        }
    }

    #[test]
    fn function_response_maps_to_function_result_step() {
        let request = request_with(vec![
            Content::new("user").with_text("What's the weather?"),
            Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "get_weather".to_string(),
                    args: serde_json::json!({"city": "Boston"}),
                    id: Some("call-1".to_string()),
                    thought_signature: None,
                }],
            },
            Content {
                role: "function".to_string(),
                parts: vec![Part::FunctionResponse {
                    function_response: FunctionResponseData::new(
                        "get_weather",
                        serde_json::json!({"temp": 72}),
                    ),
                    id: Some("call-1".to_string()),
                    annotations: None,
                }],
            },
        ]);

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        match built.input {
            Input::Steps(steps) => {
                let has_call = steps.iter().any(|s| {
                    matches!(s, Step::FunctionCall { id, name, .. } if id == "call-1" && name == "get_weather")
                });
                let has_result = steps.iter().any(
                    |s| matches!(s, Step::FunctionResult { call_id, .. } if call_id == "call-1"),
                );
                assert!(has_call, "expected a function_call step");
                assert!(has_result, "expected a function_result step");
            }
            other => panic!("expected Input::Steps, got {other:?}"),
        }
    }

    #[test]
    fn response_schema_maps_to_response_format() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } }
        });
        let mut request = request_with(vec![Content::new("user").with_text("Answer in JSON")]);
        request.config = Some(GenerateContentConfig {
            response_schema: Some(schema.clone()),
            ..Default::default()
        });

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        match built.response_format {
            Some(ResponseFormat::Text { mime_type, schema: out_schema }) => {
                assert_eq!(mime_type.as_deref(), Some("application/json"));
                assert_eq!(out_schema, Some(schema));
            }
            other => panic!("expected ResponseFormat::Text, got {other:?}"),
        }
    }

    #[test]
    fn thinking_level_maps_to_generation_config() {
        let request = request_with(vec![Content::new("user").with_text("Think hard")]);

        let built = build_request(&request, &model_target(), Some(ThinkingLevel::High), true, true)
            .expect("build_request should succeed");

        let gen_config = built.generation_config.expect("generation_config should be set");
        assert_eq!(gen_config.thinking_level, Some(ThinkingLevel::High));
    }

    #[test]
    fn no_generation_config_when_nothing_to_configure() {
        let request = request_with(vec![Content::new("user").with_text("Plain")]);

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        assert!(built.generation_config.is_none());
    }

    #[test]
    fn sampling_parameters_map_to_generation_config() {
        let mut request = request_with(vec![Content::new("user").with_text("Configure me")]);
        request.config = Some(GenerateContentConfig {
            max_output_tokens: Some(256),
            temperature: Some(0.5),
            top_p: Some(0.9),
            seed: Some(42),
            stop_sequences: vec!["STOP".to_string()],
            ..Default::default()
        });

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        let gen_config = built.generation_config.expect("generation_config should be set");
        assert_eq!(gen_config.max_output_tokens, Some(256));
        assert_eq!(gen_config.temperature, Some(0.5));
        assert_eq!(gen_config.top_p, Some(0.9));
        assert_eq!(gen_config.seed, Some(42));
        assert_eq!(gen_config.stop_sequences, vec!["STOP".to_string()]);
    }

    #[test]
    fn tools_map_to_function_tools() {
        let mut tools = HashMap::new();
        tools.insert(
            "get_weather".to_string(),
            serde_json::json!({
                "name": "get_weather",
                "description": "Get the weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } }
                }
            }),
        );
        tools.insert(
            "get_time".to_string(),
            serde_json::json!({
                "name": "get_time",
                "description": "Get the current time"
            }),
        );

        let mut request = request_with(vec![Content::new("user").with_text("Tools please")]);
        request.tools = tools;

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        assert_eq!(built.tools.len(), 2);
        // Sorted by name → get_time, get_weather.
        match &built.tools[0] {
            Tool::Function { name, description, .. } => {
                assert_eq!(name, "get_time");
                assert_eq!(description.as_deref(), Some("Get the current time"));
            }
            other => panic!("expected Tool::Function, got {other:?}"),
        }
        match &built.tools[1] {
            Tool::Function { name, parameters, .. } => {
                assert_eq!(name, "get_weather");
                assert!(parameters.is_some(), "get_weather should carry its parameters schema");
            }
            other => panic!("expected Tool::Function, got {other:?}"),
        }
    }

    #[test]
    fn tool_name_falls_back_to_map_key() {
        let mut tools = HashMap::new();
        // Declaration with no explicit "name" field.
        tools.insert(
            "lookup".to_string(),
            serde_json::json!({ "description": "Look something up" }),
        );

        let mut request = request_with(vec![Content::new("user").with_text("Hi")]);
        request.tools = tools;

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        match &built.tools[0] {
            Tool::Function { name, .. } => assert_eq!(name, "lookup"),
            other => panic!("expected Tool::Function, got {other:?}"),
        }
    }

    /// A built-in declaration (carrying the `x-adk-gemini-tool` marker) is the
    /// shape produced by `GoogleSearchTool::declaration()`.
    fn builtin_search_declaration() -> Value {
        serde_json::json!({
            "name": "google_search",
            "description": "Performs a Google search to retrieve information from the web.",
            "x-adk-gemini-tool": { "google_search": {} }
        })
    }

    /// A custom function declaration (carrying `parameters`, no marker).
    fn function_declaration(name: &str) -> Value {
        serde_json::json!({
            "name": name,
            "description": format!("Custom tool {name}"),
            "parameters": {
                "type": "object",
                "properties": { "value": { "type": "string" } }
            }
        })
    }

    #[test]
    fn mixing_builtin_and_function_tools_errors_invalid_input() {
        let mut tools = HashMap::new();
        tools.insert("google_search".to_string(), builtin_search_declaration());
        tools.insert("get_weather".to_string(), function_declaration("get_weather"));

        let mut request = request_with(vec![Content::new("user").with_text("Search and tools")]);
        request.tools = tools;

        let err = build_request(&request, &model_target(), None, true, true)
            .expect_err("mixing built-in and function tools must error");

        assert_eq!(err.category, ErrorCategory::InvalidInput);
        assert_eq!(err.details.provider.as_deref(), Some("gemini"));
        assert_eq!(err.code, "model.gemini.interactions.mixed_tools");

        let message = err.to_string();
        // The message names the offending built-in tool.
        assert!(message.contains("google_search"), "message should name the built-in tool");
        // The message points the developer at the bypass option.
        assert!(
            message.contains("with_bypass_multi_tools_limit"),
            "message should reference the bypass option"
        );
    }

    #[test]
    fn all_function_tools_including_bypass_converted_succeed() {
        let mut tools = HashMap::new();
        // A bypass-converted google_search declares `parameters` and no marker,
        // so it is indistinguishable from any other function tool.
        tools.insert(
            "google_search".to_string(),
            serde_json::json!({
                "name": "google_search",
                "description": "Performs a Google search to retrieve information from the web.",
                "parameters": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }
            }),
        );
        tools.insert("get_weather".to_string(), function_declaration("get_weather"));

        let mut request = request_with(vec![Content::new("user").with_text("Uniform tools")]);
        request.tools = tools;

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("a uniform function-tool set must build successfully");

        assert_eq!(built.tools.len(), 2);
        for tool in &built.tools {
            assert!(
                matches!(tool, Tool::Function { .. }),
                "every tool should be a function tool, got {tool:?}"
            );
        }
    }

    #[test]
    fn builtin_only_set_succeeds_and_maps_to_dedicated_variant() {
        let mut tools = HashMap::new();
        tools.insert("google_search".to_string(), builtin_search_declaration());

        let mut request = request_with(vec![Content::new("user").with_text("Search only")]);
        request.tools = tools;

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("a built-in-only tool set must build successfully");

        assert_eq!(built.tools.len(), 1);
        assert!(
            matches!(built.tools[0], Tool::GoogleSearch { .. }),
            "google_search should map to Tool::GoogleSearch, got {:?}",
            built.tools[0]
        );
    }

    #[test]
    fn unrecognized_builtin_only_set_maps_to_other() {
        let mut tools = HashMap::new();
        tools.insert(
            "google_maps".to_string(),
            serde_json::json!({
                "name": "google_maps",
                "description": "Maps grounding",
                "x-adk-gemini-tool": { "google_maps": {} }
            }),
        );

        let mut request = request_with(vec![Content::new("user").with_text("Maps")]);
        request.tools = tools;

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("an unrecognized built-in-only set must still build");

        assert_eq!(built.tools.len(), 1);
        assert!(
            matches!(built.tools[0], Tool::Other(_)),
            "an unrecognized built-in should pass through as Tool::Other, got {:?}",
            built.tools[0]
        );
    }

    #[test]
    fn inline_image_part_maps_to_image_content() {
        let request = request_with(vec![
            Content::new("user")
                .with_text("Describe")
                .with_inline_data("image/png", vec![0x89, 0x50, 0x4E, 0x47]),
        ]);

        let built = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");

        match built.input {
            Input::Content(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert!(
                    matches!(blocks[1], IxContent::Image(_)),
                    "expected an image content block"
                );
            }
            other => panic!("expected Input::Content, got {other:?}"),
        }
    }

    #[test]
    fn store_flag_is_forwarded() {
        let request = request_with(vec![Content::new("user").with_text("Store me")]);

        let stored = build_request(&request, &model_target(), None, true, true)
            .expect("build_request should succeed");
        assert_eq!(stored.store, Some(true));

        let not_stored = build_request(&request, &model_target(), None, true, false)
            .expect("build_request should succeed");
        assert_eq!(not_stored.store, Some(false));
    }

    // ── to_llm_response ────────────────────────────────────────────────

    /// Builds an [`Interaction`] with the given id, status, and steps.
    fn interaction_with(id: &str, status: InteractionStatus, steps: Vec<Step>) -> Interaction {
        Interaction {
            id: id.to_string(),
            model: Some("gemini-2.5-flash".to_string()),
            agent: None,
            status,
            steps,
            usage: None,
            created: None,
            updated: None,
            environment_id: None,
        }
    }

    #[test]
    fn completed_text_output_maps_to_text_part_and_stop() {
        let interaction = interaction_with(
            "v1_abc",
            InteractionStatus::Completed,
            vec![Step::ModelOutput { content: vec![IxContent::text("Hello, world.")] }],
        );

        let response = to_llm_response(&interaction);

        let content = response.content.expect("completed output should carry content");
        assert_eq!(content.role, "model");
        assert_eq!(content.parts.len(), 1);
        match &content.parts[0] {
            Part::Text { text } => assert_eq!(text, "Hello, world."),
            other => panic!("expected Part::Text, got {other:?}"),
        }
        assert_eq!(response.finish_reason, Some(FinishReason::Stop));
        assert!(response.turn_complete);
        assert!(!response.partial);
        assert_eq!(response.interaction_id.as_deref(), Some("v1_abc"));
    }

    #[test]
    fn requires_action_maps_function_call_with_no_finish_reason() {
        let args = serde_json::json!({ "city": "Boston" });
        let interaction = interaction_with(
            "v1_call",
            InteractionStatus::RequiresAction,
            vec![Step::FunctionCall {
                id: "call-1".to_string(),
                name: "get_weather".to_string(),
                arguments: args.clone(),
                signature: Some("sig-xyz".to_string()),
            }],
        );

        let response = to_llm_response(&interaction);

        let content = response.content.expect("requires_action should carry a function call");
        assert_eq!(content.parts.len(), 1);
        match &content.parts[0] {
            Part::FunctionCall { name, args: call_args, id, thought_signature } => {
                assert_eq!(name, "get_weather");
                assert_eq!(call_args, &args);
                assert_eq!(id.as_deref(), Some("call-1"));
                assert_eq!(thought_signature.as_deref(), Some("sig-xyz"));
            }
            other => panic!("expected Part::FunctionCall, got {other:?}"),
        }
        // requires_action keeps the turn open so the tool loop can run.
        assert_eq!(response.finish_reason, None);
        assert!(!response.turn_complete);
    }

    #[test]
    fn thought_step_maps_to_thinking_part() {
        let interaction = interaction_with(
            "v1_think",
            InteractionStatus::Completed,
            vec![
                Step::Thought {
                    signature: Some("think-sig".to_string()),
                    summary: vec![
                        IxContent::text("First I consider, "),
                        IxContent::text("then I conclude."),
                    ],
                },
                Step::ModelOutput { content: vec![IxContent::text("Answer.")] },
            ],
        );

        let response = to_llm_response(&interaction);
        let content = response.content.expect("should carry content");
        // Thinking part first, then text.
        match &content.parts[0] {
            Part::Thinking { thinking, signature } => {
                assert_eq!(thinking, "First I consider, then I conclude.");
                assert_eq!(signature.as_deref(), Some("think-sig"));
            }
            other => panic!("expected Part::Thinking, got {other:?}"),
        }
        assert!(matches!(&content.parts[1], Part::Text { text } if text == "Answer."));
    }

    #[test]
    fn inline_image_step_maps_to_inline_data_with_bytes() {
        let bytes = vec![0x89, 0x50, 0x4E, 0x47];
        let encoded = BASE64_STANDARD.encode(&bytes);
        let interaction = interaction_with(
            "v1_img",
            InteractionStatus::Completed,
            vec![Step::ModelOutput { content: vec![IxContent::image(encoded, "image/png")] }],
        );

        let response = to_llm_response(&interaction);
        let content = response.content.expect("should carry content");
        match &content.parts[0] {
            Part::InlineData { mime_type, data, .. } => {
                assert_eq!(mime_type, "image/png");
                assert_eq!(data, &bytes);
            }
            other => panic!("expected Part::InlineData, got {other:?}"),
        }
    }

    #[test]
    fn uri_image_step_maps_to_file_data() {
        let interaction = interaction_with(
            "v1_uri",
            InteractionStatus::Completed,
            vec![Step::ModelOutput {
                content: vec![IxContent::Image(ImageContent {
                    data: None,
                    mime_type: Some("image/jpeg".to_string()),
                    uri: Some("https://example.com/cat.jpg".to_string()),
                    resolution: None,
                })],
            }],
        );

        let response = to_llm_response(&interaction);
        let content = response.content.expect("should carry content");
        match &content.parts[0] {
            Part::FileData { mime_type, file_uri, .. } => {
                assert_eq!(mime_type, "image/jpeg");
                assert_eq!(file_uri, "https://example.com/cat.jpg");
            }
            other => panic!("expected Part::FileData, got {other:?}"),
        }
    }

    #[test]
    fn server_tool_step_recorded_in_provider_metadata_not_content() {
        let server_step = serde_json::json!({
            "type": "code_execution_call",
            "code": "print(2 + 2)"
        });
        let interaction = interaction_with(
            "v1_srv",
            InteractionStatus::Completed,
            vec![
                Step::Other(server_step.clone()),
                Step::ModelOutput { content: vec![IxContent::text("The answer is 4.")] },
            ],
        );

        let response = to_llm_response(&interaction);

        // The server step is NOT in the content parts.
        let content = response.content.expect("should carry the model_output text");
        assert_eq!(content.parts.len(), 1);
        assert!(matches!(&content.parts[0], Part::Text { .. }));

        // It IS recorded in provider_metadata.
        let metadata = response.provider_metadata.expect("server steps populate provider_metadata");
        let recorded = &metadata["gemini"]["interaction_steps"];
        assert_eq!(recorded.as_array().map(Vec::len), Some(1));
        assert_eq!(recorded[0], server_step);
        assert_eq!(metadata["gemini"]["status"], "completed");
    }

    #[test]
    fn no_provider_metadata_without_server_steps() {
        let interaction = interaction_with(
            "v1_plain",
            InteractionStatus::Completed,
            vec![Step::ModelOutput { content: vec![IxContent::text("plain")] }],
        );

        let response = to_llm_response(&interaction);
        assert!(response.provider_metadata.is_none());
    }

    #[test]
    fn usage_mapping_preserves_token_counts() {
        let mut interaction = interaction_with(
            "v1_usage",
            InteractionStatus::Completed,
            vec![Step::ModelOutput { content: vec![IxContent::text("counted")] }],
        );
        interaction.usage = Some(Usage {
            total_input_tokens: 120,
            total_output_tokens: 45,
            total_thought_tokens: 17,
            total_cached_tokens: 30,
            total_tool_use_tokens: 5,
            total_tokens: 182,
            input_tokens_by_modality: Vec::new(),
            output_tokens_by_modality: Vec::new(),
        });

        let response = to_llm_response(&interaction);
        let usage = response.usage_metadata.expect("usage should map");
        assert_eq!(usage.prompt_token_count, 120);
        assert_eq!(usage.candidates_token_count, 45);
        assert_eq!(usage.total_token_count, 182);
        assert_eq!(usage.thinking_token_count, Some(17));
        assert_eq!(usage.cache_read_input_token_count, Some(30));
    }

    #[test]
    fn usage_zero_optional_counts_stay_none() {
        let mut interaction = interaction_with(
            "v1_usage_zero",
            InteractionStatus::Completed,
            vec![Step::ModelOutput { content: vec![IxContent::text("x")] }],
        );
        interaction.usage = Some(Usage {
            total_input_tokens: 10,
            total_output_tokens: 5,
            total_tokens: 15,
            ..Default::default()
        });

        let response = to_llm_response(&interaction);
        let usage = response.usage_metadata.expect("usage should map");
        assert_eq!(usage.thinking_token_count, None);
        assert_eq!(usage.cache_read_input_token_count, None);
    }

    #[test]
    fn empty_interaction_id_maps_to_none() {
        let interaction = interaction_with(
            "",
            InteractionStatus::Completed,
            vec![Step::ModelOutput { content: vec![IxContent::text("anon")] }],
        );

        let response = to_llm_response(&interaction);
        assert_eq!(response.interaction_id, None);
    }

    #[test]
    fn terminal_failure_statuses_map_to_total_finish_reasons() {
        for status in [
            InteractionStatus::Failed,
            InteractionStatus::BudgetExceeded,
            InteractionStatus::Cancelled,
        ] {
            let interaction = interaction_with("v1_term", status, Vec::new());
            let response = to_llm_response(&interaction);
            assert_eq!(response.finish_reason, Some(FinishReason::Other));
            assert!(response.turn_complete);
            // No content parts → content is None.
            assert!(response.content.is_none());
        }
    }

    #[test]
    fn incomplete_status_maps_to_max_tokens() {
        let interaction = interaction_with(
            "v1_inc",
            InteractionStatus::Incomplete,
            vec![Step::ModelOutput { content: vec![IxContent::text("partial answer")] }],
        );
        let response = to_llm_response(&interaction);
        assert_eq!(response.finish_reason, Some(FinishReason::MaxTokens));
        assert!(response.turn_complete);
    }

    #[test]
    fn in_progress_status_is_partial() {
        let interaction = interaction_with("v1_prog", InteractionStatus::InProgress, Vec::new());
        let response = to_llm_response(&interaction);
        assert_eq!(response.finish_reason, None);
        assert!(!response.turn_complete);
        assert!(response.partial);
    }

    #[test]
    fn user_input_and_function_result_steps_are_skipped_for_content() {
        let interaction = interaction_with(
            "v1_skip",
            InteractionStatus::Completed,
            vec![
                Step::UserInput { content: vec![IxContent::text("a question")] },
                Step::FunctionResult {
                    call_id: "call-1".to_string(),
                    name: Some("get_weather".to_string()),
                    result: serde_json::json!({ "temp": 72 }),
                    is_error: None,
                    signature: None,
                },
                Step::ModelOutput { content: vec![IxContent::text("the answer")] },
            ],
        );

        let response = to_llm_response(&interaction);
        let content = response.content.expect("should carry only the model output");
        assert_eq!(content.parts.len(), 1);
        assert!(matches!(&content.parts[0], Part::Text { text } if text == "the answer"));
    }

    // ──────────────────────────────────────────────────────────────────
    // Streaming SSE fold (`sse_event_to_chunk`)
    // ──────────────────────────────────────────────────────────────────

    fn created_event(id: &str) -> InteractionSseEvent {
        InteractionSseEvent::InteractionCreated {
            interaction: interaction_with(id, InteractionStatus::InProgress, Vec::new()),
            event_id: None,
        }
    }

    fn text_delta_event(index: i64, text: &str) -> InteractionSseEvent {
        InteractionSseEvent::StepDelta {
            index,
            delta: StepDelta::Text { text: text.to_string() },
            event_id: None,
        }
    }

    fn function_call_delta_event(
        index: i64,
        name: Option<&str>,
        id: Option<&str>,
        arguments_delta: Option<&str>,
    ) -> InteractionSseEvent {
        InteractionSseEvent::StepDelta {
            index,
            delta: StepDelta::FunctionCall {
                arguments_delta: arguments_delta.map(ToString::to_string),
                name: name.map(ToString::to_string),
                id: id.map(ToString::to_string),
            },
            event_id: None,
        }
    }

    fn completed_event(id: &str) -> InteractionSseEvent {
        InteractionSseEvent::InteractionCompleted {
            interaction: interaction_with(id, InteractionStatus::Completed, Vec::new()),
            event_id: None,
        }
    }

    #[test]
    fn sse_created_records_id_without_emitting_a_chunk() {
        let mut acc = SseAccumulator::new();
        let chunk = sse_event_to_chunk(created_event("v1_stream"), &mut acc);
        assert!(chunk.is_none(), "created event should not emit a chunk");
        assert_eq!(acc.interaction_id(), Some("v1_stream"));
    }

    #[test]
    fn sse_status_update_records_id() {
        let mut acc = SseAccumulator::new();
        let event = InteractionSseEvent::InteractionStatusUpdate {
            interaction_id: "v1_status".to_string(),
            status: InteractionStatus::InProgress,
            event_id: None,
        };
        let chunk = sse_event_to_chunk(event, &mut acc);
        assert!(chunk.is_none());
        assert_eq!(acc.interaction_id(), Some("v1_status"));
    }

    #[test]
    fn sse_text_delta_emits_partial_text_chunk_with_id() {
        let mut acc = SseAccumulator::new();
        sse_event_to_chunk(created_event("v1_text"), &mut acc);

        let chunk = sse_event_to_chunk(text_delta_event(0, "Hello"), &mut acc)
            .expect("text delta should emit a chunk")
            .expect("text delta chunk should be Ok");

        assert!(chunk.partial, "text delta should be a partial chunk");
        assert!(!chunk.turn_complete, "text delta should not complete the turn");
        assert_eq!(chunk.interaction_id.as_deref(), Some("v1_text"));
        let content = chunk.content.expect("text delta should carry content");
        assert_eq!(content.role, "model");
        assert!(matches!(&content.parts[0], Part::Text { text } if text == "Hello"));
    }

    #[test]
    fn sse_completed_emits_final_chunk_with_turn_complete_and_id() {
        let mut acc = SseAccumulator::new();
        sse_event_to_chunk(created_event("v1_done"), &mut acc);
        sse_event_to_chunk(text_delta_event(0, "Hi"), &mut acc);

        let chunk = sse_event_to_chunk(completed_event("v1_done"), &mut acc)
            .expect("completed event should emit a chunk")
            .expect("completed chunk should be Ok");

        assert!(!chunk.partial, "completed chunk should not be partial");
        assert!(chunk.turn_complete, "completed chunk should complete the turn");
        assert_eq!(chunk.interaction_id.as_deref(), Some("v1_done"));
        assert_eq!(chunk.finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn sse_error_event_maps_to_err() {
        let mut acc = SseAccumulator::new();
        let event = InteractionSseEvent::Error {
            error: InteractionStreamError {
                message: "model overloaded".to_string(),
                code: Some("RESOURCE_EXHAUSTED".to_string()),
            },
            event_id: None,
        };
        let result =
            sse_event_to_chunk(event, &mut acc).expect("error event should produce a result");
        let err = result.expect_err("error event should map to Err");
        assert_eq!(err.category, ErrorCategory::Internal);
        assert_eq!(err.details.provider.as_deref(), Some("gemini"));
        assert!(err.message.contains("model overloaded"));
        assert!(err.message.contains("RESOURCE_EXHAUSTED"));
    }

    #[test]
    fn sse_function_call_deltas_accumulate_then_flush_on_completion() {
        let mut acc = SseAccumulator::new();
        sse_event_to_chunk(created_event("v1_fn"), &mut acc);

        // First delta carries name + id + opening args fragment.
        assert!(
            sse_event_to_chunk(
                function_call_delta_event(0, Some("get_weather"), Some("call-1"), Some("{\"ci")),
                &mut acc,
            )
            .is_none(),
            "function-call deltas should not emit chunks mid-stream"
        );
        // Subsequent deltas carry only argument fragments.
        assert!(
            sse_event_to_chunk(
                function_call_delta_event(0, None, None, Some("ty\":\"NYC\"}")),
                &mut acc,
            )
            .is_none()
        );

        let chunk = sse_event_to_chunk(completed_event("v1_fn"), &mut acc)
            .expect("completed event should emit a chunk")
            .expect("completed chunk should be Ok");

        let content = chunk.content.expect("completed chunk should carry the function call");
        assert_eq!(content.parts.len(), 1);
        match &content.parts[0] {
            Part::FunctionCall { name, args, id, .. } => {
                assert_eq!(name, "get_weather");
                assert_eq!(id.as_deref(), Some("call-1"));
                assert_eq!(args, &serde_json::json!({ "city": "NYC" }));
            }
            other => panic!("expected Part::FunctionCall, got {other:?}"),
        }
        assert!(!chunk.turn_complete, "tool-call turns must remain open for execution");
        assert_eq!(chunk.interaction_id.as_deref(), Some("v1_fn"));
    }

    #[test]
    fn sse_function_call_with_unparseable_args_falls_back_to_empty_object() {
        let mut acc = SseAccumulator::new();
        sse_event_to_chunk(
            function_call_delta_event(0, Some("do_thing"), Some("call-9"), Some("not json")),
            &mut acc,
        );

        let chunk = sse_event_to_chunk(completed_event("v1_bad_args"), &mut acc)
            .expect("completed event should emit a chunk")
            .expect("completed chunk should be Ok");

        let content = chunk.content.expect("should carry the function call");
        match &content.parts[0] {
            Part::FunctionCall { name, args, .. } => {
                assert_eq!(name, "do_thing");
                assert_eq!(args, &serde_json::json!({}));
            }
            other => panic!("expected Part::FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn sse_multiple_function_calls_flush_in_index_order() {
        let mut acc = SseAccumulator::new();
        // Provide deltas out of index order to confirm deterministic ordering.
        sse_event_to_chunk(
            function_call_delta_event(1, Some("second"), Some("call-2"), Some("{}")),
            &mut acc,
        );
        sse_event_to_chunk(
            function_call_delta_event(0, Some("first"), Some("call-1"), Some("{}")),
            &mut acc,
        );

        let chunk = sse_event_to_chunk(completed_event("v1_multi"), &mut acc)
            .expect("completed event should emit a chunk")
            .expect("completed chunk should be Ok");

        let content = chunk.content.expect("should carry both function calls");
        assert_eq!(content.parts.len(), 2);
        assert!(matches!(&content.parts[0], Part::FunctionCall { name, .. } if name == "first"));
        assert!(matches!(&content.parts[1], Part::FunctionCall { name, .. } if name == "second"));
    }

    #[test]
    fn sse_step_start_stop_and_other_are_ignored() {
        let mut acc = SseAccumulator::new();
        let start = InteractionSseEvent::StepStart {
            index: 0,
            step: Step::ModelOutput { content: Vec::new() },
            event_id: None,
        };
        let stop = InteractionSseEvent::StepStop { index: 0, event_id: None };
        let other = InteractionSseEvent::Other(serde_json::json!({ "event_type": "future.thing" }));

        assert!(sse_event_to_chunk(start, &mut acc).is_none());
        assert!(sse_event_to_chunk(stop, &mut acc).is_none());
        assert!(sse_event_to_chunk(other, &mut acc).is_none());
    }

    #[test]
    fn sse_completed_without_prior_id_uses_interaction_id_from_completed_event() {
        let mut acc = SseAccumulator::new();
        // No created/status events: id must come from the completed event.
        let chunk = sse_event_to_chunk(completed_event("v1_late"), &mut acc)
            .expect("completed event should emit a chunk")
            .expect("completed chunk should be Ok");
        assert_eq!(chunk.interaction_id.as_deref(), Some("v1_late"));
    }

    // ──────────────────────────────────────────────────────────────────
    // Tool-loop call-id round-trip (Property 5 / Requirement 6.4)
    // ──────────────────────────────────────────────────────────────────

    /// **Feature: gemini-interactions-runtime, Property 5: Round-trip call id**
    /// *For any* generated `function_call` id, after mapping to
    /// [`Part::FunctionCall`] and back through a `function_result`, the
    /// `call_id` equals the original.
    /// **Validates: Requirements 6.4**
    ///
    /// This exercises the network-free, deterministic half of the tool loop
    /// end-to-end through the conversion layer, mirroring ADK-Python's
    /// `requires_action` → tool exec → `function_result` cycle:
    ///
    /// 1. A `requires_action` [`Interaction`] carrying a
    ///    [`Step::FunctionCall`] with id `"call-abc"` is mapped by
    ///    [`to_llm_response`] to a [`Part::FunctionCall`] preserving that id
    ///    (this drives the runner's tool loop; the turn stays open).
    /// 2. After the agent runs the tool and appends a
    ///    [`Part::FunctionResponse`] carrying the *same* id, [`build_request`]
    ///    maps it to a [`Step::FunctionResult`] whose `call_id` equals the
    ///    original `function_call` id.
    ///
    /// The id therefore round-trips unchanged end-to-end:
    /// `function_call.id` → `Part::FunctionCall.id` → (tool runs) →
    /// `Part::FunctionResponse.id` → `function_result.call_id`.
    #[test]
    fn function_call_id_round_trips_through_tool_loop() {
        const CALL_ID: &str = "call-abc";
        let args = serde_json::json!({ "city": "Boston" });

        // ── Step 1: server emits requires_action with a function_call ──────
        let interaction = interaction_with(
            "v1_tool_loop",
            InteractionStatus::RequiresAction,
            vec![Step::FunctionCall {
                id: CALL_ID.to_string(),
                name: "get_weather".to_string(),
                arguments: args.clone(),
                signature: None,
            }],
        );

        let response = to_llm_response(&interaction);

        // requires_action keeps the turn open so the runner's tool loop runs.
        assert_eq!(response.finish_reason, None);
        assert!(!response.turn_complete);

        let content = response.content.expect("requires_action should carry a function call");
        assert_eq!(content.parts.len(), 1);
        let call_id = match &content.parts[0] {
            Part::FunctionCall { name, args: call_args, id, .. } => {
                assert_eq!(name, "get_weather");
                assert_eq!(call_args, &args);
                assert_eq!(
                    id.as_deref(),
                    Some(CALL_ID),
                    "the call id must survive to_llm_response"
                );
                id.clone().expect("function call must carry an id")
            }
            other => panic!("expected Part::FunctionCall, got {other:?}"),
        };

        // ── Step 2: the agent runs the tool and continues the conversation ─
        // The next-turn transcript carries the model's function call followed
        // by the function turn's response, both referencing the same id the
        // server assigned. Built statelessly (stateful=false) so the full
        // transcript maps to `Input::Steps`.
        let request = request_with(vec![
            Content::new("user").with_text("What's the weather in Boston?"),
            Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "get_weather".to_string(),
                    args: args.clone(),
                    id: Some(call_id.clone()),
                    thought_signature: None,
                }],
            },
            Content {
                role: "function".to_string(),
                parts: vec![Part::FunctionResponse {
                    function_response: FunctionResponseData::new(
                        "get_weather",
                        serde_json::json!({ "temp": 72, "conditions": "sunny" }),
                    ),
                    id: Some(call_id.clone()),
                    annotations: None,
                }],
            },
        ]);

        let built = build_request(&request, &model_target(), None, false, true)
            .expect("build_request should succeed");

        let steps = match built.input {
            Input::Steps(steps) => steps,
            other => panic!("expected Input::Steps for a multi-turn transcript, got {other:?}"),
        };

        // The function_call id round-tripped into the function_result call_id.
        let result_call_id = steps
            .iter()
            .find_map(|step| match step {
                Step::FunctionResult { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .expect("transcript should contain a function_result step");
        assert_eq!(
            result_call_id, call_id,
            "the function_result call_id must equal the original function_call id"
        );
        assert_eq!(result_call_id, CALL_ID);

        // And the originating function_call step kept the same id.
        let call_step_id = steps
            .iter()
            .find_map(|step| match step {
                Step::FunctionCall { id, .. } => Some(id.clone()),
                _ => None,
            })
            .expect("transcript should contain a function_call step");
        assert_eq!(call_step_id, CALL_ID, "the function_call step must keep the original id");
    }
}
