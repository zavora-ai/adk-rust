//! The render tools (spec 3.3 §6.1 `genui`, §7.4 rule 5): what an agent draws with, each
//! drawing checked whole by the [`Validator`] and recorded under [`DRAWN_STATE_KEY`].

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use adk_core::{
    AdkError, DRAW_TOOL_NAMES, ErrorCategory, ErrorComponent, ReadonlyContext, Result,
    TOOL_INVALID_ARGUMENTS_CODE, Tool, ToolContext, ToolEffect, Toolset,
};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::a2ui::{A2uiBody, A2uiMessage, DataModel};
use super::drawn::DrawnKind;
use super::validate::{DrawRefusal, DrawnAction, SurfaceDraft, Validator};

/// The reserved session state key every drawing writes, `{ surfaceId, version, digest }`:
/// the proof that a drawing tool result came from these tools, checked against the digest of
/// the result's `drawn` record (spec 3.3 §6.1 `genui`).
pub const DRAWN_STATE_KEY: &str = "adk.surface.drawn";

/// The reserved session state key a refused drawing writes, its [`DrawRefusal`]: the proof
/// that a render tool's error came from these tools, so the thread shows the refusal's plain
/// notice and an MCP server's tool of the same name stays an ordinary failed result
/// (spec 3.3 §7.4 rule 5).
pub const REFUSED_STATE_KEY: &str = "adk.surface.refused";

/// The prefix of the session state keys holding the latest version of each surface a
/// session drew, `adk.surface.versions.<surface id>`, so versions keep rising after a
/// restart or a compaction.
const VERSIONS_STATE_KEY: &str = "adk.surface.versions";

/// What [`DRAWN_STATE_KEY`] holds: the latest drawing of the session.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::DrawnRecord;
///
/// let record = DrawnRecord { surface_id: "invoice-10".into(), version: 2, digest: "ab".into() };
/// assert_eq!(serde_json::to_value(&record).unwrap()["surfaceId"], "invoice-10");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnRecord {
    /// The surface drawn.
    pub surface_id: String,
    /// Its version.
    pub version: u64,
    /// The SHA-256 of the drawing's [`DrawnEnvelope`], hex.
    pub digest: String,
}

/// One drawing as a drawing tool's result records it, under `drawn`: the A2UI messages that
/// rebuild the whole surface, and the actions its buttons ask for (spec 3.3 §6.1 `genui`).
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{A2uiMessage, DrawnEnvelope, DrawnKind, FORMS_CATALOG_ID};
///
/// let envelope = DrawnEnvelope {
///     surface_id: "s".into(),
///     version: 1,
///     kind: DrawnKind::Card,
///     catalog_id: FORMS_CATALOG_ID.into(),
///     title: None,
///     messages: vec![A2uiMessage::create_surface("s")],
///     actions: Vec::new(),
///     deleted: false,
/// };
/// assert_eq!(envelope.digest(), envelope.clone().digest());
/// assert_eq!(envelope.digest().len(), 64);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnEnvelope {
    /// The surface.
    pub surface_id: String,
    /// Its version: one more than any earlier drawing of the surface in the session.
    pub version: u64,
    /// What it is.
    pub kind: DrawnKind,
    /// The catalog it was checked against.
    pub catalog_id: String,
    /// The title above it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The A2UI messages that draw it: `createSurface`, `updateComponents` and
    /// `updateDataModel`, or `deleteSurface`.
    pub messages: Vec<A2uiMessage>,
    /// The actions its buttons ask for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<DrawnAction>,
    /// Whether this version removes the surface.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

impl DrawnEnvelope {
    /// The SHA-256 of the envelope's canonical JSON (keys sorted), hex.
    pub fn digest(&self) -> String {
        let mut canonical = String::new();
        write_canonical(&serde_json::to_value(self).unwrap_or_default(), &mut canonical);
        let hash = Sha256::digest(canonical.as_bytes());
        hash.iter().fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    }

    /// The drawing its messages describe, or `None` for a removal.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_surface::genui::{A2uiMessage, DrawnEnvelope, DrawnKind, FORMS_CATALOG_ID};
    /// use serde_json::json;
    ///
    /// let envelope = DrawnEnvelope {
    ///     surface_id: "s".into(),
    ///     version: 1,
    ///     kind: DrawnKind::Card,
    ///     catalog_id: FORMS_CATALOG_ID.into(),
    ///     title: None,
    ///     messages: vec![
    ///         A2uiMessage::create_surface("s"),
    ///         A2uiMessage::update_components(
    ///             "s",
    ///             vec![json!({ "id": "root", "component": "Text", "text": { "path": "/t" } })],
    ///         ),
    ///         A2uiMessage::update_data_model("s", None, Some(json!({ "t": "Hello" }))),
    ///     ],
    ///     actions: Vec::new(),
    ///     deleted: false,
    /// };
    /// assert_eq!(envelope.draft().unwrap().data_model, json!({ "t": "Hello" }));
    /// ```
    pub fn draft(&self) -> Option<SurfaceDraft> {
        if self.deleted {
            return None;
        }
        let mut components = Vec::new();
        let mut model = DataModel::default();
        for message in
            self.messages.iter().filter(|message| message.surface_id() == self.surface_id)
        {
            match &message.body {
                A2uiBody::CreateSurface(_) => {}
                A2uiBody::UpdateComponents(update) => components.clone_from(&update.components),
                A2uiBody::UpdateDataModel(update) => {
                    model.apply(update.path.as_deref(), update.value.clone()).ok()?;
                }
                A2uiBody::DeleteSurface(_) => return None,
            }
        }
        Some(SurfaceDraft {
            surface_id: self.surface_id.clone(),
            kind: self.kind,
            title: self.title.clone(),
            components,
            data_model: model.value().clone(),
            actions: self.actions.clone(),
        })
    }

    /// The envelope a drawing tool's `response` records under `drawn`, if any.
    pub fn from_response(response: &Value) -> Option<Self> {
        serde_json::from_value(response.get("drawn")?.clone()).ok()
    }

    /// The envelope of `draft`, at `version`.
    fn of(draft: &SurfaceDraft, version: u64, catalog_id: &str) -> Self {
        let surface = draft.surface_id.clone();
        let data_model = (!draft.data_model.is_null()).then(|| draft.data_model.clone());
        let mut create = A2uiMessage::create_surface(surface.clone());
        if let A2uiBody::CreateSurface(body) = &mut create.body {
            body.catalog_id = catalog_id.to_string();
        }
        Self {
            surface_id: surface.clone(),
            version,
            kind: draft.kind,
            catalog_id: catalog_id.to_string(),
            title: draft.title.clone(),
            messages: vec![
                create,
                A2uiMessage::update_components(surface.clone(), draft.components.clone()),
                A2uiMessage::update_data_model(surface, None, data_model),
            ],
            actions: draft.actions.clone(),
            deleted: false,
        }
    }
}

/// Writes `value` as JSON with every object's keys sorted.
fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// The render tools, bound to one catalog through their [`Validator`] (spec 3.3 §6.1
/// `genui`): `render_form`, `render_card`, `render_table`, `render_screen`, `patch_surface`
/// and `delete_surface`.
///
/// Every tool is read-only and safe to run beside others: drawing changes nothing but what
/// the person's threads show, and a form's Send asks the person before anything runs. Each
/// result records the drawing under `drawn` and writes [`DRAWN_STATE_KEY`]; a refused
/// drawing is a tool error whose message says why in plain words.
///
/// # Example
///
/// ```rust
/// use adk_core::Tool;
/// use adk_surface::genui::DrawToolset;
///
/// let tools = DrawToolset::forms().tools();
/// let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
/// assert_eq!(names, adk_core::DRAW_TOOL_NAMES);
/// assert!(tools.iter().all(|tool| tool.is_read_only() && tool.is_concurrency_safe()));
/// ```
#[derive(Debug, Clone)]
pub struct DrawToolset {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    validator: Validator,
    /// Versions given out in this process, by app, user, session and surface, so drawings
    /// made side by side in one turn get their own.
    issued: Mutex<HashMap<(String, String, String, String), u64>>,
}

impl DrawToolset {
    /// Render tools that check every drawing with `validator`.
    pub fn new(validator: Validator) -> Self {
        Self { shared: Arc::new(Shared { validator, issued: Mutex::new(HashMap::new()) }) }
    }

    /// Render tools on the `forms` 1.0 catalog.
    pub fn forms() -> Self {
        Self::new(Validator::forms())
    }

    /// The tools.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        [Op::Form, Op::Card, Op::Table, Op::Screen, Op::Patch, Op::Delete]
            .into_iter()
            .map(|op| Arc::new(DrawTool { op, shared: Arc::clone(&self.shared) }) as Arc<dyn Tool>)
            .collect()
    }
}

#[async_trait]
impl Toolset for DrawToolset {
    fn name(&self) -> &str {
        "genui"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(DrawToolset::tools(self))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Form,
    Card,
    Table,
    Screen,
    Patch,
    Delete,
}

impl Op {
    /// The tool's name, one of [`DRAW_TOOL_NAMES`].
    fn name(self) -> &'static str {
        match self {
            Self::Form => "render_form",
            Self::Card => "render_card",
            Self::Table => "render_table",
            Self::Screen => "render_screen",
            Self::Patch => "patch_surface",
            Self::Delete => "delete_surface",
        }
    }
}

struct DrawTool {
    op: Op,
    shared: Arc<Shared>,
}

/// A field of a form.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FormField {
    /// The field's name: letters, digits, `_` or `-`. Its value is at the data path `/<name>`.
    name: String,
    /// What the person reads beside it.
    label: String,
    /// `text` (default), `longText`, `number`, `email`, `date`, `checkbox` or `choice`.
    #[serde(rename = "type", default)]
    field_type: FieldType,
    /// The value it starts with.
    #[serde(default)]
    value: Option<Value>,
    /// Whether the person must fill it in.
    #[serde(default)]
    required: bool,
    /// The choices of a `choice` field.
    #[serde(default)]
    options: Vec<ChoiceOption>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
enum FieldType {
    #[default]
    Text,
    LongText,
    Number,
    Email,
    Date,
    Checkbox,
    Choice,
}

/// One choice of a `choice` field.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ChoiceOption {
    /// What the person reads.
    label: String,
    /// The value sent.
    value: String,
}

/// `render_form`: fields and the actions their buttons ask for.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FormParams {
    /// The surface to draw; drawing it again makes a new version. Defaults to one from the title.
    #[serde(default)]
    surface_id: Option<String>,
    /// The form's title.
    title: String,
    /// A line under the title.
    #[serde(default)]
    description: Option<String>,
    /// The fields, in order.
    fields: Vec<FormField>,
    /// What the buttons ask for: `{ id, label, tool, args }`, each argument a constant or
    /// `{ "path": "/<field name>" }`. Sending asks the person before the tool runs.
    actions: Vec<DrawnAction>,
}

/// One row of a card.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CardRow {
    /// The row's label.
    label: String,
    /// Its value: text or a number.
    value: Value,
}

/// `render_card`: label and value rows to read.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CardParams {
    /// The surface to draw. Defaults to one from the title.
    #[serde(default)]
    surface_id: Option<String>,
    /// The card's title.
    title: String,
    /// The rows, in order.
    rows: Vec<CardRow>,
    /// A line under the rows.
    #[serde(default)]
    note: Option<String>,
}

/// `render_table`: columns and rows to read.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TableParams {
    /// The surface to draw. Defaults to one from the title.
    #[serde(default)]
    surface_id: Option<String>,
    /// The table's title.
    title: String,
    /// The column headings.
    columns: Vec<String>,
    /// The rows, each a list of cells: text or numbers.
    rows: Vec<Vec<Value>>,
}

/// `render_screen`: A2UI components of the catalog.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScreenParams {
    /// The surface to draw.
    surface_id: String,
    /// The title above it.
    #[serde(default)]
    title: Option<String>,
    /// A2UI v0.9.1 components of the forms catalog, one with id `root`.
    components: Vec<Value>,
    /// The data model the components bind to.
    #[serde(default)]
    data_model: Option<Value>,
    /// What its buttons ask for, as in `render_form`.
    #[serde(default)]
    actions: Vec<DrawnAction>,
}

/// A change to a drawn surface's data model.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DataChange {
    /// Where, as a data path such as `/total`.
    path: String,
    /// The new value; absent or `null` removes it.
    #[serde(default)]
    value: Option<Value>,
}

/// `patch_surface`: changes to a surface drawn earlier, which make its next version.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PatchParams {
    /// The surface to change.
    surface_id: String,
    /// Data model changes, in order.
    #[serde(default)]
    data: Vec<DataChange>,
    /// Components to add, or to replace by id.
    #[serde(default)]
    components: Vec<Value>,
    /// Ids of components to remove.
    #[serde(default)]
    remove: Vec<String>,
}

/// `delete_surface`: removes a surface drawn earlier.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteParams {
    /// The surface to remove.
    surface_id: String,
}

fn schema_of<T: JsonSchema>() -> Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default();
    if let Some(map) = schema.as_object_mut() {
        map.remove("$schema");
        map.remove("title");
    }
    schema
}

fn invalid(tool: &str, error: impl std::fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        TOOL_INVALID_ARGUMENTS_CODE,
        format!("invalid arguments for '{tool}': {error}. Correct them and call it again."),
    )
}

fn refused(refusal: &DrawRefusal) -> AdkError {
    AdkError::new(ErrorComponent::Tool, ErrorCategory::Forbidden, refusal.code(), refusal.message())
}

fn params<T: DeserializeOwned>(tool: &str, args: Value) -> Result<T> {
    serde_json::from_value(args).map_err(|error| invalid(tool, error))
}

/// A surface id from `title`: `<kind>-` and the title's letters and digits, lowercased.
fn surface_id_from(kind: &str, title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(48).collect();
    if slug.is_empty() { kind.to_string() } else { format!("{kind}-{slug}") }
}

/// The components of a form.
fn form(params: FormParams) -> Result<SurfaceDraft> {
    let surface_id = params.surface_id.unwrap_or_else(|| surface_id_from("form", &params.title));
    let mut components = vec![
        json!({ "id": "root", "component": "Card", "child": "body" }),
        json!({ "id": "title", "component": "Text", "text": params.title, "variant": "h3" }),
    ];
    let mut body = vec![json!("title")];
    if let Some(description) = params.description {
        components.push(json!({ "id": "description", "component": "Text", "text": description }));
        body.push(json!("description"));
    }
    let mut model = serde_json::Map::new();
    for field in params.fields {
        let name = &field.name;
        if name.is_empty()
            || name.len() > 48
            || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(invalid(
                Op::Form.name(),
                format!("field name '{name}' is not 1 to 48 letters, digits, '_' or '-'"),
            ));
        }
        let id = format!("field-{name}");
        let path = format!("/{name}");
        let binding = json!({ "path": path });
        let mut checks = Vec::new();
        if field.required {
            checks.push(json!({
                "condition": { "call": "required", "args": { "value": binding } },
                "message": "Fill this in"
            }));
        }
        let (component, initial) = match field.field_type {
            FieldType::Text | FieldType::Email | FieldType::Number | FieldType::LongText => {
                let variant = match field.field_type {
                    FieldType::LongText => "longText",
                    FieldType::Number => "number",
                    _ => "shortText",
                };
                if field.field_type == FieldType::Email {
                    checks.push(json!({
                        "condition": { "call": "email", "args": { "value": binding } },
                        "message": "Enter an email address"
                    }));
                }
                (
                    json!({
                        "id": id, "component": "TextField", "label": field.label,
                        "value": binding, "variant": variant
                    }),
                    json!(""),
                )
            }
            FieldType::Date => (
                json!({
                    "id": id, "component": "DateTimeInput", "label": field.label,
                    "value": binding, "enableDate": true
                }),
                json!(""),
            ),
            FieldType::Checkbox => (
                json!({
                    "id": id, "component": "CheckBox", "label": field.label, "value": binding
                }),
                json!(false),
            ),
            FieldType::Choice => (
                json!({
                    "id": id, "component": "ChoicePicker", "label": field.label, "value": binding,
                    "options": field.options, "variant": "mutuallyExclusive"
                }),
                json!([]),
            ),
        };
        let mut component = component;
        if !checks.is_empty() {
            component["checks"] = Value::Array(checks);
        }
        model.insert(name.clone(), field.value.unwrap_or(initial));
        components.push(component);
        body.push(json!(id));
    }
    let mut buttons = Vec::new();
    for (index, action) in params.actions.iter().enumerate() {
        let button = format!("action-{}", action.id);
        let label = format!("action-{}-label", action.id);
        components.push(json!({ "id": label, "component": "Text", "text": action.label }));
        components.push(json!({
            "id": button, "component": "Button", "child": label,
            "variant": if index == 0 { "primary" } else { "default" },
            "action": { "event": { "name": action.id } }
        }));
        buttons.push(json!(button));
    }
    if !buttons.is_empty() {
        components.push(json!({ "id": "actions", "component": "Row", "children": buttons }));
        body.push(json!("actions"));
    }
    components.push(json!({ "id": "body", "component": "Column", "children": body }));
    Ok(SurfaceDraft {
        surface_id,
        kind: DrawnKind::Form,
        title: Some(params.title),
        components,
        data_model: Value::Object(model),
        actions: params.actions,
    })
}

/// The components of a card.
fn card(params: CardParams) -> SurfaceDraft {
    let surface_id = params.surface_id.unwrap_or_else(|| surface_id_from("card", &params.title));
    let mut components = vec![
        json!({ "id": "root", "component": "Card", "child": "body" }),
        json!({ "id": "title", "component": "Text", "text": params.title, "variant": "h3" }),
    ];
    let mut body = vec![json!("title")];
    for (index, row) in params.rows.into_iter().enumerate() {
        let value = match row.value {
            Value::String(text) => text,
            other => other.to_string(),
        };
        components.push(json!({
            "id": format!("row-{index}-label"), "component": "Text", "text": row.label,
            "variant": "caption"
        }));
        components.push(
            json!({ "id": format!("row-{index}-value"), "component": "Text", "text": value }),
        );
        components.push(json!({
            "id": format!("row-{index}"), "component": "Row", "justify": "spaceBetween",
            "children": [format!("row-{index}-label"), format!("row-{index}-value")]
        }));
        body.push(json!(format!("row-{index}")));
    }
    if let Some(note) = params.note {
        components
            .push(json!({ "id": "note", "component": "Text", "text": note, "variant": "caption" }));
        body.push(json!("note"));
    }
    components.push(json!({ "id": "body", "component": "Column", "children": body }));
    SurfaceDraft {
        surface_id,
        kind: DrawnKind::Card,
        title: Some(params.title),
        components,
        data_model: json!({}),
        actions: Vec::new(),
    }
}

/// The components of a table: a header row, then one row per item of `/rows`, rendered by a
/// template.
fn table(params: TableParams) -> SurfaceDraft {
    let surface_id = params.surface_id.unwrap_or_else(|| surface_id_from("table", &params.title));
    let mut components = vec![
        json!({ "id": "root", "component": "Card", "child": "body" }),
        json!({ "id": "title", "component": "Text", "text": params.title, "variant": "h3" }),
    ];
    let mut header = Vec::new();
    let mut cells = Vec::new();
    for (index, column) in params.columns.iter().enumerate() {
        components.push(json!({
            "id": format!("head-{index}"), "component": "Text", "text": column,
            "variant": "caption"
        }));
        components.push(json!({
            "id": format!("cell-{index}"), "component": "Text",
            "text": { "path": index.to_string() }
        }));
        header.push(json!(format!("head-{index}")));
        cells.push(json!(format!("cell-{index}")));
    }
    let rows: Vec<Value> = params
        .rows
        .into_iter()
        .map(|row| {
            Value::Array(
                row.into_iter()
                    .map(|cell| match cell {
                        Value::String(text) => Value::String(text),
                        other => Value::String(other.to_string()),
                    })
                    .collect(),
            )
        })
        .collect();
    components.push(json!({ "id": "header", "component": "Row", "children": header }));
    components.push(json!({ "id": "row", "component": "Row", "children": cells }));
    components.push(json!({
        "id": "rows", "component": "List",
        "children": { "componentId": "row", "path": "/rows" }
    }));
    components.push(
        json!({ "id": "body", "component": "Column", "children": ["title", "header", "rows"] }),
    );
    SurfaceDraft {
        surface_id,
        kind: DrawnKind::Table,
        title: Some(params.title),
        components,
        data_model: json!({ "rows": rows }),
        actions: Vec::new(),
    }
}

impl DrawTool {
    fn description(&self) -> &'static str {
        match self.op {
            Op::Form => {
                "Show the person a form in this thread. Each field's value is at the data path \
                 /<name>. Each action is a button that asks to run one of your tools with \
                 constants and field values; pressing it never runs the tool: it asks the \
                 person first, on the home's own card. Never draw decision words or controls \
                 (Allow once, Deny, Approve...), links, or password fields."
            }
            Op::Card => {
                "Show the person a card in this thread: a title and label and value rows. Plain \
                 text only; no buttons, links or decision words."
            }
            Op::Table => {
                "Show the person a table in this thread: a title, column headings and rows of \
                 text or numbers. No buttons, links or decision words."
            }
            Op::Screen => {
                "Show the person a screen of A2UI v0.9.1 components from the forms catalog \
                 (Text, Row, Column, List, Card, Tabs, Divider, Button, TextField, CheckBox, \
                 ChoicePicker, Slider, DateTimeInput), one with id root. Buttons dispatch only \
                 declared actions, which ask the person before a tool runs."
            }
            Op::Patch => {
                "Change a surface you drew in this session: set or remove data model values, \
                 add or replace components by id, or remove components. The whole surface is \
                 checked again."
            }
            Op::Delete => "Remove a surface you drew in this session from the person's thread.",
        }
    }

    /// The latest drawing of `surface_id` in the session, from earlier tool results.
    fn latest(&self, ctx: &dyn ToolContext, surface_id: &str) -> Option<DrawnEnvelope> {
        let history = ctx.session()?.conversation_history();
        history
            .iter()
            .flat_map(|content| &content.parts)
            .filter_map(|part| match part {
                adk_core::Part::FunctionResponse { function_response, .. }
                    if DRAW_TOOL_NAMES.contains(&function_response.name.as_str()) =>
                {
                    DrawnEnvelope::from_response(&function_response.response)
                }
                _ => None,
            })
            .filter(|envelope| envelope.surface_id == surface_id)
            .max_by_key(|envelope| envelope.version)
    }

    /// The next version of `surface_id`: one more than any version the session state, the
    /// session's earlier results or this process gave it.
    fn next_version(
        &self,
        ctx: &dyn ToolContext,
        surface_id: &str,
        latest: Option<&DrawnEnvelope>,
    ) -> u64 {
        let stored = ctx
            .session()
            .and_then(|session| session.state().get(&format!("{VERSIONS_STATE_KEY}.{surface_id}")))
            .and_then(|version| version.as_u64())
            .unwrap_or(0);
        let seen = latest.map_or(0, |envelope| envelope.version);
        let key = (
            ctx.app_name().to_string(),
            ctx.user_id().to_string(),
            ctx.session_id().to_string(),
            surface_id.to_string(),
        );
        let mut issued =
            self.shared.issued.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let version =
            stored.max(seen).max(issued.get(&key).copied().unwrap_or(0)).saturating_add(1);
        issued.insert(key, version);
        version
    }

    /// Records `envelope` as the session's latest drawing and answers the call.
    fn shown(&self, ctx: &dyn ToolContext, envelope: DrawnEnvelope) -> Value {
        let record = DrawnRecord {
            surface_id: envelope.surface_id.clone(),
            version: envelope.version,
            digest: envelope.digest(),
        };
        let mut actions = ctx.actions();
        actions.state_delta.insert(DRAWN_STATE_KEY.to_string(), json!(record));
        // One key per surface: drawings made side by side in one turn each keep theirs.
        actions.state_delta.insert(
            format!("{VERSIONS_STATE_KEY}.{}", envelope.surface_id),
            json!(envelope.version),
        );
        ctx.set_actions(actions);
        tracing::info!(
            surface.id = %envelope.surface_id,
            surface.version = envelope.version,
            surface.deleted = envelope.deleted,
            "drawing recorded"
        );
        let message = if envelope.deleted {
            format!("Removed '{}' from the person's thread.", envelope.surface_id)
        } else if envelope.actions.is_empty() {
            format!(
                "Shown to the person as version {} of '{}'.",
                envelope.version, envelope.surface_id
            )
        } else {
            format!(
                "Shown to the person as version {} of '{}'. Sending asks them first; you are told \
                 when they decide.",
                envelope.version, envelope.surface_id
            )
        };
        json!({
            "status": if envelope.deleted { "removed" } else { "shown" },
            "message": message,
            "drawn": envelope,
        })
    }

    /// Checks `draft` and records it as the next version of its surface.
    fn draw(&self, ctx: &dyn ToolContext, draft: SurfaceDraft) -> Result<Value> {
        let validator = &self.shared.validator;
        if let Err(refusal) = validator.validate(&draft) {
            tracing::info!(
                surface.id = %draft.surface_id,
                refusal.code = refusal.code(),
                refusal.reason = %refusal.technical(),
                "drawing refused"
            );
            let mut actions = ctx.actions();
            actions.state_delta.insert(REFUSED_STATE_KEY.to_string(), json!(refusal));
            ctx.set_actions(actions);
            return Err(refused(&refusal));
        }
        let latest = self.latest(ctx, &draft.surface_id);
        let version = self.next_version(ctx, &draft.surface_id, latest.as_ref());
        Ok(self.shown(ctx, DrawnEnvelope::of(&draft, version, validator.catalog().id())))
    }

    fn patch(&self, ctx: &dyn ToolContext, params: PatchParams) -> Result<Value> {
        let tool = Op::Patch.name();
        let latest = self.latest(ctx, &params.surface_id);
        let Some(mut draft) = latest.as_ref().and_then(DrawnEnvelope::draft) else {
            return Err(invalid(
                tool,
                format!(
                    "no surface '{}' is shown in this session; draw it first",
                    params.surface_id
                ),
            ));
        };
        let mut model = DataModel::new(draft.data_model.clone());
        for change in params.data {
            model.apply(Some(&change.path), change.value).map_err(|error| invalid(tool, error))?;
        }
        draft.data_model = model.value().clone();
        draft.components.retain(|component| {
            let id = component.get("id").and_then(Value::as_str).unwrap_or_default();
            !params.remove.iter().any(|removed| removed == id)
        });
        for component in params.components {
            let id = component.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            match draft
                .components
                .iter_mut()
                .find(|existing| existing.get("id").and_then(Value::as_str) == Some(id.as_str()))
            {
                Some(existing) => *existing = component,
                None => draft.components.push(component),
            }
        }
        self.draw(ctx, draft)
    }

    fn delete(&self, ctx: &dyn ToolContext, params: DeleteParams) -> Result<Value> {
        let latest = self.latest(ctx, &params.surface_id);
        let Some(previous) = latest.as_ref().filter(|envelope| !envelope.deleted) else {
            return Err(invalid(
                Op::Delete.name(),
                format!("no surface '{}' is shown in this session", params.surface_id),
            ));
        };
        let version = self.next_version(ctx, &params.surface_id, latest.as_ref());
        let envelope = DrawnEnvelope {
            surface_id: params.surface_id.clone(),
            version,
            kind: previous.kind,
            catalog_id: previous.catalog_id.clone(),
            title: None,
            messages: vec![A2uiMessage::delete_surface(params.surface_id)],
            actions: Vec::new(),
            deleted: true,
        };
        Ok(self.shown(ctx, envelope))
    }
}

#[async_trait]
impl Tool for DrawTool {
    fn name(&self) -> &str {
        self.op.name()
    }

    fn description(&self) -> &str {
        DrawTool::description(self)
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(match self.op {
            Op::Form => schema_of::<FormParams>(),
            Op::Card => schema_of::<CardParams>(),
            Op::Table => schema_of::<TableParams>(),
            Op::Screen => schema_of::<ScreenParams>(),
            Op::Patch => schema_of::<PatchParams>(),
            Op::Delete => schema_of::<DeleteParams>(),
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let tool = self.op.name();
        let ctx = ctx.as_ref();
        match self.op {
            Op::Form => self.draw(ctx, form(params(tool, args)?)?),
            Op::Card => self.draw(ctx, card(params(tool, args)?)),
            Op::Table => self.draw(ctx, table(params(tool, args)?)),
            Op::Screen => {
                let screen: ScreenParams = params(tool, args)?;
                self.draw(
                    ctx,
                    SurfaceDraft {
                        surface_id: screen.surface_id,
                        kind: DrawnKind::Screen,
                        title: screen.title,
                        components: screen.components,
                        data_model: screen.data_model.unwrap_or_else(|| json!({})),
                        actions: screen.actions,
                    },
                )
            }
            Op::Patch => self.patch(ctx, params(tool, args)?),
            Op::Delete => self.delete(ctx, params(tool, args)?),
        }
    }
}
