//! Maps ADK-Rust events to official ACP v1 session updates.

use std::path::PathBuf;

use adk_core::{Content, Event, FunctionResponseData, Part, UsageMetadata};
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Cost, Plan, PlanEntry, SessionUpdate, TextContent, ToolCall,
    ToolCallContent, ToolCallLocation, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
    ToolKind, UsageUpdate,
};

/// Converts the typed event stream produced by the ADK-Rust Runner into ACP
/// `session/update` payloads. The transport sends each returned update as soon
/// as the corresponding Runner event arrives.
pub struct ResponseStreamer;

impl ResponseStreamer {
    /// Convert one ADK event into zero or more ACP updates while preserving the
    /// order of content parts inside the event.
    ///
    /// Content parts are mapped first (message, thought, tool-call lifecycle),
    /// followed by a single [`SessionUpdate::UsageUpdate`] when the event
    /// carries usage metadata. Events without usage metadata produce no usage
    /// update, and reported token counts are never fabricated.
    pub fn map_event(event: &Event) -> Vec<SessionUpdate> {
        let mut updates = Vec::new();
        if let Some(content) = event.content() {
            Self::map_content(content, &mut updates);
        }
        if let Some(usage) = &event.llm_response.usage_metadata {
            updates.push(SessionUpdate::UsageUpdate(map_usage(usage)));
        }
        updates
    }

    fn map_content(content: &Content, updates: &mut Vec<SessionUpdate>) {
        for part in &content.parts {
            match part {
                Part::Text { text } if text.is_empty() => {}
                Part::Text { .. }
                | Part::InlineData { .. }
                | Part::FileData { .. }
                | Part::EmbeddedResource { .. } => {
                    if let Some(block) = crate::content::part_to_block(part) {
                        let chunk = ContentChunk::new(block);
                        if content.role.eq_ignore_ascii_case("user") {
                            updates.push(SessionUpdate::UserMessageChunk(chunk));
                        } else {
                            updates.push(SessionUpdate::AgentMessageChunk(chunk));
                        }
                    }
                }
                Part::Thinking { thinking, .. } if !thinking.is_empty() => {
                    updates.push(SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                        ContentBlock::Text(TextContent::new(thinking.clone())),
                    )));
                }
                Part::FunctionCall { name, args, id, .. } => {
                    let call_id = id.clone().unwrap_or_else(|| format!("{name}-call"));
                    updates.push(SessionUpdate::ToolCall(
                        ToolCall::new(call_id, name.clone())
                            .kind(infer_tool_kind(name))
                            .status(ToolCallStatus::InProgress)
                            .raw_input(args.clone()),
                    ));
                }
                Part::FunctionResponse { function_response, id } => {
                    let call_id =
                        id.clone().unwrap_or_else(|| format!("{}-call", function_response.name));
                    let mut fields = ToolCallUpdateFields::new()
                        .kind(infer_tool_kind(&function_response.name))
                        .status(ToolCallStatus::Completed)
                        .raw_output(function_response.response.clone());
                    let content = tool_result_content(function_response);
                    if !content.is_empty() {
                        fields = fields.content(content);
                    }
                    let locations = tool_result_locations(&function_response.response);
                    if !locations.is_empty() {
                        fields = fields.locations(locations);
                    }
                    updates
                        .push(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(call_id, fields)));
                }
                _ => {}
            }
        }
    }
}

/// Map ADK usage metadata to an ACP [`UsageUpdate`].
///
/// `used` reflects the total tokens consumed for the turn (prompt + response),
/// derived from the reported `total_token_count`. ADK does not report the
/// model's context-window size, so `size` is left at `0` (unknown) rather than
/// fabricating a value. Cost is populated only when ADK reports it; ADK cost is
/// an estimate in USD.
fn map_usage(usage: &UsageMetadata) -> UsageUpdate {
    let used = u64::try_from(usage.total_token_count.max(0)).unwrap_or(0);
    let mut update = UsageUpdate::new(used, 0);
    if let Some(cost) = usage.cost {
        update = update.cost(Cost::new(cost, "USD"));
    }
    update
}

/// Render a tool result as ACP tool-call content.
///
/// A JSON string payload is surfaced verbatim; any other JSON value is rendered
/// as its compact JSON representation. A `null` or empty payload contributes no
/// text block. Inline binary parts and file references are appended as their
/// native ACP image/audio and resource-link blocks.
fn tool_result_content(response: &FunctionResponseData) -> Vec<ToolCallContent> {
    let mut content = Vec::new();
    if !response.response.is_null() {
        let text = match &response.response {
            serde_json::Value::String(value) => value.clone(),
            other => other.to_string(),
        };
        if !text.is_empty() {
            content.push(ToolCallContent::from(ContentBlock::Text(TextContent::new(text))));
        }
    }
    for inline in &response.inline_data {
        let part =
            Part::InlineData { mime_type: inline.mime_type.clone(), data: inline.data.clone() };
        if let Some(block) = crate::content::part_to_block(&part) {
            content.push(ToolCallContent::from(block));
        }
    }
    for file in &response.file_data {
        let part =
            Part::FileData { mime_type: file.mime_type.clone(), file_uri: file.file_uri.clone() };
        if let Some(block) = crate::content::part_to_block(&part) {
            content.push(ToolCallContent::from(block));
        }
    }
    content
}

/// Extract file locations a tool reports affecting from its JSON result.
///
/// Recognizes a top-level `path` string, and `paths`/`locations` arrays whose
/// items are either path strings or objects carrying a `path` string. Anything
/// else is ignored so no location is fabricated.
fn tool_result_locations(response: &serde_json::Value) -> Vec<ToolCallLocation> {
    let mut locations = Vec::new();
    let serde_json::Value::Object(map) = response else {
        return locations;
    };
    if let Some(serde_json::Value::String(path)) = map.get("path") {
        locations.push(ToolCallLocation::new(PathBuf::from(path)));
    }
    for key in ["paths", "locations"] {
        let Some(serde_json::Value::Array(items)) = map.get(key) else {
            continue;
        };
        for item in items {
            match item {
                serde_json::Value::String(path) => {
                    locations.push(ToolCallLocation::new(PathBuf::from(path)));
                }
                serde_json::Value::Object(obj) => {
                    if let Some(serde_json::Value::String(path)) = obj.get("path") {
                        locations.push(ToolCallLocation::new(PathBuf::from(path)));
                    }
                }
                _ => {}
            }
        }
    }
    locations
}

/// Map plan entries surfaced by the ADK runtime into an ACP [`SessionUpdate::Plan`].
///
/// # Dormant extension point
///
/// This helper is **ready but dormant**: ADK has no plan primitive today, so
/// nothing in the runtime produces plan entries and nothing calls this function
/// on the live event path. It exists as the single, documented place to plug a
/// future ADK plan source into the ACP `Plan` `session/update` (Requirement
/// 11.3), rather than inventing a fake plan source now.
///
/// When a future ADK version surfaces plan entries, construct the ACP
/// [`PlanEntry`] values (each carrying a description, [`PlanEntryPriority`], and
/// [`PlanEntryStatus`]) and call `map_plan` to obtain the notification to send.
/// The mapping is pure: it preserves the entries and their order exactly and
/// performs no I/O.
///
/// [`PlanEntry`]: agent_client_protocol::schema::v1::PlanEntry
/// [`PlanEntryPriority`]: agent_client_protocol::schema::v1::PlanEntryPriority
/// [`PlanEntryStatus`]: agent_client_protocol::schema::v1::PlanEntryStatus
pub fn map_plan(entries: Vec<PlanEntry>) -> SessionUpdate {
    SessionUpdate::Plan(Plan::new(entries))
}

/// Infer a conservative ACP [`ToolKind`] from a tool name.
///
/// ADK's Runner event does not carry a tool's declared read-only/behavior flags
/// at the point a result is streamed, so the kind is derived from common naming
/// conventions. Unrecognized names default to [`ToolKind::Other`].
///
/// Shared with the permission bridge so a `session/request_permission` request
/// carries the same tool `kind` a client would see on the corresponding
/// `ToolCall`/`ToolCallUpdate`.
pub(crate) fn infer_tool_kind(name: &str) -> ToolKind {
    let lower = name.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    if has(&["read", "cat", "view", "show", "list", "get"]) {
        ToolKind::Read
    } else if has(&["delete", "remove", "unlink"]) {
        ToolKind::Delete
    } else if has(&["move", "rename"]) {
        ToolKind::Move
    } else if has(&["write", "edit", "update", "create", "patch", "insert", "append"]) {
        ToolKind::Edit
    } else if has(&["search", "grep", "find", "query"]) {
        ToolKind::Search
    } else if has(&["fetch", "download", "http", "curl"]) {
        ToolKind::Fetch
    } else if has(&["exec", "run", "bash", "shell", "command"]) {
        ToolKind::Execute
    } else {
        ToolKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_text_thought_and_tool_lifecycle_in_order() {
        let mut event = Event::new("inv-1");
        let mut content = Content::new("model");
        content
            .parts
            .push(Part::Thinking { thinking: "Inspect the project".into(), signature: None });
        content.parts.push(Part::Text { text: "I will inspect it.".into() });
        content.parts.push(Part::FunctionCall {
            name: "read_file".into(),
            args: serde_json::json!({"path":"src/main.rs"}),
            id: Some("call-1".into()),
            thought_signature: None,
        });
        event.set_content(content);

        let updates = ResponseStreamer::map_event(&event);
        assert_eq!(updates.len(), 3);
        assert!(matches!(updates[0], SessionUpdate::AgentThoughtChunk(_)));
        assert!(matches!(updates[1], SessionUpdate::AgentMessageChunk(_)));
        assert!(matches!(updates[2], SessionUpdate::ToolCall(_)));
    }

    #[test]
    fn maps_text_to_message_chunk_for_its_stored_role() {
        let mut user_event = Event::new("inv-user");
        user_event.set_content(Content::new("user").with_text("stored request"));
        assert_eq!(
            ResponseStreamer::map_event(&user_event),
            vec![SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new("stored request"),
            )))],
        );

        let mut model_event = Event::new("inv-model");
        model_event.set_content(Content::new("model").with_text("stored response"));
        assert_eq!(
            ResponseStreamer::map_event(&model_event),
            vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new("stored response"),
            )))],
        );

        let mut agent_event = Event::new("inv-agent");
        agent_event.set_content(Content::new("agent").with_text("agent response"));
        assert_eq!(
            ResponseStreamer::map_event(&agent_event),
            vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new("agent response"),
            )))],
        );
    }

    #[test]
    fn replays_inline_image_and_audio_with_exact_wire_payloads() {
        use agent_client_protocol::schema::v1::{AudioContent, ImageContent};

        let mut event = Event::new("inv-media");
        let mut content = Content::new("user");
        content.parts.push(Part::InlineData {
            mime_type: "image/png".into(),
            data: vec![0x89, 0x50, 0x4e, 0x47],
        });
        content
            .parts
            .push(Part::InlineData { mime_type: "audio/mpeg".into(), data: vec![1, 2, 3, 4, 5] });
        event.set_content(content);

        assert_eq!(
            ResponseStreamer::map_event(&event),
            vec![
                SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Image(
                    ImageContent::new("iVBORw==", "image/png"),
                ))),
                SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Audio(
                    AudioContent::new("AQIDBAU=", "audio/mpeg"),
                ))),
            ],
        );
    }

    #[test]
    fn replays_embedded_resource_as_role_aware_message_content() {
        use adk_core::{EmbeddedResource, TextResourceContents};
        use agent_client_protocol::schema::v1::{
            EmbeddedResource as AcpEmbeddedResource, EmbeddedResourceResource,
            TextResourceContents as AcpTextResourceContents,
        };

        let mut event = Event::new("inv-resource");
        let mut content = Content::new("user");
        content.parts.push(Part::EmbeddedResource {
            resource: EmbeddedResource::Text(TextResourceContents::new(
                "file:///notes.md",
                Some("text/markdown".into()),
                "# Notes",
            )),
        });
        event.set_content(content);

        assert_eq!(
            ResponseStreamer::map_event(&event),
            vec![SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Resource(
                AcpEmbeddedResource::new(EmbeddedResourceResource::TextResourceContents(
                    AcpTextResourceContents::new("# Notes", "file:///notes.md")
                        .mime_type(Some("text/markdown".into())),
                )),
            )))],
        );
    }

    #[test]
    fn maps_function_response_to_completed_tool_update() {
        let mut event = Event::new("inv-2");
        let mut content = Content::new("function");
        content.parts.push(Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "read_file",
                serde_json::json!({"content":"fn main() {}"}),
            ),
            id: Some("call-1".into()),
        });
        event.set_content(content);

        let updates = ResponseStreamer::map_event(&event);
        assert!(matches!(updates.as_slice(), [SessionUpdate::ToolCallUpdate(_)]));
    }

    #[test]
    fn maps_function_response_media_and_files_to_tool_content() {
        use adk_core::{FileDataPart, InlineDataPart};

        let mut event = Event::new("inv-tool-media");
        let mut content = Content::new("function");
        content.parts.push(Part::FunctionResponse {
            function_response: FunctionResponseData::with_multimodal(
                "render_report",
                serde_json::Value::Null,
                vec![InlineDataPart {
                    mime_type: "image/png".into(),
                    data: vec![0x89, 0x50, 0x4e, 0x47],
                }],
                vec![FileDataPart {
                    mime_type: "application/pdf".into(),
                    file_uri: "https://example.com/reports/result.pdf".into(),
                }],
            ),
            id: Some("call-media".into()),
        });
        event.set_content(content);

        let updates = ResponseStreamer::map_event(&event);
        let [SessionUpdate::ToolCallUpdate(update)] = updates.as_slice() else {
            panic!("expected one tool-call update, got {updates:?}");
        };
        let tool_content = update.fields.content.as_ref().expect("multimodal content");
        assert_eq!(tool_content.len(), 2);
        match &tool_content[0] {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Image(image) => {
                    assert_eq!(image.mime_type, "image/png");
                    assert_eq!(image.data, "iVBORw==");
                }
                other => panic!("expected image content, got {other:?}"),
            },
            other => panic!("expected standard tool content, got {other:?}"),
        }
        match &tool_content[1] {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::ResourceLink(link) => {
                    assert_eq!(link.name, "result.pdf");
                    assert_eq!(link.uri, "https://example.com/reports/result.pdf");
                    assert_eq!(link.mime_type.as_deref(), Some("application/pdf"));
                }
                other => panic!("expected resource-link content, got {other:?}"),
            },
            other => panic!("expected standard tool content, got {other:?}"),
        }
    }

    /// **Feature: acp-v1-full-support, Property 5: Usage fidelity**
    /// *For any* event carrying usage metadata, exactly one `UsageUpdate` is
    /// emitted reflecting the reported token counts (and cost when present).
    /// **Validates: Requirements 3.1**
    #[test]
    fn emits_usage_update_reflecting_reported_counts() {
        let mut event = Event::new("inv-usage");
        event.llm_response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 100,
            candidates_token_count: 50,
            total_token_count: 150,
            cost: Some(0.0025),
            ..Default::default()
        });

        let updates = ResponseStreamer::map_event(&event);
        match updates.as_slice() {
            [SessionUpdate::UsageUpdate(usage)] => {
                assert_eq!(usage.used, 150);
                assert_eq!(usage.size, 0);
                let cost = usage.cost.as_ref().expect("cost present");
                assert_eq!(cost.currency, "USD");
                assert!((cost.amount - 0.0025).abs() < f64::EPSILON);
            }
            other => panic!("expected a single usage update, got {other:?}"),
        }
    }

    /// **Feature: acp-v1-full-support, Property 5: Usage fidelity**
    /// *For any* event without usage metadata, no `UsageUpdate` is emitted.
    /// **Validates: Requirements 3.2**
    #[test]
    fn emits_no_usage_update_when_metadata_absent() {
        let mut event = Event::new("inv-none");
        let mut content = Content::new("model");
        content.parts.push(Part::Text { text: "hi".into() });
        event.set_content(content);

        let updates = ResponseStreamer::map_event(&event);
        assert!(updates.iter().all(|update| !matches!(update, SessionUpdate::UsageUpdate(_))));
    }

    #[test]
    fn usage_update_omits_cost_when_absent() {
        let mut event = Event::new("inv-usage-nocost");
        event.llm_response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 10,
            candidates_token_count: 5,
            total_token_count: 15,
            ..Default::default()
        });

        let updates = ResponseStreamer::map_event(&event);
        match updates.as_slice() {
            [SessionUpdate::UsageUpdate(usage)] => {
                assert_eq!(usage.used, 15);
                assert!(usage.cost.is_none());
            }
            other => panic!("expected a single usage update, got {other:?}"),
        }
    }

    /// **Feature: acp-v1-full-support, Property 6: Tool-call correlation**
    /// *For any* tool call, its `ToolCall` and later `ToolCallUpdate` share the
    /// same identifier.
    /// **Validates: Requirements 4.4**
    #[test]
    fn tool_call_and_update_share_the_same_id() {
        let call_id = "call-xyz";

        let mut call_event = Event::new("inv-corr");
        let mut call_content = Content::new("model");
        call_content.parts.push(Part::FunctionCall {
            name: "read_file".into(),
            args: serde_json::json!({"path":"src/lib.rs"}),
            id: Some(call_id.into()),
            thought_signature: None,
        });
        call_event.set_content(call_content);

        let mut result_event = Event::new("inv-corr");
        let mut result_content = Content::new("function");
        result_content.parts.push(Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "read_file",
                serde_json::json!({"path":"src/lib.rs","content":"fn main() {}"}),
            ),
            id: Some(call_id.into()),
        });
        result_event.set_content(result_content);

        let tool_call_id = match ResponseStreamer::map_event(&call_event).as_slice() {
            [SessionUpdate::ToolCall(call)] => call.tool_call_id.clone(),
            other => panic!("expected a tool call, got {other:?}"),
        };
        let update_id = match ResponseStreamer::map_event(&result_event).as_slice() {
            [SessionUpdate::ToolCallUpdate(update)] => update.tool_call_id.clone(),
            other => panic!("expected a tool call update, got {other:?}"),
        };

        assert_eq!(tool_call_id, update_id);
    }

    #[test]
    fn tool_call_update_is_enriched_with_content_locations_and_kind() {
        let mut event = Event::new("inv-rich");
        let mut content = Content::new("function");
        content.parts.push(Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "read_file",
                serde_json::json!({"path":"src/main.rs","content":"fn main() {}"}),
            ),
            id: Some("call-1".into()),
        });
        event.set_content(content);

        let updates = ResponseStreamer::map_event(&event);
        match updates.as_slice() {
            [SessionUpdate::ToolCallUpdate(update)] => {
                assert_eq!(update.fields.kind, Some(ToolKind::Read));
                assert!(!update.fields.content.as_ref().expect("content set").is_empty());
                let locations = update.fields.locations.as_ref().expect("locations set");
                assert_eq!(locations, &vec![ToolCallLocation::new(PathBuf::from("src/main.rs"))]);
            }
            other => panic!("expected an enriched tool call update, got {other:?}"),
        }
    }

    #[test]
    fn extracts_multiple_locations_from_paths_array() {
        let mut event = Event::new("inv-paths");
        let mut content = Content::new("function");
        content.parts.push(Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "edit_files",
                serde_json::json!({"paths":["a.rs","b.rs"]}),
            ),
            id: Some("call-2".into()),
        });
        event.set_content(content);

        match ResponseStreamer::map_event(&event).as_slice() {
            [SessionUpdate::ToolCallUpdate(update)] => {
                let locations = update.fields.locations.as_ref().expect("locations set");
                assert_eq!(
                    locations,
                    &vec![
                        ToolCallLocation::new(PathBuf::from("a.rs")),
                        ToolCallLocation::new(PathBuf::from("b.rs")),
                    ]
                );
            }
            other => panic!("expected a tool call update, got {other:?}"),
        }
    }

    #[test]
    fn function_call_tool_kind_is_inferred() {
        let mut event = Event::new("inv-kind");
        let mut content = Content::new("model");
        content.parts.push(Part::FunctionCall {
            name: "write_file".into(),
            args: serde_json::json!({}),
            id: Some("c1".into()),
            thought_signature: None,
        });
        event.set_content(content);

        match ResponseStreamer::map_event(&event).as_slice() {
            [SessionUpdate::ToolCall(call)] => assert_eq!(call.kind, ToolKind::Edit),
            other => panic!("expected a tool call, got {other:?}"),
        }
    }

    /// The dormant `map_plan` extension point purely wraps plan entries into a
    /// `SessionUpdate::Plan`, preserving the entries and their order. Nothing
    /// emits it live (ADK has no plan primitive), but the mapping is verified so
    /// a future plan source can rely on it.
    ///
    /// **Validates: Requirements 11.3**
    #[test]
    fn map_plan_wraps_entries_preserving_order() {
        use agent_client_protocol::schema::v1::{PlanEntryPriority, PlanEntryStatus};

        let entries = vec![
            PlanEntry::new(
                "Investigate the failing test",
                PlanEntryPriority::High,
                PlanEntryStatus::InProgress,
            ),
            PlanEntry::new("Write the fix", PlanEntryPriority::Medium, PlanEntryStatus::Pending),
            PlanEntry::new("Document the change", PlanEntryPriority::Low, PlanEntryStatus::Pending),
        ];

        match map_plan(entries.clone()) {
            SessionUpdate::Plan(plan) => assert_eq!(plan.entries, entries),
            other => panic!("expected a plan update, got {other:?}"),
        }
    }

    #[test]
    fn map_plan_of_empty_entries_yields_empty_plan() {
        match map_plan(Vec::new()) {
            SessionUpdate::Plan(plan) => assert!(plan.entries.is_empty()),
            other => panic!("expected a plan update, got {other:?}"),
        }
    }

    #[test]
    fn tool_kind_inference_covers_common_conventions() {
        assert_eq!(infer_tool_kind("read_file"), ToolKind::Read);
        assert_eq!(infer_tool_kind("delete_file"), ToolKind::Delete);
        assert_eq!(infer_tool_kind("rename_path"), ToolKind::Move);
        assert_eq!(infer_tool_kind("edit_file"), ToolKind::Edit);
        assert_eq!(infer_tool_kind("grep_search"), ToolKind::Search);
        assert_eq!(infer_tool_kind("fetch_url"), ToolKind::Fetch);
        assert_eq!(infer_tool_kind("run_command"), ToolKind::Execute);
        assert_eq!(infer_tool_kind("summarize"), ToolKind::Other);
    }
}
