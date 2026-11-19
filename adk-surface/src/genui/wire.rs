//! A2UI versions on the wire (spec 3.3 §6.1 `genui`): a drawing is stored and checked in
//! one internal form (the A2UI v0.9.1 shape of [`A2uiMessage`]) and written in the version a
//! renderer speaks. A2UI 1.0 is the default; v0.9.1 is kept for renderers that offer nothing
//! newer.
//!
//! | 1.0 on the wire | Internal |
//! |---|---|
//! | `{"@path": "/to"}` where a value is dynamic | `{"path": "/to"}` |
//! | `{"@call": "formatNumber", "args": {…}}` | `{"call": "formatNumber", "args": {…}}` |
//! | `@accessibility`, `@checks` at a component's root | `accessibility`, `checks` |
//! | `@catalogId` on an extension component; the surface's default catalog is `forms` | the drawing's one catalog |
//! | a literal key `"@@x"` | `"@x"` |
//! | `createSurface` carrying `components` and `dataModel` | `createSurface`, `updateComponents`, `updateDataModel` |
//! | `updateDataModel.value: null` | no value (removes the path) |
//! | `createSurface.metadata.extensions.adkrust_display_only` | implied: every drawing follows rule 5 |
//!
//! Every 1.0 `createSurface` claims the display-only extension ([`DISPLAY_ONLY_EXTENSION`],
//! `docs/design/a2ui-extension-display-only.md`): a drawing shows; only the host decides.
//!
//! Reading 1.0 refuses what the internal form cannot hold without changing its meaning: an
//! unknown `@` key, a literal object whose only key is `path` or that has a `call` key (the
//! internal form would read it as a binding or a call), `@metadata`, `@index`, and surface
//! metadata other than the display-only claim.

use serde_json::{Map, Value, json};

use super::a2ui::{A2UI_VERSION, A2uiBody, A2uiMessage};
use super::catalog::{Catalog, FORMS_CATALOG_ID};

/// The A2UI 1.0 extension every drawing `genui` writes claims in its `createSurface`
/// (`metadata.extensions`): the surface only shows, no string in it reads as a decision, a
/// secret or a link, and a send only asks the host for a call the person then decides on
/// (spec 3.3 §7.4 rule 5; `docs/design/a2ui-extension-display-only.md`).
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{A2uiMessage, A2uiVersion, Catalog, DISPLAY_ONLY_EXTENSION, to_wire};
///
/// let wire = to_wire(&[A2uiMessage::create_surface("s")], Catalog::forms(), A2uiVersion::V1_0);
/// let claim = &wire[0]["createSurface"]["metadata"]["extensions"][DISPLAY_ONLY_EXTENSION];
/// assert_eq!(claim["version"], "1.0");
/// ```
pub const DISPLAY_ONLY_EXTENSION: &str = "adkrust_display_only";

/// The version of [`DISPLAY_ONLY_EXTENSION`] this build follows.
pub const DISPLAY_ONLY_EXTENSION_VERSION: &str = "1.0";

/// An A2UI protocol version `genui` reads and writes.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::A2uiVersion;
///
/// assert_eq!(A2uiVersion::CURRENT, A2uiVersion::V1_0);
/// assert_eq!(A2uiVersion::of_wire("v0.9.1"), Some(A2uiVersion::V0_9_1));
/// assert_eq!(A2uiVersion::V1_0.wire(), "1.0");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum A2uiVersion {
    /// A2UI v0.9.1, the version drawings are stored in.
    V0_9_1,
    /// A2UI 1.0, the current version.
    V1_0,
}

impl A2uiVersion {
    /// The version written when a renderer says nothing about what it speaks.
    pub const CURRENT: Self = Self::V1_0;

    /// Every version, oldest first.
    pub const ALL: [Self; 2] = [Self::V0_9_1, Self::V1_0];

    /// The `version` field of a message in this version.
    pub fn wire(self) -> &'static str {
        match self {
            Self::V0_9_1 => A2UI_VERSION,
            Self::V1_0 => "1.0",
        }
    }

    /// The key a capabilities object lists this version's catalogs under.
    pub fn capability_key(self) -> &'static str {
        match self {
            Self::V0_9_1 => "v0.9",
            Self::V1_0 => "1.0",
        }
    }

    /// The version a message's `version` field (or a capabilities key) names.
    pub fn of_wire(version: &str) -> Option<Self> {
        match version.trim() {
            "v0.9" | "v0.9.1" | "0.9" | "0.9.1" => Some(Self::V0_9_1),
            "1.0" | "v1.0" | "1" => Some(Self::V1_0),
            _ => None,
        }
    }
}

impl Default for A2uiVersion {
    fn default() -> Self {
        Self::CURRENT
    }
}

impl std::fmt::Display for A2uiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.wire())
    }
}

/// Why A2UI from the wire cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{at}: {reason}")]
pub struct WireError {
    /// Where, as a JSON pointer into what was read.
    pub at: String,
    /// What is wrong, in words a model can act on.
    pub reason: String,
}

impl WireError {
    fn new(at: &str, reason: impl Into<String>) -> Self {
        Self {
            at: if at.is_empty() { "/".to_string() } else { at.to_string() },
            reason: reason.into(),
        }
    }
}

/// Whether `components` are written in A2UI 1.0's syntax: a reserved `@` key anywhere.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::is_v1_0_syntax;
/// use serde_json::json;
///
/// assert!(is_v1_0_syntax(&[json!({ "id": "root", "component": "Text", "text": { "@path": "/t" } })]));
/// assert!(!is_v1_0_syntax(&[json!({ "id": "root", "component": "Text", "text": { "path": "/t" } })]));
/// ```
pub fn is_v1_0_syntax(components: &[Value]) -> bool {
    fn has_reserved(value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                map.keys().any(|key| key.starts_with('@')) || map.values().any(has_reserved)
            }
            Value::Array(items) => items.iter().any(has_reserved),
            _ => false,
        }
    }
    components.iter().any(has_reserved)
}

/// The catalog a component of a drawing in `catalog` comes from: `forms` for the forms
/// components, `catalog` itself for the ones it adds.
fn component_catalog<'a>(catalog: &'a Catalog, component: &str) -> &'a str {
    if catalog.is_extension() && !Catalog::forms().has_component(component) {
        catalog.id()
    } else {
        FORMS_CATALOG_ID
    }
}

/// Whether `map` is what the internal form reads as a data binding.
fn is_binding(map: &Map<String, Value>) -> bool {
    map.len() == 1 && map.get("path").is_some_and(Value::is_string)
}

/// Whether `map` is what the internal form reads as a function call.
fn is_call(map: &Map<String, Value>) -> bool {
    map.get("call").is_some_and(Value::is_string)
}

/// A dynamic value in 1.0's syntax.
fn dynamic_to_v1_0(value: &Value) -> Value {
    match value {
        Value::Object(map) if is_binding(map) => json!({ "@path": map["path"] }),
        Value::Object(map) if is_call(map) => {
            let mut out = Map::new();
            out.insert("@call".to_string(), map["call"].clone());
            if let Some(args) = map.get("args") {
                out.insert("args".to_string(), dynamic_to_v1_0(args));
            }
            Value::Object(out)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let key = if key.starts_with('@') { format!("@{key}") } else { key.clone() };
                    (key, dynamic_to_v1_0(value))
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(dynamic_to_v1_0).collect()),
        other => other.clone(),
    }
}

/// One component of a drawing in `catalog`, in 1.0's syntax.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{Catalog, component_to_v1_0};
/// use serde_json::json;
///
/// let text = json!({ "id": "t", "component": "Text", "text": { "path": "/title" },
///     "accessibility": { "label": "Title" } });
/// let wire = component_to_v1_0(&text, Catalog::forms());
/// assert_eq!(wire["text"], json!({ "@path": "/title" }));
/// assert_eq!(wire["@accessibility"], json!({ "label": "Title" }));
/// ```
pub fn component_to_v1_0(component: &Value, catalog: &Catalog) -> Value {
    let Value::Object(map) = component else {
        return component.clone();
    };
    let mut out = Map::new();
    for (key, value) in map {
        match key.as_str() {
            "id" | "component" => {
                out.insert(key.clone(), value.clone());
            }
            "accessibility" | "checks" => {
                out.insert(format!("@{key}"), dynamic_to_v1_0(value));
            }
            _ => {
                out.insert(key.clone(), dynamic_to_v1_0(value));
            }
        }
    }
    let kind = map.get("component").and_then(Value::as_str).unwrap_or_default();
    let from = component_catalog(catalog, kind);
    if from != FORMS_CATALOG_ID {
        out.insert("@catalogId".to_string(), json!(from));
    }
    Value::Object(out)
}

/// A drawing's messages in `version`'s wire shape. In 1.0, a surface's `createSurface`, its
/// first `updateComponents` and a whole-model `updateDataModel` that follow it become one
/// `createSurface`; the surface's default catalog is `forms`, and each extension component
/// names its own.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{A2uiMessage, A2uiVersion, Catalog, to_wire};
/// use serde_json::json;
///
/// let messages = vec![
///     A2uiMessage::create_surface("s"),
///     A2uiMessage::update_components("s", vec![json!({ "id": "root", "component": "Text", "text": "Hi" })]),
///     A2uiMessage::update_data_model("s", None, Some(json!({}))),
/// ];
/// let wire = to_wire(&messages, Catalog::forms(), A2uiVersion::V1_0);
/// assert_eq!(wire.len(), 1);
/// assert_eq!(wire[0]["version"], "1.0");
/// assert_eq!(wire[0]["createSurface"]["components"][0]["text"], "Hi");
/// assert_eq!(to_wire(&messages, Catalog::forms(), A2uiVersion::V0_9_1).len(), 3);
/// ```
pub fn to_wire(messages: &[A2uiMessage], catalog: &Catalog, version: A2uiVersion) -> Vec<Value> {
    if version == A2uiVersion::V0_9_1 {
        return messages
            .iter()
            .map(|message| serde_json::to_value(message).unwrap_or_default())
            .collect();
    }
    let mut out: Vec<Value> = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        index += 1;
        let body = match &message.body {
            A2uiBody::CreateSurface(create) => {
                let mut body = Map::new();
                body.insert("surfaceId".to_string(), json!(create.surface_id));
                body.insert("catalogId".to_string(), json!(FORMS_CATALOG_ID));
                if let Some(send) = create.send_data_model {
                    body.insert("sendDataModel".to_string(), json!(send));
                }
                body.insert(
                    "metadata".to_string(),
                    json!({ "extensions": {
                        DISPLAY_ONLY_EXTENSION: { "version": DISPLAY_ONLY_EXTENSION_VERSION }
                    } }),
                );
                // One message for a new surface: its components and whole data model inline.
                if let Some(A2uiBody::UpdateComponents(update)) =
                    messages.get(index).map(|m| &m.body)
                    && update.surface_id == create.surface_id
                {
                    body.insert(
                        "components".to_string(),
                        Value::Array(
                            update
                                .components
                                .iter()
                                .map(|c| component_to_v1_0(c, catalog))
                                .collect(),
                        ),
                    );
                    index += 1;
                    if let Some(A2uiBody::UpdateDataModel(data)) =
                        messages.get(index).map(|m| &m.body)
                        && data.surface_id == create.surface_id
                        && data.path.as_deref().is_none_or(|path| path == "/")
                        && let Some(value) = &data.value
                    {
                        body.insert("dataModel".to_string(), value.clone());
                        index += 1;
                    }
                }
                json!({ "createSurface": body })
            }
            A2uiBody::UpdateComponents(update) => json!({
                "updateComponents": {
                    "surfaceId": update.surface_id,
                    "components": update
                        .components
                        .iter()
                        .map(|component| component_to_v1_0(component, catalog))
                        .collect::<Vec<_>>(),
                }
            }),
            A2uiBody::UpdateDataModel(data) => {
                let mut body = Map::new();
                body.insert("surfaceId".to_string(), json!(data.surface_id));
                if let Some(path) = &data.path {
                    body.insert("path".to_string(), json!(path));
                }
                body.insert("value".to_string(), data.value.clone().unwrap_or(Value::Null));
                json!({ "updateDataModel": body })
            }
            A2uiBody::DeleteSurface(delete) => {
                json!({ "deleteSurface": { "surfaceId": delete.surface_id } })
            }
        };
        let mut body = body;
        body["version"] = json!(A2uiVersion::V1_0.wire());
        out.push(body);
    }
    out
}

/// A dynamic value read from 1.0's syntax into the internal form.
fn dynamic_from_v1_0(value: &Value, catalog: &Catalog, at: &str) -> Result<Value, WireError> {
    match value {
        Value::Object(map) if map.contains_key("@path") => {
            let path = map.get("@path").filter(|path| path.is_string()).ok_or_else(|| {
                WireError::new(at, "@path is a data model path, a string such as \"/total\"")
            })?;
            if map.len() > 1 {
                return Err(WireError::new(at, "a binding has only @path"));
            }
            Ok(json!({ "path": path }))
        }
        Value::Object(map) if map.contains_key("@call") => {
            let call = map.get("@call").and_then(Value::as_str).ok_or_else(|| {
                WireError::new(
                    at,
                    "@call names a catalog function, a string such as \"formatNumber\"",
                )
            })?;
            if call == "@index" {
                return Err(WireError::new(
                    at,
                    "@index is not available in a drawing; put the index in the data model instead",
                ));
            }
            let mut out = Map::new();
            out.insert("call".to_string(), json!(call));
            for (key, value) in map {
                match key.as_str() {
                    "@call" => {}
                    "args" => {
                        out.insert(
                            "args".to_string(),
                            dynamic_from_v1_0(value, catalog, &format!("{at}/args"))?,
                        );
                    }
                    "catalogId" => {
                        let id = value.as_str().unwrap_or_default();
                        if id != FORMS_CATALOG_ID && id != catalog.id() {
                            return Err(WireError::new(
                                &format!("{at}/catalogId"),
                                format!("functions come from {FORMS_CATALOG_ID}, not {id}"),
                            ));
                        }
                    }
                    other => {
                        return Err(WireError::new(
                            at,
                            format!("a function call has @call, args and catalogId, not {other}"),
                        ));
                    }
                }
            }
            Ok(Value::Object(out))
        }
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                let here = format!("{at}/{}", key.replace('~', "~0").replace('/', "~1"));
                let key = if let Some(literal) = key.strip_prefix("@@") {
                    format!("@{literal}")
                } else if key.starts_with('@') {
                    return Err(WireError::new(
                        &here,
                        format!(
                            "{key} is a reserved key here; write \"@{key}\" for a literal \"{key}\""
                        ),
                    ));
                } else {
                    key.clone()
                };
                out.insert(key, dynamic_from_v1_0(value, catalog, &here)?);
            }
            if is_binding(&out) || is_call(&out) {
                return Err(WireError::new(
                    at,
                    "a literal object with a `path` or `call` key cannot be drawn: bind it from the \
                     data model instead",
                ));
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| dynamic_from_v1_0(item, catalog, &format!("{at}/{index}")))
            .collect(),
        other => Ok(other.clone()),
    }
}

/// One component read from 1.0's syntax into the internal form of a drawing in `catalog`.
fn component_from_v1_0(component: &Value, catalog: &Catalog, at: &str) -> Result<Value, WireError> {
    let Value::Object(map) = component else {
        return Err(WireError::new(at, "a component is an object with an id and a component"));
    };
    let kind = map.get("component").and_then(Value::as_str).unwrap_or_default();
    let mut out = Map::new();
    for (key, value) in map {
        let here = format!("{at}/{key}");
        match key.as_str() {
            "id" | "component" => {
                out.insert(key.clone(), value.clone());
            }
            "@accessibility" | "@checks" => {
                let bare = &key[1..];
                if map.contains_key(bare) {
                    return Err(WireError::new(
                        &here,
                        format!("a component has {key} or {bare}, not both"),
                    ));
                }
                out.insert(bare.to_string(), dynamic_from_v1_0(value, catalog, &here)?);
            }
            "@catalogId" => {
                let id = value.as_str().unwrap_or_default();
                let expected = component_catalog(catalog, kind);
                if id != expected && id != catalog.id() {
                    return Err(WireError::new(
                        &here,
                        format!("{kind} comes from {expected}, not {id}"),
                    ));
                }
            }
            "@metadata" => {
                return Err(WireError::new(
                    &here,
                    "a drawing carries no extension metadata; leave @metadata out",
                ));
            }
            other if other.starts_with('@') => {
                return Err(WireError::new(
                    &here,
                    format!(
                        "{other} is not a component key; a component's reserved keys are \
                             @accessibility, @checks and @catalogId"
                    ),
                ));
            }
            _ => {
                out.insert(key.clone(), dynamic_from_v1_0(value, catalog, &here)?);
            }
        }
    }
    Ok(Value::Object(out))
}

/// Components written in either version, in the internal form of a drawing in `catalog`:
/// 1.0's syntax is read when any reserved `@` key appears, and v0.9.1's is kept as it is.
///
/// # Errors
///
/// Returns a [`WireError`] naming the first value 1.0's syntax does not allow, or that the
/// internal form would read differently.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{Catalog, components_in};
/// use serde_json::json;
///
/// let read = components_in(
///     vec![json!({ "id": "root", "component": "Text", "text": { "@path": "/t" } })],
///     Catalog::forms(),
/// )?;
/// assert_eq!(read[0]["text"], json!({ "path": "/t" }));
/// # Ok::<(), adk_surface::genui::WireError>(())
/// ```
pub fn components_in(components: Vec<Value>, catalog: &Catalog) -> Result<Vec<Value>, WireError> {
    if !is_v1_0_syntax(&components) {
        return Ok(components);
    }
    components
        .iter()
        .enumerate()
        .map(|(index, component)| component_from_v1_0(component, catalog, &format!("/{index}")))
        .collect()
}

/// Checks a 1.0 `createSurface.metadata`: nothing but the display-only claim, which every
/// drawing this home keeps follows anyway. Any other extension would ask the renderer for
/// behavior the stored drawing cannot carry, so it is refused rather than dropped.
fn display_only_claim(metadata: &Value) -> Result<(), WireError> {
    const AT: &str = "/createSurface/metadata";
    let refuse = |what: &str| {
        Err(WireError::new(
            AT,
            format!(
                "a drawing's metadata holds only the {DISPLAY_ONLY_EXTENSION} extension, not {what}"
            ),
        ))
    };
    let Some(metadata) = metadata.as_object() else { return refuse("a non-object") };
    if let Some(key) = metadata.keys().find(|key| *key != "extensions") {
        return refuse(key);
    }
    let Some(extensions) = metadata.get("extensions") else { return Ok(()) };
    let Some(extensions) = extensions.as_object() else { return refuse("a non-object extensions") };
    match extensions.keys().find(|key| *key != DISPLAY_ONLY_EXTENSION) {
        Some(key) => refuse(key),
        None => Ok(()),
    }
}

/// One message from the wire, in either version, as internal messages: a 1.0 `createSurface`
/// that carries components and a data model becomes three.
///
/// # Errors
///
/// Returns a [`WireError`] for an unknown version, a message `genui` does not read (a function
/// call or response), or a component 1.0's syntax does not allow.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{A2uiBody, Catalog, from_wire};
/// use serde_json::json;
///
/// let read = from_wire(
///     &json!({ "version": "1.0", "createSurface": {
///         "surfaceId": "s", "components": [{ "id": "root", "component": "Text", "text": "Hi" }],
///         "dataModel": { "a": 1 } } }),
///     Catalog::forms(),
/// )?;
/// assert_eq!(read.len(), 3);
/// assert!(matches!(read[1].body, A2uiBody::UpdateComponents(_)));
/// # Ok::<(), adk_surface::genui::WireError>(())
/// ```
pub fn from_wire(message: &Value, catalog: &Catalog) -> Result<Vec<A2uiMessage>, WireError> {
    let version =
        message.get("version").and_then(Value::as_str).and_then(A2uiVersion::of_wire).ok_or_else(
            || WireError::new("/version", "an A2UI message names version \"1.0\" or \"v0.9.1\""),
        )?;
    if version == A2uiVersion::V0_9_1 {
        let read: A2uiMessage = serde_json::from_value(message.clone())
            .map_err(|error| WireError::new("/", error.to_string()))?;
        return Ok(vec![read]);
    }
    let Value::Object(map) = message else {
        return Err(WireError::new("/", "an A2UI message is an object"));
    };
    let mut bodies = map.iter().filter(|(key, _)| key.as_str() != "version");
    let (Some((kind, body)), None) = (bodies.next(), bodies.next()) else {
        return Err(WireError::new("/", "an A2UI message has its version and one body"));
    };
    let surface = |body: &Value| -> Result<String, WireError> {
        body.get("surfaceId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| WireError::new(&format!("/{kind}/surfaceId"), "a surfaceId is required"))
    };
    let components = |body: &Value, at: &str| -> Result<Vec<Value>, WireError> {
        body.get("components")
            .and_then(Value::as_array)
            .ok_or_else(|| WireError::new(at, "components is a list"))?
            .iter()
            .enumerate()
            .map(|(index, component)| {
                component_from_v1_0(component, catalog, &format!("{at}/{index}"))
            })
            .collect()
    };
    match kind.as_str() {
        "createSurface" => {
            let surface_id = surface(body)?;
            if body.get("theme").is_some() {
                return Err(WireError::new("/createSurface/theme", "A2UI 1.0 has no theme"));
            }
            if let Some(metadata) = body.get("metadata") {
                display_only_claim(metadata)?;
            }
            if let Some(id) = body.get("catalogId").and_then(Value::as_str)
                && id != FORMS_CATALOG_ID
                && id != catalog.id()
            {
                return Err(WireError::new(
                    "/createSurface/catalogId",
                    format!("this home draws {FORMS_CATALOG_ID} and {}, not {id}", catalog.id()),
                ));
            }
            let mut create = A2uiMessage::create_surface(surface_id.clone());
            if let A2uiBody::CreateSurface(inner) = &mut create.body {
                inner.catalog_id = catalog.id().to_string();
                inner.send_data_model = body.get("sendDataModel").and_then(Value::as_bool);
            }
            let mut out = vec![create];
            if body.get("components").is_some() {
                out.push(A2uiMessage::update_components(
                    surface_id.clone(),
                    components(body, "/createSurface/components")?,
                ));
            }
            if let Some(model) = body.get("dataModel") {
                out.push(A2uiMessage::update_data_model(surface_id, None, Some(model.clone())));
            }
            Ok(out)
        }
        "updateComponents" => Ok(vec![A2uiMessage::update_components(
            surface(body)?,
            components(body, "/updateComponents/components")?,
        )]),
        "updateDataModel" => {
            if body.get("value").is_none() {
                return Err(WireError::new(
                    "/updateDataModel/value",
                    "A2UI 1.0 requires value; null removes the path",
                ));
            }
            let value = body.get("value").filter(|value| !value.is_null()).cloned();
            let path = body.get("path").and_then(Value::as_str).map(str::to_string);
            Ok(vec![A2uiMessage::update_data_model(surface(body)?, path, value)])
        }
        "deleteSurface" => Ok(vec![A2uiMessage::delete_surface(surface(body)?)]),
        other => Err(WireError::new(
            &format!("/{other}"),
            format!("{other} is not a message a drawing is made of"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawing() -> Vec<A2uiMessage> {
        vec![
            A2uiMessage::create_surface("s"),
            A2uiMessage::update_components(
                "s",
                vec![
                    json!({ "id": "root", "component": "Column", "children": ["t", "list"] }),
                    json!({ "id": "t", "component": "Text",
                        "text": { "call": "formatString", "args": { "value": "Hi ${/name}" }, "returnType": "string" },
                        "accessibility": { "label": { "path": "/name" } } }),
                    json!({ "id": "list", "component": "List",
                        "children": { "componentId": "row", "path": "/rows" } }),
                    json!({ "id": "row", "component": "Text", "text": { "path": "label" } }),
                    json!({ "id": "f", "component": "TextField", "label": "Name", "value": { "path": "/name" },
                        "checks": [{ "condition": { "call": "required", "args": { "value": { "path": "/name" } } },
                            "message": "Required" }] }),
                ],
            ),
            A2uiMessage::update_data_model("s", None, Some(json!({ "name": "Ana", "rows": [] }))),
            A2uiMessage::update_data_model("s", Some("/name".to_string()), None),
        ]
    }

    #[test]
    fn a_drawing_is_written_in_1_0_with_one_create_and_reserved_keys() {
        let wire = to_wire(&drawing(), Catalog::forms(), A2uiVersion::V1_0);
        assert_eq!(wire.len(), 2, "{wire:#?}");
        let create = &wire[0]["createSurface"];
        assert_eq!(wire[0]["version"], "1.0");
        assert_eq!(create["catalogId"], FORMS_CATALOG_ID);
        assert_eq!(
            create["metadata"],
            json!({ "extensions": { "adkrust_display_only": { "version": "1.0" } } })
        );
        assert_eq!(create["dataModel"]["name"], "Ana");
        let components = create["components"].as_array().unwrap();
        assert_eq!(components[1]["text"]["@call"], "formatString");
        assert!(components[1]["text"].get("returnType").is_none());
        assert_eq!(components[1]["@accessibility"]["label"], json!({ "@path": "/name" }));
        // A template keeps its plain `path`; a relative binding takes the prefix.
        assert_eq!(components[2]["children"], json!({ "componentId": "row", "path": "/rows" }));
        assert_eq!(components[3]["text"], json!({ "@path": "label" }));
        assert_eq!(
            components[4]["@checks"][0]["condition"]["args"]["value"],
            json!({ "@path": "/name" })
        );
        // A removal writes value null, which 1.0 requires.
        assert_eq!(
            wire[1]["updateDataModel"],
            json!({ "surfaceId": "s", "path": "/name", "value": null })
        );
    }

    #[test]
    fn what_1_0_writes_reads_back_to_the_same_drawing() {
        let original = drawing();
        let wire = to_wire(&original, Catalog::forms(), A2uiVersion::V1_0);
        let read: Vec<A2uiMessage> =
            wire.iter().flat_map(|message| from_wire(message, Catalog::forms()).unwrap()).collect();
        // Only the call's returnType, which 1.0 leaves to the catalog, is not carried.
        let mut expected = original;
        if let A2uiBody::UpdateComponents(update) = &mut expected[1].body {
            update.components[1]["text"].as_object_mut().unwrap().remove("returnType");
        }
        assert_eq!(read, expected);
    }

    #[test]
    fn v0_9_1_is_written_as_stored() {
        let messages = drawing();
        let wire = to_wire(&messages, Catalog::forms(), A2uiVersion::V0_9_1);
        assert_eq!(wire.len(), 4);
        assert_eq!(wire[0]["version"], "v0.9.1");
        assert_eq!(from_wire(&wire[1], Catalog::forms()).unwrap()[0], messages[1]);
    }

    #[test]
    fn literal_at_keys_are_escaped_and_unescaped() {
        let component =
            json!({ "id": "t", "component": "Text", "text": "x", "extra": { "@type": "a" } });
        let wire = component_to_v1_0(&component, Catalog::forms());
        assert_eq!(wire["extra"], json!({ "@@type": "a" }));
        let read = components_in(vec![wire], Catalog::forms()).unwrap();
        assert_eq!(read[0]["extra"], json!({ "@type": "a" }));
    }

    #[test]
    fn reading_1_0_refuses_what_the_internal_form_would_misread() {
        for (component, says) in [
            (
                json!({ "id": "t", "component": "Text", "text": { "@path": "/a" }, "x": { "path": "/b" } }),
                "literal object",
            ),
            (
                json!({ "id": "t", "component": "Text", "text": { "@path": "/a" }, "x": { "call": "y" } }),
                "literal object",
            ),
            (
                json!({ "id": "t", "component": "Text", "text": { "@path": "/a" }, "x": { "@type": "a" } }),
                "reserved key",
            ),
            (json!({ "id": "t", "component": "Text", "text": { "@call": "@index" } }), "@index"),
            (
                json!({ "id": "t", "component": "Text", "text": "a", "@metadata": { "extensions": {} } }),
                "metadata",
            ),
            (
                json!({ "id": "t", "component": "Text", "text": "a", "@style": 1 }),
                "not a component key",
            ),
            (
                json!({ "id": "t", "component": "Text", "text": "a", "@catalogId": "https://x.test/c.json" }),
                "comes from",
            ),
            (
                json!({ "id": "t", "component": "Text", "text": "a", "@checks": [], "checks": [] }),
                "not both",
            ),
        ] {
            let error = components_in(vec![component.clone()], Catalog::forms()).unwrap_err();
            assert!(error.to_string().contains(says), "{component}: {error}");
        }
    }

    #[test]
    fn reading_1_0_messages_refuses_theme_metadata_and_missing_values() {
        for (message, says) in [
            (
                json!({ "version": "1.0", "createSurface": { "surfaceId": "s", "theme": {} } }),
                "theme",
            ),
            (
                json!({ "version": "1.0", "createSurface": { "surfaceId": "s", "metadata": { "extensions": { "org_example_theme": {} } } } }),
                "not org_example_theme",
            ),
            (
                json!({ "version": "1.0", "createSurface": { "surfaceId": "s", "metadata": { "theme": {} } } }),
                "not theme",
            ),
            (
                json!({ "version": "1.0", "createSurface": { "surfaceId": "s", "catalogId": "https://x.test/c.json" } }),
                "not https://x.test",
            ),
            (
                json!({ "version": "1.0", "updateDataModel": { "surfaceId": "s", "path": "/a" } }),
                "requires value",
            ),
            (
                json!({ "version": "1.0", "callRendererFunction": { "functionCallId": "1" } }),
                "not a message",
            ),
            (json!({ "version": "2.0", "deleteSurface": { "surfaceId": "s" } }), "version"),
        ] {
            let error = from_wire(&message, Catalog::forms()).unwrap_err();
            assert!(error.to_string().contains(says), "{message}: {error}");
        }
    }

    #[test]
    fn versions_are_named_as_the_wire_names_them() {
        for (name, version) in [
            ("v0.9", A2uiVersion::V0_9_1),
            ("v0.9.1", A2uiVersion::V0_9_1),
            ("1.0", A2uiVersion::V1_0),
        ] {
            assert_eq!(A2uiVersion::of_wire(name), Some(version));
        }
        assert_eq!(A2uiVersion::of_wire("0.8"), None);
        assert_eq!(A2uiVersion::V0_9_1.capability_key(), "v0.9");
        assert_eq!(A2uiVersion::V1_0.capability_key(), "1.0");
    }
}
