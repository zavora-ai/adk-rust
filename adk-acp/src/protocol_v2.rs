//! Adapters between adk-acp's protocol v1 vocabulary and draft ACP protocol v2.
//!
//! adk-acp keeps one implementation of each side: the server's session handler and the
//! client's public API speak the v1 schema. These functions translate the values that cross
//! the v2 wire at those boundaries. They are not general schema conversions: each one covers
//! the values adk-acp itself produces or consumes, and drops (returning `None`) what has no
//! counterpart.
//!
//! Content blocks, tool-call content and system prompts have the same wire shape in both
//! versions, so they convert through JSON. Configuration options and commands differ in field
//! names and tags, so they convert explicitly.
//!
//! Enabled by the `unstable-protocol-v2` feature.

use std::path::PathBuf;

use agent_client_protocol::v1::schema as v1;
use agent_client_protocol::v2::schema as v2;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Convert a value through its JSON form, for types whose wire shape both versions share.
pub(crate) fn convert_json<T: Serialize, U: DeserializeOwned>(value: &T) -> Option<U> {
    serde_json::to_value(value).ok().and_then(|json| serde_json::from_value(json).ok())
}

/// A v1 content block as v2. Text, image, audio, resource links and embedded resources have
/// the same shape in both versions.
pub(crate) fn block_to_v2(block: &v1::ContentBlock) -> Option<v2::ContentBlock> {
    convert_json(block)
}

/// A v2 content block as v1, or `None` for a block kind v1 does not have.
pub(crate) fn block_to_v1(block: &v2::ContentBlock) -> Option<v1::ContentBlock> {
    if matches!(block, v2::ContentBlock::Other(_)) {
        return None;
    }
    convert_json(block)
}

/// v1 content blocks as v2, dropping none: fails when a block has no v2 form.
pub(crate) fn blocks_to_v2(blocks: &[v1::ContentBlock]) -> Option<Vec<v2::ContentBlock>> {
    blocks.iter().map(block_to_v2).collect()
}

/// v2 content blocks as v1: fails when a block has no v1 form.
pub(crate) fn blocks_to_v1(blocks: &[v2::ContentBlock]) -> Option<Vec<v1::ContentBlock>> {
    blocks.iter().map(block_to_v1).collect()
}

/// A v2 system prompt as v1, or `None` when its mode or content has no v1 form.
pub(crate) fn system_prompt_to_v1(prompt: &v2::SystemPrompt) -> Option<v1::SystemPrompt> {
    convert_json(prompt)
}

/// A v1 system prompt as v2.
pub(crate) fn system_prompt_to_v2(prompt: &v1::SystemPrompt) -> Option<v2::SystemPrompt> {
    convert_json(prompt)
}

/// A v2 MCP server declaration as v1. Stdio and HTTP have v1 forms; an ACP-transport or
/// unknown declaration does not.
pub(crate) fn mcp_server_to_v1(server: &v2::McpServer) -> Option<v1::McpServer> {
    match server {
        v2::McpServer::Stdio(stdio) => Some(v1::McpServer::Stdio(
            v1::McpServerStdio::new(stdio.name.clone(), stdio.command.0.clone())
                .args(stdio.args.clone())
                .env(
                    stdio
                        .env
                        .iter()
                        .map(|variable| {
                            v1::EnvVariable::new(variable.name.clone(), variable.value.clone())
                        })
                        .collect(),
                ),
        )),
        v2::McpServer::Http(http) => Some(v1::McpServer::Http(
            v1::McpServerHttp::new(http.name.clone(), http.url.clone()).headers(
                http.headers
                    .iter()
                    .map(|header| v1::HttpHeader::new(header.name.clone(), header.value.clone()))
                    .collect(),
            ),
        )),
        _ => None,
    }
}

/// A v1 MCP server declaration as v2. v2 has no SSE transport.
pub(crate) fn mcp_server_to_v2(server: &v1::McpServer) -> Option<v2::McpServer> {
    match server {
        v1::McpServer::Stdio(stdio) => Some(v2::McpServer::Stdio(
            v2::McpServerStdio::new(stdio.name.clone(), stdio.command.clone())
                .args(stdio.args.clone())
                .env(
                    stdio
                        .env
                        .iter()
                        .map(|variable| {
                            v2::EnvVariable::new(variable.name.clone(), variable.value.clone())
                        })
                        .collect(),
                ),
        )),
        v1::McpServer::Http(http) => Some(v2::McpServer::Http(
            v2::McpServerHttp::new(http.name.clone(), http.url.clone()).headers(
                http.headers
                    .iter()
                    .map(|header| v2::HttpHeader::new(header.name.clone(), header.value.clone()))
                    .collect(),
            ),
        )),
        _ => None,
    }
}

/// Rename the keys v1 and v2 spell differently in a configuration option's JSON form: the
/// option's `id`/`configId` and a select group's `group`/`groupId`.
fn rename_config_keys(
    mut json: serde_json::Value,
    option_key: (&str, &str),
    group_key: (&str, &str),
) -> serde_json::Value {
    fn rename(object: &mut serde_json::Map<String, serde_json::Value>, from: &str, to: &str) {
        if let Some(value) = object.remove(from) {
            object.insert(to.to_string(), value);
        }
    }
    if let Some(option) = json.as_object_mut() {
        rename(option, option_key.0, option_key.1);
        if let Some(serde_json::Value::Array(options)) = option.get_mut("options") {
            for entry in options {
                // A group carries its own `options`; an ungrouped value does not.
                if let Some(group) = entry.as_object_mut()
                    && group.contains_key("options")
                {
                    rename(group, group_key.0, group_key.1);
                }
            }
        }
    }
    json
}

/// A v1 configuration option as v2.
pub(crate) fn config_option_to_v2(
    option: &v1::SessionConfigOption,
) -> Option<v2::SessionConfigOption> {
    let json = serde_json::to_value(option).ok()?;
    serde_json::from_value(rename_config_keys(json, ("id", "configId"), ("group", "groupId"))).ok()
}

/// A v2 configuration option as v1, or `None` for an option kind v1 does not have.
pub(crate) fn config_option_to_v1(
    option: &v2::SessionConfigOption,
) -> Option<v1::SessionConfigOption> {
    if matches!(option.kind, v2::SessionConfigKind::Other(_)) {
        return None;
    }
    let json = serde_json::to_value(option).ok()?;
    serde_json::from_value(rename_config_keys(json, ("configId", "id"), ("groupId", "group"))).ok()
}

/// v1 configuration options as v2, leaving out any without a v2 form.
pub(crate) fn config_options_to_v2(
    options: &[v1::SessionConfigOption],
) -> Vec<v2::SessionConfigOption> {
    options.iter().filter_map(config_option_to_v2).collect()
}

/// v2 configuration options as v1, leaving out kinds v1 does not have.
pub(crate) fn config_options_to_v1(
    options: &[v2::SessionConfigOption],
) -> Vec<v1::SessionConfigOption> {
    options.iter().filter_map(config_option_to_v1).collect()
}

/// A v2 configuration value as v1, or `None` for a value kind v1 does not have.
pub(crate) fn config_value_to_v1(
    value: &v2::SessionConfigOptionValue,
) -> Option<v1::SessionConfigOptionValue> {
    match value {
        v2::SessionConfigOptionValue::Id { value } => {
            Some(v1::SessionConfigOptionValue::value_id(value.0.to_string()))
        }
        v2::SessionConfigOptionValue::Boolean { value } => {
            Some(v1::SessionConfigOptionValue::Boolean { value: *value })
        }
        _ => None,
    }
}

/// A v1 configuration value as v2.
pub(crate) fn config_value_to_v2(
    value: &v1::SessionConfigOptionValue,
) -> Option<v2::SessionConfigOptionValue> {
    match value {
        v1::SessionConfigOptionValue::ValueId { value } => {
            Some(v2::SessionConfigOptionValue::Id { value: value.0.to_string().into() })
        }
        v1::SessionConfigOptionValue::Boolean { value } => {
            Some(v2::SessionConfigOptionValue::Boolean { value: *value })
        }
        _ => None,
    }
}

/// v2 initial configuration values as v1: fails when a value has no v1 form.
pub(crate) fn initial_config_to_v1(
    initial: &[v2::InitialSessionConfigOption],
) -> Option<Vec<v1::InitialSessionConfigOption>> {
    initial
        .iter()
        .map(|option| {
            config_value_to_v1(&option.value).map(|value| {
                v1::InitialSessionConfigOption::new(option.config_id.0.to_string(), value)
            })
        })
        .collect()
}

/// v1 initial configuration values as v2.
pub(crate) fn initial_config_to_v2(
    initial: &[v1::InitialSessionConfigOption],
) -> Option<Vec<v2::InitialSessionConfigOption>> {
    initial
        .iter()
        .map(|option| {
            config_value_to_v2(&option.value).map(|value| {
                v2::InitialSessionConfigOption::new(option.config_id.0.to_string(), value)
            })
        })
        .collect()
}

/// A v1 command as v2.
pub(crate) fn command_to_v2(command: &v1::AvailableCommand) -> v2::AvailableCommand {
    let input = command.input.as_ref().and_then(|input| match input {
        v1::AvailableCommandInput::Unstructured(input) => {
            Some(v2::AvailableCommandInput::Text(v2::TextCommandInput::new(input.hint.clone())))
        }
        _ => None,
    });
    v2::AvailableCommand::Command(
        v2::Command::new(command.name.clone(), command.description.clone()).input(input),
    )
}

/// A v2 command as v1. A command kind v1 does not know is kept by name and description, as
/// the protocol asks clients to show it.
pub(crate) fn command_to_v1(command: &v2::AvailableCommand) -> v1::AvailableCommand {
    let input = match command {
        v2::AvailableCommand::Command(command) => {
            command.input.as_ref().and_then(|input| match input {
                v2::AvailableCommandInput::Text(text) => {
                    Some(v1::AvailableCommandInput::Unstructured(
                        v1::UnstructuredCommandInput::new(text.hint.clone()),
                    ))
                }
                _ => None,
            })
        }
        _ => None,
    };
    v1::AvailableCommand::new(command.name(), command.description()).input(input)
}

/// v2 session info as v1.
pub(crate) fn session_info_to_v1(info: &v2::SessionInfo) -> v1::SessionInfo {
    v1::SessionInfo::new(info.session_id.0.to_string(), info.cwd.0.clone())
        .additional_directories(
            info.additional_directories.iter().map(|directory| directory.0.clone()).collect(),
        )
        .title(info.title.clone())
        .updated_at(info.updated_at.clone())
        .archived(info.archived)
}

/// v1 session info as v2.
pub(crate) fn session_info_to_v2(info: &v1::SessionInfo) -> v2::SessionInfo {
    v2::SessionInfo::new(info.session_id.0.to_string(), info.cwd.clone())
        .additional_directories(info.additional_directories.iter().cloned())
        .title(info.title.clone())
        .updated_at(info.updated_at.clone())
        .archived(info.archived)
}

/// v1 paths as v2 absolute paths.
pub(crate) fn paths_to_v2(paths: &[PathBuf]) -> Vec<v2::AbsolutePath> {
    paths.iter().cloned().map(v2::AbsolutePath::new).collect()
}

/// v2 absolute paths as v1 paths.
pub(crate) fn paths_to_v1(paths: &[v2::AbsolutePath]) -> Vec<PathBuf> {
    paths.iter().map(|path| path.0.clone()).collect()
}

/// A v1 tool kind as v2.
pub(crate) fn tool_kind_to_v2(kind: v1::ToolKind) -> v2::ToolKind {
    match kind {
        v1::ToolKind::Read => v2::ToolKind::Read,
        v1::ToolKind::Edit => v2::ToolKind::Edit,
        v1::ToolKind::Delete => v2::ToolKind::Delete,
        v1::ToolKind::Move => v2::ToolKind::Move,
        v1::ToolKind::Search => v2::ToolKind::Search,
        v1::ToolKind::Execute => v2::ToolKind::Execute,
        v1::ToolKind::Think => v2::ToolKind::Think,
        v1::ToolKind::Fetch => v2::ToolKind::Fetch,
        v1::ToolKind::SwitchMode => v2::ToolKind::SwitchMode,
        _ => v2::ToolKind::Other,
    }
}

/// A v2 tool kind as v1. An unknown kind shows as `other`.
pub(crate) fn tool_kind_to_v1(kind: &v2::ToolKind) -> v1::ToolKind {
    match kind {
        v2::ToolKind::Read => v1::ToolKind::Read,
        v2::ToolKind::Edit => v1::ToolKind::Edit,
        v2::ToolKind::Delete => v1::ToolKind::Delete,
        v2::ToolKind::Move => v1::ToolKind::Move,
        v2::ToolKind::Search => v1::ToolKind::Search,
        v2::ToolKind::Execute => v1::ToolKind::Execute,
        v2::ToolKind::Think => v1::ToolKind::Think,
        v2::ToolKind::Fetch => v1::ToolKind::Fetch,
        v2::ToolKind::SwitchMode => v1::ToolKind::SwitchMode,
        _ => v1::ToolKind::Other,
    }
}

/// A v1 tool-call status as v2.
pub(crate) fn tool_status_to_v2(status: v1::ToolCallStatus) -> v2::ToolCallStatus {
    match status {
        v1::ToolCallStatus::Pending => v2::ToolCallStatus::Pending,
        v1::ToolCallStatus::InProgress => v2::ToolCallStatus::InProgress,
        v1::ToolCallStatus::Completed => v2::ToolCallStatus::Completed,
        v1::ToolCallStatus::Failed => v2::ToolCallStatus::Failed,
        _ => v2::ToolCallStatus::InProgress,
    }
}

/// A v2 tool-call status as v1. v1 has no `paused` or `cancelled`: a paused call is still in
/// progress and a cancelled one has failed. An unknown status is treated as in progress, as
/// the protocol asks.
pub(crate) fn tool_status_to_v1(status: &v2::ToolCallStatus) -> v1::ToolCallStatus {
    match status {
        v2::ToolCallStatus::Pending => v1::ToolCallStatus::Pending,
        v2::ToolCallStatus::Completed => v1::ToolCallStatus::Completed,
        v2::ToolCallStatus::Failed | v2::ToolCallStatus::Cancelled => v1::ToolCallStatus::Failed,
        _ => v1::ToolCallStatus::InProgress,
    }
}

/// Whether a v2 tool-call status is final.
pub(crate) fn tool_status_is_final(status: &v2::ToolCallStatus) -> bool {
    matches!(
        status,
        v2::ToolCallStatus::Completed | v2::ToolCallStatus::Failed | v2::ToolCallStatus::Cancelled
    )
}

/// v1 tool-call content as v2. Content blocks carry over; diffs and terminals adk-acp's
/// server never produces are left out.
fn tool_content_to_v2(content: &[v1::ToolCallContent]) -> Vec<v2::ToolCallContent> {
    content
        .iter()
        .filter_map(|item| match item {
            v1::ToolCallContent::Content(content) => block_to_v2(&content.content)
                .map(|block| v2::ToolCallContent::Content(Box::new(v2::Content::new(block)))),
            _ => None,
        })
        .collect()
}

/// v2 tool-call content as v1: content blocks only.
fn tool_content_to_v1(content: &[v2::ToolCallContent]) -> Vec<v1::ToolCallContent> {
    content
        .iter()
        .filter_map(|item| match item {
            v2::ToolCallContent::Content(content) => block_to_v1(&content.content)
                .map(|block| v1::ToolCallContent::Content(v1::Content::new(block))),
            _ => None,
        })
        .collect()
}

/// v1 locations as v2. v2 locations must be absolute, so relative ones are left out.
fn locations_to_v2(locations: &[v1::ToolCallLocation]) -> Vec<v2::ToolCallLocation> {
    locations
        .iter()
        .filter(|location| location.path.is_absolute())
        .map(|location| {
            v2::ToolCallLocation::new(location.path.clone()).line(location.line.unwrap_or(0))
        })
        .collect()
}

/// v2 locations as v1.
fn locations_to_v1(locations: &[v2::ToolCallLocation]) -> Vec<v1::ToolCallLocation> {
    locations
        .iter()
        .map(|location| {
            v1::ToolCallLocation::new(location.path.0.clone())
                .line(location.line.map(std::num::NonZeroU32::get))
        })
        .collect()
}

/// A v1 tool call, as the server's streamer announces it, as a v2 tool-call upsert.
pub(crate) fn tool_call_to_v2(call: &v1::ToolCall) -> v2::ToolCallUpdate {
    let mut update = v2::ToolCallUpdate::new(call.tool_call_id.0.to_string())
        .title(call.title.clone())
        .kind(tool_kind_to_v2(call.kind))
        .status(tool_status_to_v2(call.status));
    if let Some(name) = &call.name {
        update = update.name(name.clone());
    }
    if !call.content.is_empty() {
        update = update.content(tool_content_to_v2(&call.content));
    }
    if !call.locations.is_empty() {
        update = update.locations(locations_to_v2(&call.locations));
    }
    if let Some(input) = &call.raw_input {
        update = update.raw_input(input.clone());
    }
    if let Some(output) = &call.raw_output {
        update = update.raw_output(output.clone());
    }
    update
}

/// A v1 tool-call update as a v2 upsert carrying only the fields the update sets.
pub(crate) fn tool_call_update_to_v2(update: &v1::ToolCallUpdate) -> v2::ToolCallUpdate {
    let fields = &update.fields;
    let mut upsert = v2::ToolCallUpdate::new(update.tool_call_id.0.to_string());
    if let Some(title) = &fields.title {
        upsert = upsert.title(title.clone());
    }
    if let Some(kind) = fields.kind {
        upsert = upsert.kind(tool_kind_to_v2(kind));
    }
    if let Some(status) = fields.status {
        upsert = upsert.status(tool_status_to_v2(status));
    }
    if let Some(content) = &fields.content {
        upsert = upsert.content(tool_content_to_v2(content));
    }
    if let Some(locations) = &fields.locations {
        upsert = upsert.locations(locations_to_v2(locations));
    }
    if let Some(input) = &fields.raw_input {
        upsert = upsert.raw_input(input.clone());
    }
    if let Some(output) = &fields.raw_output {
        upsert = upsert.raw_output(output.clone());
    }
    upsert
}

/// A v2 tool-call upsert as a v1 update. `first_seen` makes it a v1 `tool_call`
/// announcement, with v2's defaults for the fields the upsert leaves out.
pub(crate) fn tool_call_upsert_to_v1(
    upsert: &v2::ToolCallUpdate,
    first_seen: bool,
) -> v1::SessionUpdate {
    let id = upsert.tool_call_id.0.to_string();
    let title = upsert.title.value().cloned();
    let kind = upsert.kind.value().map(tool_kind_to_v1);
    let status = upsert.status.value().map(tool_status_to_v1);
    let content = upsert.content.value().map(|content| tool_content_to_v1(content));
    let locations = upsert.locations.value().map(|locations| locations_to_v1(locations));
    let raw_input = upsert.raw_input.value().cloned();
    let raw_output = upsert.raw_output.value().cloned();
    if first_seen {
        let mut call = v1::ToolCall::new(id, title.unwrap_or_else(|| "Tool call".to_string()))
            .kind(kind.unwrap_or_default())
            .status(status.unwrap_or_default())
            .content(content.unwrap_or_default())
            .locations(locations.unwrap_or_default())
            .raw_input(raw_input)
            .raw_output(raw_output);
        if let Some(name) = upsert.name.value() {
            call = call.name(name.clone());
        }
        return v1::SessionUpdate::ToolCall(call);
    }
    let mut fields = v1::ToolCallUpdateFields::new();
    if let Some(title) = title {
        fields = fields.title(title);
    }
    if let Some(kind) = kind {
        fields = fields.kind(kind);
    }
    if let Some(status) = status {
        fields = fields.status(status);
    }
    if let Some(content) = content {
        fields = fields.content(content);
    }
    if let Some(locations) = locations {
        fields = fields.locations(locations);
    }
    if let Some(input) = raw_input {
        fields = fields.raw_input(input);
    }
    if let Some(output) = raw_output {
        fields = fields.raw_output(output);
    }
    v1::SessionUpdate::ToolCallUpdate(v1::ToolCallUpdate::new(id, fields))
}

/// v1 token usage as v2.
pub(crate) fn usage_to_v2(usage: &v1::Usage) -> v2::Usage {
    v2::Usage::new(usage.total_tokens, usage.input_tokens, usage.output_tokens)
        .thought_tokens(usage.thought_tokens)
        .cached_read_tokens(usage.cached_read_tokens)
        .cached_write_tokens(usage.cached_write_tokens)
}

/// v2 token usage as v1.
pub(crate) fn usage_to_v1(usage: &v2::Usage) -> v1::Usage {
    v1::Usage::new(usage.total_tokens, usage.input_tokens, usage.output_tokens)
        .thought_tokens(usage.thought_tokens)
        .cached_read_tokens(usage.cached_read_tokens)
        .cached_write_tokens(usage.cached_write_tokens)
}

/// A v1 context-usage update as v2.
pub(crate) fn usage_update_to_v2(update: &v1::UsageUpdate) -> v2::UsageUpdate {
    let mut converted = v2::UsageUpdate::new(update.used, update.size);
    if let Some(cost) = &update.cost {
        converted = converted.cost(v2::Cost::new(cost.amount, cost.currency.clone()));
    }
    converted
}

/// A v2 context-usage update as v1.
pub(crate) fn usage_update_to_v1(update: &v2::UsageUpdate) -> v1::UsageUpdate {
    let mut converted = v1::UsageUpdate::new(update.used, update.size);
    if let Some(cost) = update.cost.value() {
        converted = converted.cost(v1::Cost::new(cost.amount, cost.currency.clone()));
    }
    converted
}

/// A v2 stop reason as v1, or `None` for `error` and unknown reasons, which v1 cannot
/// express: a v1 turn that fails ends with an error response instead.
pub(crate) fn stop_reason_to_v1(reason: &v2::StopReason) -> Option<v1::StopReason> {
    match reason {
        v2::StopReason::EndTurn => Some(v1::StopReason::EndTurn),
        v2::StopReason::MaxTokens => Some(v1::StopReason::MaxTokens),
        v2::StopReason::MaxTurnRequests => Some(v1::StopReason::MaxTurnRequests),
        v2::StopReason::Refusal => Some(v1::StopReason::Refusal),
        v2::StopReason::Cancelled => Some(v1::StopReason::Cancelled),
        _ => None,
    }
}

/// A v1 stop reason as v2.
pub(crate) fn stop_reason_to_v2(reason: v1::StopReason) -> v2::StopReason {
    match reason {
        v1::StopReason::MaxTokens => v2::StopReason::MaxTokens,
        v1::StopReason::MaxTurnRequests => v2::StopReason::MaxTurnRequests,
        v1::StopReason::Refusal => v2::StopReason::Refusal,
        v1::StopReason::Cancelled => v2::StopReason::Cancelled,
        _ => v2::StopReason::EndTurn,
    }
}

/// A v1 permission option kind as v2.
pub(crate) fn permission_kind_to_v2(kind: v1::PermissionOptionKind) -> v2::PermissionOptionKind {
    match kind {
        v1::PermissionOptionKind::AllowOnce => v2::PermissionOptionKind::AllowOnce,
        v1::PermissionOptionKind::AllowAlways => v2::PermissionOptionKind::AllowAlways,
        v1::PermissionOptionKind::RejectAlways => v2::PermissionOptionKind::RejectAlways,
        _ => v2::PermissionOptionKind::RejectOnce,
    }
}

/// A v2 permission option kind as v1, or `None` for a kind v1 does not have: such an option
/// is not offered to a v1 policy.
pub(crate) fn permission_kind_to_v1(
    kind: &v2::PermissionOptionKind,
) -> Option<v1::PermissionOptionKind> {
    match kind {
        v2::PermissionOptionKind::AllowOnce => Some(v1::PermissionOptionKind::AllowOnce),
        v2::PermissionOptionKind::AllowAlways => Some(v1::PermissionOptionKind::AllowAlways),
        v2::PermissionOptionKind::RejectOnce => Some(v1::PermissionOptionKind::RejectOnce),
        v2::PermissionOptionKind::RejectAlways => Some(v1::PermissionOptionKind::RejectAlways),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_blocks_round_trip_between_versions() {
        let blocks = vec![
            v1::ContentBlock::from("hello"),
            v1::ContentBlock::ResourceLink(v1::ResourceLink::new("notes", "file:///tmp/notes.md")),
        ];
        let v2_blocks = blocks_to_v2(&blocks).expect("v1 blocks have v2 forms");
        assert_eq!(blocks_to_v1(&v2_blocks).expect("and back"), blocks);
    }

    #[test]
    fn select_and_boolean_config_options_convert_both_ways() {
        let options = vec![
            v1::SessionConfigOption::select(
                "model",
                "Model",
                "fast",
                vec![
                    v1::SessionConfigSelectOption::new("fast", "Fast"),
                    v1::SessionConfigSelectOption::new("deep", "Deep"),
                ],
            ),
            v1::SessionConfigOption::boolean("verbose", "Verbose", true),
        ];
        let converted = config_options_to_v2(&options);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0].config_id.0.as_ref(), "model");
        assert!(matches!(converted[1].kind, v2::SessionConfigKind::Boolean(_)));
        assert_eq!(config_options_to_v1(&converted), options);
    }

    #[test]
    fn config_values_convert_both_ways() {
        let id = v1::SessionConfigOptionValue::value_id("deep");
        let flag = v1::SessionConfigOptionValue::Boolean { value: true };
        for value in [id, flag] {
            let converted = config_value_to_v2(&value).expect("v2 form");
            assert_eq!(config_value_to_v1(&converted), Some(value));
        }
    }

    #[test]
    fn commands_keep_their_name_description_and_hint() {
        let command = v1::AvailableCommand::new("test", "Run the tests").input(
            v1::AvailableCommandInput::Unstructured(v1::UnstructuredCommandInput::new("filter")),
        );
        let converted = command_to_v2(&command);
        assert_eq!(converted.name(), "test");
        assert_eq!(command_to_v1(&converted), command);
    }

    #[test]
    fn tool_calls_carry_their_fields_and_a_first_seen_upsert_announces_a_call() {
        let call = v1::ToolCall::new("call-1", "Run tool 'search'")
            .kind(v1::ToolKind::Search)
            .status(v1::ToolCallStatus::InProgress)
            .raw_input(serde_json::json!({ "q": "acp" }))
            .locations(vec![
                v1::ToolCallLocation::new("/work/src/lib.rs"),
                v1::ToolCallLocation::new("relative.rs"),
            ]);
        let upsert = tool_call_to_v2(&call);
        assert_eq!(upsert.kind.value(), Some(&v2::ToolKind::Search));
        assert_eq!(
            upsert.locations.value().map(Vec::len),
            Some(1),
            "relative locations are left out of v2"
        );
        let v1::SessionUpdate::ToolCall(back) = tool_call_upsert_to_v1(&upsert, true) else {
            panic!("a first-seen upsert announces the call");
        };
        assert_eq!(back.title, call.title);
        assert_eq!(back.raw_input, call.raw_input);

        let finish = v1::ToolCallUpdate::new(
            "call-1",
            v1::ToolCallUpdateFields::new().status(v1::ToolCallStatus::Completed),
        );
        let upsert = tool_call_update_to_v2(&finish);
        assert!(upsert.title.is_undefined(), "fields the update does not set stay unset");
        assert!(tool_status_is_final(upsert.status.value().expect("status")));
        assert!(matches!(
            tool_call_upsert_to_v1(&upsert, false),
            v1::SessionUpdate::ToolCallUpdate(_)
        ));
    }

    #[test]
    fn mcp_servers_convert_except_transports_the_other_version_lacks() {
        let stdio = v1::McpServer::Stdio(
            v1::McpServerStdio::new("tools", "/bin/tools")
                .args(vec!["--serve".into()])
                .env(vec![v1::EnvVariable::new("MODE", "test")]),
        );
        let converted = mcp_server_to_v2(&stdio).expect("stdio has a v2 form");
        assert_eq!(mcp_server_to_v1(&converted), Some(stdio));
        let sse = v1::McpServer::Sse(v1::McpServerSse::new("events", "https://example.test/sse"));
        assert!(mcp_server_to_v2(&sse).is_none(), "v2 has no SSE transport");
    }

    #[test]
    fn stop_reasons_map_and_error_has_no_v1_form() {
        assert_eq!(
            stop_reason_to_v1(&stop_reason_to_v2(v1::StopReason::Cancelled)),
            Some(v1::StopReason::Cancelled)
        );
        assert_eq!(stop_reason_to_v1(&v2::StopReason::Error(v2::ErrorStopReason::new())), None);
    }
}
