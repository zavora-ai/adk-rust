//! The A2UI v0.9.1 wire model (spec 3.3 §6.1 `genui`): the messages an agent sends a
//! renderer and a renderer sends back, dynamic strings, the data model, JSON lines, stable
//! component ids and capability sets.
//!
//! Ported from `adk-ui` (commit `5a82754`) onto adk-core 3. Only the production v0.9.1
//! protocol is modelled; the v1.0 release candidate's messages are not.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::catalog::FORMS_CATALOG_ID;

/// The A2UI version every envelope carries.
pub const A2UI_VERSION: &str = "v0.9.1";

/// The media type of an A2UI payload.
pub const A2UI_MIME_TYPE: &str = "application/a2ui+json";

/// What can go wrong reading A2UI (spec 3.3 §6.1 `genui`).
#[derive(Debug, thiserror::Error)]
pub enum A2uiError {
    /// A JSON line is not an A2UI message.
    #[error("line {line} is not an A2UI v0.9.1 message: {source}. Send one message per line.")]
    Line {
        /// The line's number, from 1.
        line: usize,
        /// Why it does not parse.
        #[source]
        source: serde_json::Error,
    },
    /// A data model path is not a JSON pointer.
    #[error("'{path}' is not a data model path: a path starts with '/', like '/user/name'")]
    Path {
        /// The path.
        path: String,
    },
}

/// One agent-to-renderer message: its version and its body (A2UI v0.9.1
/// `server_to_client.json`).
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{A2uiBody, A2uiMessage};
///
/// let message = A2uiMessage::create_surface("invoice-10");
/// let wire = serde_json::to_value(&message).unwrap();
/// assert_eq!(wire["version"], "v0.9.1");
/// assert_eq!(wire["createSurface"]["surfaceId"], "invoice-10");
/// assert!(matches!(message.body, A2uiBody::CreateSurface(_)));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct A2uiMessage {
    /// The protocol version, `v0.9.1`.
    pub version: String,
    /// What the message does.
    #[serde(flatten)]
    pub body: A2uiBody,
}

/// What an [`A2uiMessage`] does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum A2uiBody {
    /// Starts a surface.
    CreateSurface(CreateSurface),
    /// Sets a surface's components.
    UpdateComponents(UpdateComponents),
    /// Sets or removes a value in a surface's data model.
    UpdateDataModel(UpdateDataModel),
    /// Removes a surface.
    DeleteSurface(DeleteSurface),
}

/// The body of a `createSurface` message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSurface {
    /// The surface's id.
    pub surface_id: String,
    /// The catalog its components come from.
    pub catalog_id: String,
    /// Theme parameters. Drawn surfaces carry none: the home draws them in its own colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Value>,
    /// Whether the renderer sends the data model back with every action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_data_model: Option<bool>,
}

/// The body of an `updateComponents` message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateComponents {
    /// The surface's id.
    pub surface_id: String,
    /// Every component of the surface, one with id `root`.
    pub components: Vec<Value>,
}

/// The body of an `updateDataModel` message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateDataModel {
    /// The surface's id.
    pub surface_id: String,
    /// Where in the data model, as a JSON pointer; absent or `/` is the whole model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The value set at `path`; absent or `null` removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

/// The body of a `deleteSurface` message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteSurface {
    /// The surface's id.
    pub surface_id: String,
}

impl A2uiMessage {
    fn new(body: A2uiBody) -> Self {
        Self { version: A2UI_VERSION.to_string(), body }
    }

    /// A `createSurface` for `surface_id` on the `forms` 1.0 catalog, with no theme.
    pub fn create_surface(surface_id: impl Into<String>) -> Self {
        Self::new(A2uiBody::CreateSurface(CreateSurface {
            surface_id: surface_id.into(),
            catalog_id: FORMS_CATALOG_ID.to_string(),
            theme: None,
            send_data_model: None,
        }))
    }

    /// An `updateComponents` setting every component of `surface_id`.
    pub fn update_components(surface_id: impl Into<String>, components: Vec<Value>) -> Self {
        Self::new(A2uiBody::UpdateComponents(UpdateComponents {
            surface_id: surface_id.into(),
            components,
        }))
    }

    /// An `updateDataModel` setting `value` at `path` of `surface_id`'s data model.
    pub fn update_data_model(
        surface_id: impl Into<String>,
        path: Option<String>,
        value: Option<Value>,
    ) -> Self {
        Self::new(A2uiBody::UpdateDataModel(UpdateDataModel {
            surface_id: surface_id.into(),
            path,
            value,
        }))
    }

    /// A `deleteSurface` for `surface_id`.
    pub fn delete_surface(surface_id: impl Into<String>) -> Self {
        Self::new(A2uiBody::DeleteSurface(DeleteSurface { surface_id: surface_id.into() }))
    }

    /// The surface the message is for.
    pub fn surface_id(&self) -> &str {
        match &self.body {
            A2uiBody::CreateSurface(body) => &body.surface_id,
            A2uiBody::UpdateComponents(body) => &body.surface_id,
            A2uiBody::UpdateDataModel(body) => &body.surface_id,
            A2uiBody::DeleteSurface(body) => &body.surface_id,
        }
    }
}

/// One renderer-to-agent message: an action a person took, or an error the renderer
/// reports (A2UI v0.9.1 `client_to_server.json`).
///
/// A drawn surface's actions never reach the agent this way: a person's Send becomes a
/// requested call through the engine (spec 3.3 §7.4 rule 5). The type reads what an
/// A2UI renderer sends.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{A2uiClientBody, A2uiClientMessage};
///
/// let message: A2uiClientMessage = serde_json::from_value(serde_json::json!({
///     "version": "v0.9.1",
///     "action": {
///         "name": "send",
///         "surfaceId": "invoice-10",
///         "sourceComponentId": "send-button",
///         "timestamp": "2026-10-26T10:03:00Z",
///         "context": {}
///     }
/// }))
/// .unwrap();
/// assert!(matches!(message.body, A2uiClientBody::Action(ref action) if action.name == "send"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct A2uiClientMessage {
    /// The protocol version, `v0.9.1`.
    pub version: String,
    /// What the renderer reports.
    #[serde(flatten)]
    pub body: A2uiClientBody,
}

/// What an [`A2uiClientMessage`] reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum A2uiClientBody {
    /// A person used a control that dispatches an event.
    Action(ClientAction),
    /// The renderer could not show a message.
    Error(ClientError),
}

/// A person used a control that dispatches an event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientAction {
    /// The event's name.
    pub name: String,
    /// The surface it happened on.
    pub surface_id: String,
    /// The component that dispatched it.
    pub source_component_id: String,
    /// When, in RFC 3339.
    pub timestamp: String,
    /// The event's context, with bound values resolved.
    #[serde(default)]
    pub context: Value,
}

/// The renderer could not show a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientError {
    /// `VALIDATION_FAILED`, or another code.
    pub code: String,
    /// The surface.
    pub surface_id: String,
    /// Where in the message, as a JSON pointer.
    pub path: String,
    /// What went wrong.
    pub message: String,
}

/// A string a component shows: a literal, a binding to the data model, or a function call
/// that returns a string (A2UI v0.9.1 `DynamicString`).
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::DynamicString;
/// use serde_json::json;
///
/// let bound: DynamicString = serde_json::from_value(json!({ "path": "/to" })).unwrap();
/// assert_eq!(bound, DynamicString::path("/to"));
/// assert_eq!(serde_json::to_value(DynamicString::literal("To")).unwrap(), json!("To"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum DynamicString {
    /// The text itself.
    Literal(String),
    /// The value at a data model path.
    Binding(DataBinding),
    /// A function's result.
    Call(FunctionCall),
}

impl DynamicString {
    /// The text `value`.
    pub fn literal(value: impl Into<String>) -> Self {
        Self::Literal(value.into())
    }

    /// The value at `path`.
    pub fn path(path: impl Into<String>) -> Self {
        Self::Binding(DataBinding { path: path.into() })
    }
}

/// A binding to a data model value: an absolute JSON pointer such as `/user/name`, or a path
/// relative to the list item a template renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DataBinding {
    /// The path.
    pub path: String,
}

/// A call of a catalog function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FunctionCall {
    /// The function's name.
    pub call: String,
    /// Its named arguments.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, Value>,
    /// The type it returns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
}

/// A surface's data model: the JSON its components bind to.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::DataModel;
/// use serde_json::json;
///
/// let mut model = DataModel::default();
/// model.apply(Some("/invoice/to"), Some(json!("ana@example.com")))?;
/// assert_eq!(model.get("/invoice/to"), Some(&json!("ana@example.com")));
/// model.apply(Some("/invoice/to"), None)?;
/// assert_eq!(model.value(), &json!({ "invoice": {} }));
/// # Ok::<(), adk_surface::genui::A2uiError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct DataModel(Value);

impl DataModel {
    /// A data model holding `value`.
    pub fn new(value: Value) -> Self {
        Self(value)
    }

    /// The whole model.
    pub fn value(&self) -> &Value {
        &self.0
    }

    /// The value at `pointer`, or `None`.
    pub fn get(&self, pointer: &str) -> Option<&Value> {
        if pointer.is_empty() || pointer == "/" {
            return Some(&self.0);
        }
        self.0.pointer(pointer)
    }

    /// Applies an `updateDataModel`: sets `value` at `path`, creating the objects on the way,
    /// or removes what is there when `value` is `None` or `null`. An absent path or `/` is the
    /// whole model.
    ///
    /// # Errors
    ///
    /// Returns [`A2uiError::Path`] when `path` does not start with `/`.
    pub fn apply(&mut self, path: Option<&str>, value: Option<Value>) -> Result<(), A2uiError> {
        let value = value.filter(|value| !value.is_null());
        let path = path.unwrap_or("/");
        if path == "/" || path.is_empty() {
            self.0 = value.unwrap_or(Value::Null);
            return Ok(());
        }
        let invalid = || A2uiError::Path { path: path.to_string() };
        let rest = path.strip_prefix('/').ok_or_else(invalid)?;
        let tokens: Vec<String> =
            rest.split('/').map(|token| token.replace("~1", "/").replace("~0", "~")).collect();
        let Some((last, parents)) = tokens.split_last() else {
            return Err(invalid());
        };
        let setting = value.is_some();
        let mut node = &mut self.0;
        for token in parents {
            if setting && !node.is_object() && !node.is_array() {
                *node = Value::Object(serde_json::Map::new());
            }
            node = match (node, setting) {
                (Value::Object(map), true) => map
                    .entry(token.clone())
                    .or_insert_with(|| Value::Object(serde_json::Map::new())),
                (Value::Object(map), false) => match map.get_mut(token) {
                    Some(next) => next,
                    None => return Ok(()),
                },
                (Value::Array(items), _) => match token.parse::<usize>() {
                    Ok(index) if index < items.len() => &mut items[index],
                    _ if setting => return Err(invalid()),
                    _ => return Ok(()),
                },
                // Removing below a value that is not a container removes nothing.
                _ => return Ok(()),
            };
        }
        match (node, value) {
            (Value::Object(map), Some(value)) => {
                map.insert(last.clone(), value);
            }
            (Value::Object(map), None) => {
                map.remove(last);
            }
            (Value::Array(items), Some(value)) => match last.parse::<usize>() {
                Ok(index) if index < items.len() => items[index] = value,
                Ok(index) if index == items.len() => items.push(value),
                _ => return Err(invalid()),
            },
            (Value::Array(items), None) => {
                if let Ok(index) = last.parse::<usize>()
                    && index < items.len()
                {
                    items.remove(index);
                }
            }
            (node, Some(value)) => {
                let mut map = serde_json::Map::new();
                map.insert(last.clone(), value);
                *node = Value::Object(map);
            }
            (_, None) => {}
        }
        Ok(())
    }
}

/// Encodes `messages` as JSON lines, one message per line.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{A2uiMessage, decode_jsonl, encode_jsonl};
///
/// let messages = vec![A2uiMessage::create_surface("s"), A2uiMessage::delete_surface("s")];
/// let lines = encode_jsonl(&messages);
/// assert_eq!(lines.lines().count(), 2);
/// assert_eq!(decode_jsonl(&lines)?, messages);
/// # Ok::<(), adk_surface::genui::A2uiError>(())
/// ```
pub fn encode_jsonl(messages: &[A2uiMessage]) -> String {
    let mut out = String::new();
    for message in messages {
        // An A2UI message is plain JSON data, so it always serializes.
        out.push_str(&serde_json::to_string(message).unwrap_or_default());
        out.push('\n');
    }
    out
}

/// Decodes JSON lines into messages; blank lines are skipped.
///
/// # Errors
///
/// Returns [`A2uiError::Line`] for the first line that is not an A2UI message.
pub fn decode_jsonl(text: &str) -> Result<Vec<A2uiMessage>, A2uiError> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|source| A2uiError::Line { line: index + 1, source })
        })
        .collect()
}

/// A deterministic component id from `seed`: `id_` and a 64-bit FNV-1a hash in hex.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::stable_id;
///
/// assert_eq!(stable_id("form:to"), stable_id("form:to"));
/// assert_ne!(stable_id("form:to"), stable_id("form:amount"));
/// assert!(stable_id("x").starts_with("id_"));
/// ```
pub fn stable_id(seed: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    let hash = seed.bytes().fold(OFFSET, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(PRIME));
    format!("id_{hash:016x}")
}

/// The catalogs a renderer or an agent supports, by A2UI version family (A2UI v0.9.1
/// `client_capabilities.json`, keyed `v0.9` upstream).
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{A2uiCapabilities, FORMS_CATALOG_ID};
///
/// let wire = serde_json::to_value(A2uiCapabilities::forms()).unwrap();
/// assert_eq!(wire["v0.9"]["supportedCatalogIds"][0], FORMS_CATALOG_ID);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct A2uiCapabilities {
    /// What v0.9 and v0.9.1 support.
    #[serde(rename = "v0.9", default, skip_serializing_if = "Option::is_none")]
    pub v0_9: Option<CapabilitySet>,
}

impl A2uiCapabilities {
    /// The `forms` 1.0 catalog only, with no inline catalogs: what this home renders.
    pub fn forms() -> Self {
        Self {
            v0_9: Some(CapabilitySet {
                supported_catalog_ids: vec![FORMS_CATALOG_ID.to_string()],
                accepts_inline_catalogs: false,
            }),
        }
    }
}

/// The catalogs supported under one A2UI version family.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySet {
    /// Catalog ids, by preference.
    pub supported_catalog_ids: Vec<String>,
    /// Whether catalogs sent inline are accepted. Drawn surfaces never use one.
    #[serde(default)]
    pub accepts_inline_catalogs: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_round_trip_through_the_upstream_wire_shape() {
        let wire = json!({
            "version": "v0.9.1",
            "updateDataModel": { "surfaceId": "s", "path": "/to", "value": "ana@example.com" }
        });
        let message: A2uiMessage = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            message,
            A2uiMessage::update_data_model(
                "s",
                Some("/to".to_string()),
                Some(json!("ana@example.com"))
            )
        );
        assert_eq!(serde_json::to_value(&message).unwrap(), wire);
    }

    #[test]
    fn an_unknown_body_or_field_is_not_a_message() {
        for wire in [
            json!({ "version": "v0.9.1", "callFunction": { "call": "x" } }),
            json!({ "version": "v0.9.1", "deleteSurface": { "surfaceId": "s", "extra": 1 } }),
        ] {
            assert!(serde_json::from_value::<A2uiMessage>(wire.clone()).is_err(), "{wire}");
        }
    }

    #[test]
    fn decoding_names_the_line_that_fails() {
        let error = decode_jsonl(
            "{\"version\":\"v0.9.1\",\"deleteSurface\":{\"surfaceId\":\"s\"}}\n\nnope\n",
        )
        .unwrap_err();
        assert!(matches!(error, A2uiError::Line { line: 3, .. }), "{error}");
    }

    #[test]
    fn the_data_model_sets_and_removes_values_in_objects_and_arrays() {
        let mut model = DataModel::new(json!({ "rows": [{ "a": 1 }] }));
        model.apply(Some("/rows/0/a"), Some(json!(2))).unwrap();
        model.apply(Some("/rows/1"), Some(json!({ "a": 3 }))).unwrap();
        model.apply(Some("/a~1b"), Some(json!("slash"))).unwrap();
        assert_eq!(model.value(), &json!({ "rows": [{ "a": 2 }, { "a": 3 }], "a/b": "slash" }));
        model.apply(Some("/rows/0"), None).unwrap();
        model.apply(Some("/missing/deep"), None).unwrap();
        assert_eq!(model.value(), &json!({ "rows": [{ "a": 3 }], "a/b": "slash" }));
        assert!(model.apply(Some("rows"), Some(json!(1))).is_err());
        assert!(model.apply(Some("/rows/7"), Some(json!(1))).is_err());
        model.apply(None, Some(json!({ "fresh": true }))).unwrap();
        assert_eq!(model.value(), &json!({ "fresh": true }));
    }
}
