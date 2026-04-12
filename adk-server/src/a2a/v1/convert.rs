//! A2A v1.0.0 type conversion layer.
//!
//! Bidirectional conversion between `a2a_protocol_types` wire types and
//! ADK internal types (`adk_core::Part`, `crate::a2a::Message`, etc.).

use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose};
use serde_json::{Map, Value, json};

use super::error::A2aError;
use super::task_store::TaskStoreEntry;

// ── Part conversion ──────────────────────────────────────────────────────────

/// Convert an `a2a_protocol_types::Part` to an `adk_core::Part`.
///
/// Mapping:
/// - `PartContent::Text` → `Part::Text`
/// - `PartContent::Raw` → `Part::InlineData` (base64 decode)
/// - `PartContent::Url` → `Part::FileData`
/// - `PartContent::Data` → `Part::FunctionCall` / `Part::FunctionResponse` /
///   `Part::ServerToolCall` / `Part::ServerToolResponse` based on JSON keys
pub fn wire_part_to_adk(part: &a2a_protocol_types::Part) -> Result<adk_core::Part, A2aError> {
    match &part.content {
        a2a_protocol_types::PartContent::Text(text) => {
            Ok(adk_core::Part::Text { text: text.clone() })
        }
        a2a_protocol_types::PartContent::Raw(base64_str) => {
            let data = general_purpose::STANDARD.decode(base64_str).map_err(|e| {
                A2aError::InvalidParams { message: format!("base64 decode error: {e}") }
            })?;
            Ok(adk_core::Part::InlineData {
                mime_type: part.media_type.clone().unwrap_or_default(),
                data,
            })
        }
        a2a_protocol_types::PartContent::Url(uri) => Ok(adk_core::Part::FileData {
            mime_type: part.media_type.clone().unwrap_or_default(),
            file_uri: uri.clone(),
        }),
        a2a_protocol_types::PartContent::Data(data) => wire_data_to_adk(data),
        _ => {
            Err(A2aError::InvalidParams { message: "unsupported PartContent variant".to_string() })
        }
    }
}

/// Parse a `PartContent::Data` JSON value into the appropriate ADK Part.
fn wire_data_to_adk(data: &Value) -> Result<adk_core::Part, A2aError> {
    if let Some(call) = data.get("function_call") {
        let name = call
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| A2aError::InvalidParams {
                message: "function_call missing 'name'".to_string(),
            })?
            .to_string();
        let args = call.get("args").cloned().unwrap_or(Value::Object(Map::new()));
        let id = call.get("id").and_then(|v| v.as_str()).map(String::from);
        Ok(adk_core::Part::FunctionCall { name, args, id, thought_signature: None })
    } else if let Some(resp) = data.get("function_response") {
        let name = resp
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| A2aError::InvalidParams {
                message: "function_response missing 'name'".to_string(),
            })?
            .to_string();
        let response = resp.get("response").cloned().unwrap_or(Value::Object(Map::new()));
        let id = resp.get("id").and_then(|v| v.as_str()).map(String::from);
        Ok(adk_core::Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData { name, response },
            id,
        })
    } else if let Some(stc) = data.get("server_tool_call") {
        Ok(adk_core::Part::ServerToolCall { server_tool_call: stc.clone() })
    } else if let Some(str_val) = data.get("server_tool_response") {
        Ok(adk_core::Part::ServerToolResponse { server_tool_response: str_val.clone() })
    } else {
        Err(A2aError::InvalidParams {
            message: "data part does not contain a recognized key (function_call, function_response, server_tool_call, server_tool_response)".to_string(),
        })
    }
}

/// Convert an `adk_core::Part` to an `a2a_protocol_types::Part`.
///
/// Mapping:
/// - `Part::Text` → `PartContent::Text`
/// - `Part::InlineData` → `PartContent::Raw` (base64 encode)
/// - `Part::FileData` → `PartContent::Url`
/// - `Part::FunctionCall` → `PartContent::Data` with `function_call` key
/// - `Part::FunctionResponse` → `PartContent::Data` with `function_response` key
/// - `Part::Thinking` → `PartContent::Text` (one-way, loses thinking metadata)
/// - `Part::ServerToolCall` → `PartContent::Data` with `server_tool_call` key
/// - `Part::ServerToolResponse` → `PartContent::Data` with `server_tool_response` key
pub fn adk_part_to_wire(part: &adk_core::Part) -> Result<a2a_protocol_types::Part, A2aError> {
    match part {
        adk_core::Part::Text { text } => Ok(a2a_protocol_types::Part::text(text.clone())),
        adk_core::Part::InlineData { mime_type, data } => {
            let encoded = general_purpose::STANDARD.encode(data);
            Ok(a2a_protocol_types::Part::raw(encoded).with_media_type(mime_type.clone()))
        }
        adk_core::Part::FileData { mime_type, file_uri } => {
            Ok(a2a_protocol_types::Part::url(file_uri.clone()).with_media_type(mime_type.clone()))
        }
        adk_core::Part::FunctionCall { name, args, id, .. } => {
            let mut call_data = Map::new();
            call_data.insert("name".to_string(), Value::String(name.clone()));
            call_data.insert("args".to_string(), args.clone());
            if let Some(call_id) = id {
                call_data.insert("id".to_string(), Value::String(call_id.clone()));
            }
            let data = json!({ "function_call": Value::Object(call_data) });
            Ok(a2a_protocol_types::Part::data(data))
        }
        adk_core::Part::FunctionResponse { function_response, id } => {
            let mut resp_data = Map::new();
            resp_data.insert("name".to_string(), Value::String(function_response.name.clone()));
            resp_data.insert("response".to_string(), function_response.response.clone());
            if let Some(resp_id) = id {
                resp_data.insert("id".to_string(), Value::String(resp_id.clone()));
            }
            let data = json!({ "function_response": Value::Object(resp_data) });
            Ok(a2a_protocol_types::Part::data(data))
        }
        adk_core::Part::Thinking { thinking, .. } => {
            // One-way: thinking metadata (signature) is lost
            Ok(a2a_protocol_types::Part::text(thinking.clone()))
        }
        adk_core::Part::ServerToolCall { server_tool_call } => {
            let data = json!({ "server_tool_call": server_tool_call.clone() });
            Ok(a2a_protocol_types::Part::data(data))
        }
        adk_core::Part::ServerToolResponse { server_tool_response } => {
            let data = json!({ "server_tool_response": server_tool_response.clone() });
            Ok(a2a_protocol_types::Part::data(data))
        }
    }
}

// ── Message conversion ───────────────────────────────────────────────────────

/// Convert an `a2a_protocol_types::Message` to the internal `crate::a2a::Message`.
///
/// Preserves message_id, role, parts, task_id, context_id, and metadata.
/// Extensions and reference_task_ids are stored in metadata under
/// `"_a2a_extensions"` and `"_a2a_reference_task_ids"` keys.
pub fn wire_message_to_adk(
    msg: &a2a_protocol_types::Message,
) -> Result<crate::a2a::Message, A2aError> {
    let role = match msg.role {
        a2a_protocol_types::MessageRole::User => crate::a2a::Role::User,
        a2a_protocol_types::MessageRole::Agent => crate::a2a::Role::Agent,
        _ => crate::a2a::Role::User, // Unspecified defaults to User
    };

    let parts: Vec<crate::a2a::Part> =
        msg.parts.iter().map(wire_part_to_adk_internal).collect::<Result<Vec<_>, _>>()?;

    // Build metadata, merging wire metadata with extensions/reference_task_ids
    let mut metadata: Option<Map<String, Value>> =
        msg.metadata.as_ref().and_then(|v| v.as_object().cloned());

    // Preserve extensions in metadata
    if let Some(ref extensions) = msg.extensions {
        let meta = metadata.get_or_insert_with(Map::new);
        meta.insert(
            "_a2a_extensions".to_string(),
            serde_json::to_value(extensions).unwrap_or(Value::Null),
        );
    }

    // Preserve reference_task_ids in metadata
    if let Some(ref ref_ids) = msg.reference_task_ids {
        let meta = metadata.get_or_insert_with(Map::new);
        let ids: Vec<String> = ref_ids.iter().map(|id| id.0.clone()).collect();
        meta.insert(
            "_a2a_reference_task_ids".to_string(),
            serde_json::to_value(ids).unwrap_or(Value::Null),
        );
    }

    Ok(crate::a2a::Message {
        role,
        parts,
        metadata,
        message_id: msg.id.0.clone(),
        task_id: msg.task_id.as_ref().map(|id| id.0.clone()),
        context_id: msg.context_id.as_ref().map(|id| id.0.clone()),
    })
}

/// Convert a wire Part to the internal a2a Part (v0.3 format).
fn wire_part_to_adk_internal(
    part: &a2a_protocol_types::Part,
) -> Result<crate::a2a::Part, A2aError> {
    match &part.content {
        a2a_protocol_types::PartContent::Text(text) => {
            Ok(crate::a2a::Part::Text { text: text.clone(), metadata: None })
        }
        a2a_protocol_types::PartContent::Raw(base64_str) => Ok(crate::a2a::Part::File {
            file: crate::a2a::FileContent {
                name: part.filename.clone(),
                mime_type: part.media_type.clone(),
                bytes: Some(base64_str.clone()),
                uri: None,
            },
            metadata: None,
        }),
        a2a_protocol_types::PartContent::Url(uri) => Ok(crate::a2a::Part::File {
            file: crate::a2a::FileContent {
                name: part.filename.clone(),
                mime_type: part.media_type.clone(),
                bytes: None,
                uri: Some(uri.clone()),
            },
            metadata: None,
        }),
        a2a_protocol_types::PartContent::Data(data) => {
            let obj = data.as_object().cloned().unwrap_or_default();
            Ok(crate::a2a::Part::Data { data: obj, metadata: None })
        }
        _ => {
            Err(A2aError::InvalidParams { message: "unsupported PartContent variant".to_string() })
        }
    }
}

/// Convert the internal `crate::a2a::Message` to an `a2a_protocol_types::Message`.
pub fn adk_message_to_wire(
    msg: &crate::a2a::Message,
) -> Result<a2a_protocol_types::Message, A2aError> {
    let role = match msg.role {
        crate::a2a::Role::User => a2a_protocol_types::MessageRole::User,
        crate::a2a::Role::Agent => a2a_protocol_types::MessageRole::Agent,
    };

    let parts: Vec<a2a_protocol_types::Part> =
        msg.parts.iter().map(adk_internal_part_to_wire).collect::<Result<Vec<_>, _>>()?;

    // Extract extensions and reference_task_ids from metadata
    let mut extensions: Option<Vec<String>> = None;
    let mut reference_task_ids: Option<Vec<a2a_protocol_types::TaskId>> = None;
    let mut wire_metadata: Option<Value> = None;

    if let Some(ref meta) = msg.metadata {
        let mut clean_meta = meta.clone();

        if let Some(ext_val) = clean_meta.remove("_a2a_extensions") {
            if let Ok(exts) = serde_json::from_value::<Vec<String>>(ext_val) {
                extensions = Some(exts);
            }
        }

        if let Some(ref_val) = clean_meta.remove("_a2a_reference_task_ids") {
            if let Ok(ids) = serde_json::from_value::<Vec<String>>(ref_val) {
                reference_task_ids =
                    Some(ids.into_iter().map(a2a_protocol_types::TaskId::new).collect());
            }
        }

        if !clean_meta.is_empty() {
            wire_metadata = Some(Value::Object(clean_meta));
        }
    }

    Ok(a2a_protocol_types::Message {
        id: a2a_protocol_types::MessageId::new(&msg.message_id),
        role,
        parts,
        task_id: msg.task_id.as_ref().map(a2a_protocol_types::TaskId::new),
        context_id: msg.context_id.as_ref().map(a2a_protocol_types::ContextId::new),
        reference_task_ids,
        extensions,
        metadata: wire_metadata,
    })
}

/// Convert an internal a2a Part to a wire Part.
fn adk_internal_part_to_wire(
    part: &crate::a2a::Part,
) -> Result<a2a_protocol_types::Part, A2aError> {
    match part {
        crate::a2a::Part::Text { text, .. } => Ok(a2a_protocol_types::Part::text(text.clone())),
        crate::a2a::Part::File { file, .. } => {
            if let Some(ref bytes) = file.bytes {
                let mut p = a2a_protocol_types::Part::raw(bytes.clone());
                if let Some(ref name) = file.name {
                    p = p.with_filename(name.clone());
                }
                if let Some(ref mt) = file.mime_type {
                    p = p.with_media_type(mt.clone());
                }
                Ok(p)
            } else if let Some(ref uri) = file.uri {
                let mut p = a2a_protocol_types::Part::url(uri.clone());
                if let Some(ref name) = file.name {
                    p = p.with_filename(name.clone());
                }
                if let Some(ref mt) = file.mime_type {
                    p = p.with_media_type(mt.clone());
                }
                Ok(p)
            } else {
                Err(A2aError::InvalidParams {
                    message: "file part has neither bytes nor uri".to_string(),
                })
            }
        }
        crate::a2a::Part::Data { data, .. } => {
            Ok(a2a_protocol_types::Part::data(Value::Object(data.clone())))
        }
    }
}

// ── Task conversion ──────────────────────────────────────────────────────────

/// Convert an `a2a_protocol_types::Task` to a `TaskStoreEntry`.
pub fn wire_task_to_internal(task: &a2a_protocol_types::Task) -> Result<TaskStoreEntry, A2aError> {
    let now = chrono::Utc::now();

    let metadata: HashMap<String, Value> = task
        .metadata
        .as_ref()
        .and_then(|v| v.as_object())
        .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();

    Ok(TaskStoreEntry {
        id: task.id.0.clone(),
        context_id: task.context_id.0.clone(),
        status: task.status.clone(),
        artifacts: task.artifacts.clone().unwrap_or_default(),
        history: task.history.clone().unwrap_or_default(),
        metadata,
        push_configs: Vec::new(),
        created_at: now,
        updated_at: now,
    })
}

/// Convert a `TaskStoreEntry` to an `a2a_protocol_types::Task`.
pub fn internal_task_to_wire(entry: &TaskStoreEntry) -> Result<a2a_protocol_types::Task, A2aError> {
    let metadata = if entry.metadata.is_empty() {
        None
    } else {
        let obj: Map<String, Value> =
            entry.metadata.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Some(Value::Object(obj))
    };

    let history = if entry.history.is_empty() { None } else { Some(entry.history.clone()) };

    let artifacts = if entry.artifacts.is_empty() { None } else { Some(entry.artifacts.clone()) };

    Ok(a2a_protocol_types::Task {
        id: a2a_protocol_types::TaskId::new(&entry.id),
        context_id: a2a_protocol_types::ContextId::new(&entry.context_id),
        status: entry.status.clone(),
        history,
        artifacts,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── wire_part_to_adk ─────────────────────────────────────────────────

    #[test]
    fn text_part_wire_to_adk() {
        let wire = a2a_protocol_types::Part::text("hello");
        let adk = wire_part_to_adk(&wire).unwrap();
        assert!(matches!(adk, adk_core::Part::Text { ref text } if text == "hello"));
    }

    #[test]
    fn raw_part_wire_to_adk() {
        let encoded = general_purpose::STANDARD.encode(b"binary data");
        let wire = a2a_protocol_types::Part::raw(&encoded).with_media_type("image/png");
        let adk = wire_part_to_adk(&wire).unwrap();
        match adk {
            adk_core::Part::InlineData { mime_type, data } => {
                assert_eq!(mime_type, "image/png");
                assert_eq!(data, b"binary data");
            }
            _ => panic!("expected InlineData"),
        }
    }

    #[test]
    fn url_part_wire_to_adk() {
        let wire = a2a_protocol_types::Part::url("https://example.com/f.pdf")
            .with_media_type("application/pdf");
        let adk = wire_part_to_adk(&wire).unwrap();
        match adk {
            adk_core::Part::FileData { mime_type, file_uri } => {
                assert_eq!(mime_type, "application/pdf");
                assert_eq!(file_uri, "https://example.com/f.pdf");
            }
            _ => panic!("expected FileData"),
        }
    }

    #[test]
    fn data_function_call_wire_to_adk() {
        let data = json!({
            "function_call": {
                "name": "get_weather",
                "args": {"city": "Seattle"},
                "id": "call_1"
            }
        });
        let wire = a2a_protocol_types::Part::data(data);
        let adk = wire_part_to_adk(&wire).unwrap();
        match adk {
            adk_core::Part::FunctionCall { name, args, id, .. } => {
                assert_eq!(name, "get_weather");
                assert_eq!(args["city"], "Seattle");
                assert_eq!(id.as_deref(), Some("call_1"));
            }
            _ => panic!("expected FunctionCall"),
        }
    }

    #[test]
    fn data_function_response_wire_to_adk() {
        let data = json!({
            "function_response": {
                "name": "get_weather",
                "response": {"temp": 72},
                "id": "call_1"
            }
        });
        let wire = a2a_protocol_types::Part::data(data);
        let adk = wire_part_to_adk(&wire).unwrap();
        match adk {
            adk_core::Part::FunctionResponse { function_response, id } => {
                assert_eq!(function_response.name, "get_weather");
                assert_eq!(function_response.response["temp"], 72);
                assert_eq!(id.as_deref(), Some("call_1"));
            }
            _ => panic!("expected FunctionResponse"),
        }
    }

    #[test]
    fn data_server_tool_call_wire_to_adk() {
        let data = json!({ "server_tool_call": {"tool": "search", "query": "rust"} });
        let wire = a2a_protocol_types::Part::data(data);
        let adk = wire_part_to_adk(&wire).unwrap();
        match adk {
            adk_core::Part::ServerToolCall { server_tool_call } => {
                assert_eq!(server_tool_call["tool"], "search");
            }
            _ => panic!("expected ServerToolCall"),
        }
    }

    #[test]
    fn data_server_tool_response_wire_to_adk() {
        let data = json!({ "server_tool_response": {"result": "found"} });
        let wire = a2a_protocol_types::Part::data(data);
        let adk = wire_part_to_adk(&wire).unwrap();
        match adk {
            adk_core::Part::ServerToolResponse { server_tool_response } => {
                assert_eq!(server_tool_response["result"], "found");
            }
            _ => panic!("expected ServerToolResponse"),
        }
    }

    #[test]
    fn data_unknown_key_returns_error() {
        let data = json!({ "unknown_key": "value" });
        let wire = a2a_protocol_types::Part::data(data);
        assert!(wire_part_to_adk(&wire).is_err());
    }

    #[test]
    fn raw_invalid_base64_returns_error() {
        let wire = a2a_protocol_types::Part::raw("not-valid-base64!!!");
        assert!(wire_part_to_adk(&wire).is_err());
    }

    // ── adk_part_to_wire ─────────────────────────────────────────────────

    #[test]
    fn text_adk_to_wire() {
        let adk = adk_core::Part::Text { text: "hello".to_string() };
        let wire = adk_part_to_wire(&adk).unwrap();
        assert!(
            matches!(wire.content, a2a_protocol_types::PartContent::Text(ref t) if t == "hello")
        );
    }

    #[test]
    fn inline_data_adk_to_wire() {
        let adk = adk_core::Part::InlineData {
            mime_type: "image/png".to_string(),
            data: b"binary".to_vec(),
        };
        let wire = adk_part_to_wire(&adk).unwrap();
        match wire.content {
            a2a_protocol_types::PartContent::Raw(ref encoded) => {
                let decoded = general_purpose::STANDARD.decode(encoded).unwrap();
                assert_eq!(decoded, b"binary");
            }
            _ => panic!("expected Raw"),
        }
        assert_eq!(wire.media_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn file_data_adk_to_wire() {
        let adk = adk_core::Part::FileData {
            mime_type: "application/pdf".to_string(),
            file_uri: "https://example.com/f.pdf".to_string(),
        };
        let wire = adk_part_to_wire(&adk).unwrap();
        assert!(
            matches!(wire.content, a2a_protocol_types::PartContent::Url(ref u) if u == "https://example.com/f.pdf")
        );
        assert_eq!(wire.media_type.as_deref(), Some("application/pdf"));
    }

    #[test]
    fn function_call_adk_to_wire() {
        let adk = adk_core::Part::FunctionCall {
            name: "test_fn".to_string(),
            args: json!({"key": "val"}),
            id: Some("c1".to_string()),
            thought_signature: None,
        };
        let wire = adk_part_to_wire(&adk).unwrap();
        match wire.content {
            a2a_protocol_types::PartContent::Data(ref data) => {
                let call = &data["function_call"];
                assert_eq!(call["name"], "test_fn");
                assert_eq!(call["args"]["key"], "val");
                assert_eq!(call["id"], "c1");
            }
            _ => panic!("expected Data"),
        }
    }

    #[test]
    fn function_response_adk_to_wire() {
        let adk = adk_core::Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData {
                name: "test_fn".to_string(),
                response: json!({"result": 42}),
            },
            id: Some("r1".to_string()),
        };
        let wire = adk_part_to_wire(&adk).unwrap();
        match wire.content {
            a2a_protocol_types::PartContent::Data(ref data) => {
                let resp = &data["function_response"];
                assert_eq!(resp["name"], "test_fn");
                assert_eq!(resp["response"]["result"], 42);
                assert_eq!(resp["id"], "r1");
            }
            _ => panic!("expected Data"),
        }
    }

    #[test]
    fn thinking_adk_to_wire_becomes_text() {
        let adk = adk_core::Part::Thinking {
            thinking: "let me think...".to_string(),
            signature: Some("sig123".to_string()),
        };
        let wire = adk_part_to_wire(&adk).unwrap();
        assert!(
            matches!(wire.content, a2a_protocol_types::PartContent::Text(ref t) if t == "let me think...")
        );
    }

    #[test]
    fn server_tool_call_adk_to_wire() {
        let adk = adk_core::Part::ServerToolCall { server_tool_call: json!({"tool": "search"}) };
        let wire = adk_part_to_wire(&adk).unwrap();
        match wire.content {
            a2a_protocol_types::PartContent::Data(ref data) => {
                assert_eq!(data["server_tool_call"]["tool"], "search");
            }
            _ => panic!("expected Data"),
        }
    }

    #[test]
    fn server_tool_response_adk_to_wire() {
        let adk =
            adk_core::Part::ServerToolResponse { server_tool_response: json!({"result": "ok"}) };
        let wire = adk_part_to_wire(&adk).unwrap();
        match wire.content {
            a2a_protocol_types::PartContent::Data(ref data) => {
                assert_eq!(data["server_tool_response"]["result"], "ok");
            }
            _ => panic!("expected Data"),
        }
    }

    // ── Part round-trip ──────────────────────────────────────────────────

    #[test]
    fn text_roundtrip() {
        let original = adk_core::Part::Text { text: "round trip".to_string() };
        let wire = adk_part_to_wire(&original).unwrap();
        let back = wire_part_to_adk(&wire).unwrap();
        assert!(matches!(back, adk_core::Part::Text { ref text } if text == "round trip"));
    }

    #[test]
    fn inline_data_roundtrip() {
        let original = adk_core::Part::InlineData {
            mime_type: "image/jpeg".to_string(),
            data: vec![0xFF, 0xD8, 0xFF, 0xE0],
        };
        let wire = adk_part_to_wire(&original).unwrap();
        let back = wire_part_to_adk(&wire).unwrap();
        match back {
            adk_core::Part::InlineData { mime_type, data } => {
                assert_eq!(mime_type, "image/jpeg");
                assert_eq!(data, vec![0xFF, 0xD8, 0xFF, 0xE0]);
            }
            _ => panic!("expected InlineData"),
        }
    }

    #[test]
    fn file_data_roundtrip() {
        let original = adk_core::Part::FileData {
            mime_type: "text/plain".to_string(),
            file_uri: "gs://bucket/file.txt".to_string(),
        };
        let wire = adk_part_to_wire(&original).unwrap();
        let back = wire_part_to_adk(&wire).unwrap();
        match back {
            adk_core::Part::FileData { mime_type, file_uri } => {
                assert_eq!(mime_type, "text/plain");
                assert_eq!(file_uri, "gs://bucket/file.txt");
            }
            _ => panic!("expected FileData"),
        }
    }

    #[test]
    fn function_call_roundtrip() {
        let original = adk_core::Part::FunctionCall {
            name: "my_func".to_string(),
            args: json!({"a": 1}),
            id: Some("id1".to_string()),
            thought_signature: None,
        };
        let wire = adk_part_to_wire(&original).unwrap();
        let back = wire_part_to_adk(&wire).unwrap();
        match back {
            adk_core::Part::FunctionCall { name, args, id, .. } => {
                assert_eq!(name, "my_func");
                assert_eq!(args, json!({"a": 1}));
                assert_eq!(id.as_deref(), Some("id1"));
            }
            _ => panic!("expected FunctionCall"),
        }
    }

    // ── Message conversion ───────────────────────────────────────────────

    #[test]
    fn message_wire_to_adk_basic() {
        let wire = a2a_protocol_types::Message {
            id: a2a_protocol_types::MessageId::new("msg-1"),
            role: a2a_protocol_types::MessageRole::User,
            parts: vec![a2a_protocol_types::Part::text("hello")],
            task_id: Some(a2a_protocol_types::TaskId::new("task-1")),
            context_id: Some(a2a_protocol_types::ContextId::new("ctx-1")),
            reference_task_ids: None,
            extensions: None,
            metadata: None,
        };
        let adk = wire_message_to_adk(&wire).unwrap();
        assert_eq!(adk.message_id, "msg-1");
        assert!(matches!(adk.role, crate::a2a::Role::User));
        assert_eq!(adk.parts.len(), 1);
        assert_eq!(adk.task_id.as_deref(), Some("task-1"));
        assert_eq!(adk.context_id.as_deref(), Some("ctx-1"));
    }

    #[test]
    fn message_preserves_extensions_in_metadata() {
        let wire = a2a_protocol_types::Message {
            id: a2a_protocol_types::MessageId::new("msg-2"),
            role: a2a_protocol_types::MessageRole::Agent,
            parts: vec![a2a_protocol_types::Part::text("hi")],
            task_id: None,
            context_id: None,
            reference_task_ids: Some(vec![a2a_protocol_types::TaskId::new("ref-1")]),
            extensions: Some(vec!["urn:ext:custom".to_string()]),
            metadata: None,
        };
        let adk = wire_message_to_adk(&wire).unwrap();
        let meta = adk.metadata.as_ref().unwrap();
        assert!(meta.contains_key("_a2a_extensions"));
        assert!(meta.contains_key("_a2a_reference_task_ids"));
    }

    #[test]
    fn message_adk_to_wire_basic() {
        let adk = crate::a2a::Message {
            role: crate::a2a::Role::Agent,
            parts: vec![crate::a2a::Part::text("response".to_string())],
            metadata: None,
            message_id: "msg-3".to_string(),
            task_id: Some("task-2".to_string()),
            context_id: Some("ctx-2".to_string()),
        };
        let wire = adk_message_to_wire(&adk).unwrap();
        assert_eq!(wire.id.0, "msg-3");
        assert_eq!(wire.role, a2a_protocol_types::MessageRole::Agent);
        assert_eq!(wire.parts.len(), 1);
        assert_eq!(wire.task_id.as_ref().unwrap().0, "task-2");
        assert_eq!(wire.context_id.as_ref().unwrap().0, "ctx-2");
    }

    #[test]
    fn message_roundtrip_preserves_extensions() {
        let wire_original = a2a_protocol_types::Message {
            id: a2a_protocol_types::MessageId::new("msg-rt"),
            role: a2a_protocol_types::MessageRole::User,
            parts: vec![a2a_protocol_types::Part::text("test")],
            task_id: None,
            context_id: None,
            reference_task_ids: Some(vec![a2a_protocol_types::TaskId::new("ref-1")]),
            extensions: Some(vec!["urn:ext:a".to_string()]),
            metadata: None,
        };
        let adk = wire_message_to_adk(&wire_original).unwrap();
        let wire_back = adk_message_to_wire(&adk).unwrap();

        assert_eq!(wire_back.extensions, wire_original.extensions);
        assert_eq!(
            wire_back.reference_task_ids.as_ref().map(|v| v.len()),
            wire_original.reference_task_ids.as_ref().map(|v| v.len())
        );
    }

    // ── Task conversion ──────────────────────────────────────────────────

    #[test]
    fn task_wire_to_internal() {
        let wire = a2a_protocol_types::Task {
            id: a2a_protocol_types::TaskId::new("task-1"),
            context_id: a2a_protocol_types::ContextId::new("ctx-1"),
            status: a2a_protocol_types::TaskStatus::new(a2a_protocol_types::TaskState::Working),
            history: Some(vec![a2a_protocol_types::Message {
                id: a2a_protocol_types::MessageId::new("m1"),
                role: a2a_protocol_types::MessageRole::User,
                parts: vec![a2a_protocol_types::Part::text("hi")],
                task_id: None,
                context_id: None,
                reference_task_ids: None,
                extensions: None,
                metadata: None,
            }]),
            artifacts: None,
            metadata: Some(json!({"key": "val"})),
        };
        let entry = wire_task_to_internal(&wire).unwrap();
        assert_eq!(entry.id, "task-1");
        assert_eq!(entry.context_id, "ctx-1");
        assert_eq!(entry.status.state, a2a_protocol_types::TaskState::Working);
        assert_eq!(entry.history.len(), 1);
        assert!(entry.artifacts.is_empty());
        assert_eq!(entry.metadata.get("key").unwrap(), &json!("val"));
    }

    #[test]
    fn task_internal_to_wire() {
        let now = chrono::Utc::now();
        let mut metadata = HashMap::new();
        metadata.insert("foo".to_string(), json!("bar"));

        let entry = TaskStoreEntry {
            id: "task-2".to_string(),
            context_id: "ctx-2".to_string(),
            status: a2a_protocol_types::TaskStatus::new(a2a_protocol_types::TaskState::Completed),
            artifacts: vec![a2a_protocol_types::Artifact::new(
                a2a_protocol_types::ArtifactId::new("art-1"),
                vec![a2a_protocol_types::Part::text("result")],
            )],
            history: Vec::new(),
            metadata,
            push_configs: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        let wire = internal_task_to_wire(&entry).unwrap();
        assert_eq!(wire.id.0, "task-2");
        assert_eq!(wire.context_id.0, "ctx-2");
        assert_eq!(wire.status.state, a2a_protocol_types::TaskState::Completed);
        assert_eq!(wire.artifacts.as_ref().unwrap().len(), 1);
        assert!(wire.history.is_none());
        assert_eq!(wire.metadata.as_ref().unwrap()["foo"], "bar");
    }

    #[test]
    fn task_roundtrip() {
        let wire_original = a2a_protocol_types::Task {
            id: a2a_protocol_types::TaskId::new("task-rt"),
            context_id: a2a_protocol_types::ContextId::new("ctx-rt"),
            status: a2a_protocol_types::TaskStatus::new(a2a_protocol_types::TaskState::Submitted),
            history: None,
            artifacts: None,
            metadata: None,
        };
        let entry = wire_task_to_internal(&wire_original).unwrap();
        let wire_back = internal_task_to_wire(&entry).unwrap();

        assert_eq!(wire_back.id, wire_original.id);
        assert_eq!(wire_back.context_id, wire_original.context_id);
        assert_eq!(wire_back.status.state, wire_original.status.state);
    }
}
