//! The rule 5 validator (spec 3.3 §7.4 rule 5): agent-drawn UI never contains approval
//! controls, never asks for a secret, and shows only what the home's catalog renders.
//!
//! A drawing is checked whole, every time it changes: its structure and every component
//! against the catalog, then every string a person would read, resolved on the server from
//! literals, data model bindings and `formatString` and `pluralize` results, against the
//! reserved words of a decision after [`fold_display_text`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use adk_harness::approvals::matcher::fold_display_text;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::a2ui::DataModel;
use super::catalog::Catalog;
use super::drawn::{DrawnKind, RefusalKind};

/// Phrases of a decision and of the controls every surface owns, refused anywhere in a
/// drawing after folding (spec 3.3 §8.1, §8.2; wave 4 decision 20).
pub const RESERVED_PHRASES: &[&str] = &[
    "Allow once",
    "Always allow",
    "Waiting for you",
    "You said yes",
    "You said no",
    "Confirm it's you",
    "Stop all",
    "Resume all",
    "Take over",
    "Add this agent",
];

/// Words of a decision refused when they are a whole label or heading, so "Edit address"
/// is shown and "Edit" is not (wave 4 decision 20), and "Don't add sugar" is shown and the
/// suggested agent's "Don't add" is not (spec 3.3 §8.2).
pub const RESERVED_LABELS: &[&str] =
    &["Edit", "Deny", "Done", "Approve", "Approved", "Decided", "Don't add"];

/// The most components a drawing has.
pub const MAX_COMPONENTS: usize = 256;
/// The deepest a component tree nests.
pub const MAX_DEPTH: usize = 16;
/// The most components a drawing shows once its templates are expanded.
pub const MAX_SHOWN: usize = 2_000;
/// The most items a template renders from one list.
pub const MAX_LIST_ITEMS: usize = 500;
/// The longest string a drawing shows, in characters.
pub const MAX_TEXT_CHARS: usize = 4_000;
/// The most bytes a drawing's components, data model and actions take as JSON.
pub const MAX_DRAWING_BYTES: usize = 128 * 1024;
/// The most actions a drawing declares.
pub const MAX_ACTIONS: usize = 8;

/// The functions a drawing may call to check a field.
const CHECK_FUNCTIONS: &[&str] =
    &["required", "regex", "length", "numeric", "email", "and", "or", "not"];
/// The functions whose text the server resolves for display.
const TEXT_FUNCTIONS: &[&str] = &["formatString", "pluralize"];
/// The components a person types or picks a value in.
const INPUTS: &[&str] = &["TextField", "CheckBox", "ChoicePicker", "Slider", "DateTimeInput"];

/// Words that name a secret, matched as whole words of a field's label, id or path.
const SECRET_WORDS: &[&str] = &[
    "password",
    "passwords",
    "passcode",
    "passphrase",
    "pin",
    "secret",
    "token",
    "apikey",
    "otp",
    "totp",
    "2fa",
    "mfa",
    "cvv",
    "cvc",
    "ssn",
    "mnemonic",
];
/// Phrases that name a secret.
const SECRET_PHRASES: &[&str] = &[
    "api key",
    "secret key",
    "private key",
    "access key",
    "card number",
    "credit card",
    "security code",
    "one time code",
    "one time password",
    "verification code",
    "recovery code",
    "seed phrase",
    "social security",
];

/// An argument of a [`DrawnAction`]: the value a person typed in a field, or a constant the
/// drawing fixes.
///
/// An object with only `path` is a field binding; anything else is a constant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ActionArg {
    /// The value at a field's data model path.
    Field(FieldArg),
    /// A constant.
    Constant(Value),
}

/// The value at a field's data model path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldArg {
    /// The field's absolute data model path, such as `/to`.
    pub path: String,
}

/// An action a drawing declares: a button `label` that asks to run `tool` with `args`
/// (spec 3.3 §7.4 rule 5). Pressing it never runs the tool: it sends a requested call,
/// which waits for the person's decision.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{ActionArg, DrawnAction, FieldArg};
/// use serde_json::json;
///
/// let send: DrawnAction = serde_json::from_value(json!({
///     "id": "send",
///     "label": "Send",
///     "tool": "send_invoice",
///     "args": { "to": { "path": "/to" }, "invoice": "INV-10" }
/// }))
/// .unwrap();
/// assert_eq!(send.args["to"], ActionArg::Field(FieldArg { path: "/to".into() }));
/// assert_eq!(send.args["invoice"], ActionArg::Constant(json!("INV-10")));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DrawnAction {
    /// The action's id, which a button's event names.
    pub id: String,
    /// The button's text.
    pub label: String,
    /// The tool the person asks to run: one of the drawing agent's own tools.
    pub tool: String,
    /// The tool's arguments.
    #[serde(default)]
    pub args: BTreeMap<String, ActionArg>,
}

/// A drawing before it is checked: its components, data model and actions (spec 3.3 §6.1
/// `genui`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceDraft {
    /// The surface's id.
    pub surface_id: String,
    /// What it is.
    pub kind: DrawnKind,
    /// The title above it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Its A2UI components, one with id `root`.
    pub components: Vec<Value>,
    /// Its data model.
    #[serde(default)]
    pub data_model: Value,
    /// Its actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<DrawnAction>,
}

/// A string a drawing shows, resolved on the server (spec 3.3 §7.4 rule 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedText {
    /// The component, or `None` for the drawing's title and actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    /// The list item a template rendered it for, as a data model path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The property, such as `text`, `label` or `options/0/label`.
    pub field: String,
    /// What a person reads.
    pub text: String,
}

/// A drawing that passed every check, with every string it shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedSurface {
    /// The drawing.
    pub draft: SurfaceDraft,
    /// Every string it shows, in tree order.
    pub texts: Vec<ResolvedText>,
}

/// The component a refusal is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComponentRef {
    /// Its id.
    pub id: String,
    /// Its type, such as `Button`.
    pub component: String,
}

/// A refused drawing: what it looked like to a person, and the technical reason the
/// developer panel and the terminal show (spec 3.3 §7.4 rule 5).
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{ComponentRef, DrawRefusal, RefusalKind};
///
/// let refusal = DrawRefusal {
///     kind: RefusalKind::LooksLikeDecision,
///     catalog: "forms 1.0".into(),
///     component: Some(ComponentRef { id: "c7".into(), component: "Button".into() }),
///     detail: "label matches reserved word \"Allow once\" after normalisation".into(),
/// };
/// assert_eq!(
///     refusal.technical(),
///     "catalog forms 1.0 · component c7 Button · label matches reserved word \"Allow once\" \
///      after normalisation"
/// );
/// assert_eq!(refusal.code(), "genui.refused.looks_like_decision");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[serde(rename_all = "camelCase")]
#[error("{}", self.message())]
pub struct DrawRefusal {
    /// What it looked like.
    pub kind: RefusalKind,
    /// The catalog it was checked against, such as `forms 1.0`.
    pub catalog: String,
    /// The component at fault, if one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentRef>,
    /// What is wrong, in technical words.
    pub detail: String,
}

impl DrawRefusal {
    /// The technical reason: catalog, component and detail.
    pub fn technical(&self) -> String {
        match &self.component {
            Some(component) => format!(
                "catalog {} · component {} {} · {}",
                self.catalog, component.id, component.component, self.detail
            ),
            None => format!("catalog {} · {}", self.catalog, self.detail),
        }
    }

    /// The error code of the drawing tool's refusal.
    pub fn code(&self) -> &'static str {
        match self.kind {
            RefusalKind::LooksLikeDecision => "genui.refused.looks_like_decision",
            RefusalKind::AsksForSecret => "genui.refused.asks_for_secret",
            RefusalKind::CannotShow => "genui.refused.cannot_show",
        }
    }

    /// What the drawing agent is told: why it was not shown and what to do instead, then
    /// the technical reason.
    pub fn message(&self) -> String {
        let plain = match self.kind {
            RefusalKind::LooksLikeDecision => {
                "Not shown: it looks like a decision. A drawing never shows approval controls or \
                 a decision's words; the person decides on the home's own card. Draw it without \
                 them"
            }
            RefusalKind::AsksForSecret => {
                "Not shown: a drawn form never asks for a password or another secret. Ask the \
                 person to use Take over for passwords"
            }
            RefusalKind::CannotShow => {
                "Not shown: this home can't show it. Use only the catalog's components, no \
                 links, and the declared actions"
            }
        };
        format!("{plain}. ({})", self.technical())
    }
}

/// Checks drawings against a catalog and rule 5 (spec 3.3 §7.4 rule 5).
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{DrawnKind, RefusalKind, SurfaceDraft, Validator};
/// use serde_json::json;
///
/// let validator = Validator::forms();
/// let card = |text: &str| SurfaceDraft {
///     surface_id: "status".into(),
///     kind: DrawnKind::Card,
///     title: None,
///     components: vec![json!({ "id": "root", "component": "Text", "text": text })],
///     data_model: json!({}),
///     actions: Vec::new(),
/// };
/// assert!(validator.validate(&card("Invoice sent to ACME")).is_ok());
/// let refused = validator.validate(&card("Ａpproved")).unwrap_err();
/// assert_eq!(refused.kind, RefusalKind::LooksLikeDecision);
/// ```
#[derive(Debug, Clone)]
pub struct Validator {
    catalog: &'static Catalog,
    bindable: Option<BTreeSet<String>>,
}

impl Validator {
    /// A validator for `catalog`.
    pub fn new(catalog: &'static Catalog) -> Self {
        Self { catalog, bindable: None }
    }

    /// A validator for the `forms` 1.0 catalog.
    pub fn forms() -> Self {
        Self::new(Catalog::forms())
    }

    /// Accepts actions bound to `tools` only: the drawing agent's own tools. Without it, an
    /// action may name any tool but the kernel's and the drawing tools, and the agent
    /// refuses a tool it does not have when the person sends. Given twice, an action may
    /// name only a tool both give. The render tools also take the drawing agent's tools from
    /// its runtime when it gives them ([`ToolContext::agent_tool_names`]).
    ///
    /// [`ToolContext::agent_tool_names`]: adk_core::ToolContext::agent_tool_names
    #[must_use]
    pub fn with_bindable_tools(
        mut self,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let tools: BTreeSet<String> = tools.into_iter().map(Into::into).collect();
        self.bindable = Some(match self.bindable.take() {
            Some(earlier) => earlier.intersection(&tools).cloned().collect(),
            None => tools,
        });
        self
    }

    /// The catalog drawings are checked against.
    pub fn catalog(&self) -> &'static Catalog {
        self.catalog
    }

    /// Checks `draft` whole.
    ///
    /// # Errors
    ///
    /// Returns the first [`DrawRefusal`] found.
    pub fn validate(&self, draft: &SurfaceDraft) -> Result<ValidatedSurface, DrawRefusal> {
        let check =
            Check { catalog: self.catalog, model: DataModel::new(draft.data_model.clone()) };
        check.limits(draft)?;
        let components = check.components(draft)?;
        check.links(draft)?;
        let mut texts = Vec::new();
        if let Some(title) = &draft.title {
            texts.push(check.display(None, None, "title", title)?);
        }
        let mut walk = Walk { check: &check, components: &components, shown: 0, texts };
        walk.tree()?;
        let mut texts = walk.texts;
        check.actions(draft, &components, self.bindable.as_ref(), &mut texts)?;
        Ok(ValidatedSurface { draft: draft.clone(), texts })
    }
}

/// One component of a drawing: its type and properties.
struct Component<'a> {
    name: &'a str,
    props: &'a Map<String, Value>,
}

/// The checks of one drawing.
struct Check {
    catalog: &'static Catalog,
    model: DataModel,
}

impl Check {
    fn refuse(
        &self,
        kind: RefusalKind,
        component: Option<(&str, &str)>,
        detail: String,
    ) -> DrawRefusal {
        DrawRefusal {
            kind,
            catalog: self.catalog.label(),
            component: component
                .map(|(id, name)| ComponentRef { id: id.to_string(), component: name.to_string() }),
            detail,
        }
    }

    fn cannot_show(
        &self,
        component: Option<(&str, &str)>,
        detail: impl Into<String>,
    ) -> DrawRefusal {
        self.refuse(RefusalKind::CannotShow, component, detail.into())
    }

    /// Sizes and identifiers.
    fn limits(&self, draft: &SurfaceDraft) -> Result<(), DrawRefusal> {
        if !is_identifier(&draft.surface_id) {
            return Err(self.cannot_show(
                None,
                format!(
                    "surface id '{}' is not 1 to 64 letters, digits, '.', '_' or '-'",
                    draft.surface_id
                ),
            ));
        }
        if draft.components.is_empty() || draft.components.len() > MAX_COMPONENTS {
            return Err(self.cannot_show(
                None,
                format!(
                    "a drawing has 1 to {MAX_COMPONENTS} components, not {}",
                    draft.components.len()
                ),
            ));
        }
        if draft.actions.len() > MAX_ACTIONS {
            return Err(
                self.cannot_show(None, format!("a drawing declares at most {MAX_ACTIONS} actions"))
            );
        }
        let bytes = serde_json::to_vec(&(&draft.components, &draft.data_model, &draft.actions))
            .map_or(usize::MAX, |bytes| bytes.len());
        if bytes > MAX_DRAWING_BYTES {
            return Err(self.cannot_show(
                None,
                format!("the drawing takes {bytes} bytes; at most {MAX_DRAWING_BYTES}"),
            ));
        }
        Ok(())
    }

    /// Every component: an object with a unique id, a type the catalog renders, no secret
    /// field, only allowed functions and declared actions, and its catalog schema.
    fn components<'a>(
        &self,
        draft: &'a SurfaceDraft,
    ) -> Result<BTreeMap<&'a str, Component<'a>>, DrawRefusal> {
        let mut components = BTreeMap::new();
        for value in &draft.components {
            let Some(props) = value.as_object() else {
                return Err(self.cannot_show(None, "a component is not a JSON object"));
            };
            let (Some(id), Some(name)) = (
                props.get("id").and_then(Value::as_str),
                props.get("component").and_then(Value::as_str),
            ) else {
                return Err(
                    self.cannot_show(None, "a component has no string id or component type")
                );
            };
            let at = Some((id, name));
            if components.insert(id, Component { name, props }).is_some() {
                return Err(self.cannot_show(at, format!("id '{id}' is used twice")));
            }
            if !self.catalog.has_component(name) {
                return Err(self.cannot_show(at, format!("{name} is not in the catalog")));
            }
            self.secrets(id, name, props)?;
            if name == "Button" {
                self.button(id, props, draft)?;
            }
            self.functions(id, name, value)?;
            if let Some(error) = self.catalog.schema_error(name, value) {
                return Err(self.cannot_show(at, format!("does not match the catalog: {error}")));
            }
        }
        if !components.contains_key("root") {
            return Err(self.cannot_show(None, "no component has the id 'root'"));
        }
        Ok(components)
    }

    /// A field that asks for a secret: an obscured text field, or one whose label, id or
    /// bound path names a secret.
    fn secrets(&self, id: &str, name: &str, props: &Map<String, Value>) -> Result<(), DrawRefusal> {
        if name != "TextField" {
            return Ok(());
        }
        let refuse =
            |detail: String| Err(self.refuse(RefusalKind::AsksForSecret, Some((id, name)), detail));
        if props.get("variant").and_then(Value::as_str) == Some("obscured") {
            return refuse("an obscured field asks for a secret".to_string());
        }
        let mut names = vec![id.to_string()];
        if let Some(label) = props.get("label").and_then(Value::as_str) {
            names.push(label.to_string());
        }
        if let Some(path) =
            props.get("value").and_then(|value| value.get("path")).and_then(Value::as_str)
        {
            names.push(path.to_string());
        }
        for name in names {
            if let Some(secret) = names_secret(&name) {
                return refuse(format!("'{name}' names a secret ({secret})"));
            }
        }
        Ok(())
    }

    /// Function calls: the catalog's checks, and `formatString` and `pluralize` for text.
    fn functions(&self, id: &str, name: &str, value: &Value) -> Result<(), DrawRefusal> {
        let mut found = Vec::new();
        calls(value, &mut found);
        for call in found {
            if !CHECK_FUNCTIONS.contains(&call.as_str()) && !TEXT_FUNCTIONS.contains(&call.as_str())
            {
                return Err(self.cannot_show(
                    Some((id, name)),
                    format!("calls the function '{call}', which a drawing may not use"),
                ));
            }
        }
        // The server checks a sent value against the pattern, so one it cannot read would
        // refuse every send; the page leaves the check to the server.
        let mut patterns = Vec::new();
        regex_patterns(value, &mut patterns);
        for pattern in patterns {
            if regex::Regex::new(&format!("^(?:{pattern})$")).is_err() {
                return Err(self.cannot_show(
                    Some((id, name)),
                    format!(
                        "its regex check's pattern '{}' can't be read here: use one without \
                         look-around or back-references",
                        truncate(pattern)
                    ),
                ));
            }
        }
        Ok(())
    }

    /// A button dispatches one of the drawing's declared actions and nothing else.
    fn button(
        &self,
        id: &str,
        props: &Map<String, Value>,
        draft: &SurfaceDraft,
    ) -> Result<(), DrawRefusal> {
        let at = Some((id, "Button"));
        if matches!(draft.kind, DrawnKind::Card | DrawnKind::Table) {
            return Err(self.cannot_show(at, "a card or table has no buttons"));
        }
        let event = props.get("action").and_then(|action| action.get("event"));
        let Some(event) = event else {
            return Err(self.cannot_show(at, "a button runs only a declared action"));
        };
        let name = event.get("name").and_then(Value::as_str).unwrap_or_default();
        if !draft.actions.iter().any(|action| action.id == name) {
            return Err(
                self.cannot_show(at, format!("the button's action '{name}' is not declared"))
            );
        }
        if event
            .get("context")
            .is_some_and(|context| context.as_object().is_none_or(|map| !map.is_empty()))
        {
            return Err(self.cannot_show(
                at,
                "a button carries no event context; the declared action binds the fields",
            ));
        }
        Ok(())
    }

    /// No string anywhere in the drawing is a link.
    fn links(&self, draft: &SurfaceDraft) -> Result<(), DrawRefusal> {
        let mut strings = Vec::new();
        for component in &draft.components {
            strings_of(component, &mut strings);
        }
        strings_of(&draft.data_model, &mut strings);
        let actions = serde_json::to_value(&draft.actions).unwrap_or_default();
        strings_of(&actions, &mut strings);
        if let Some(title) = &draft.title {
            strings.push(title.as_str());
        }
        match strings.into_iter().find(|text| looks_like_link(text)) {
            Some(link) => Err(self.cannot_show(None, format!("'{}' is a link", truncate(link)))),
            None => Ok(()),
        }
    }

    /// A string a person reads: no longer than [`MAX_TEXT_CHARS`] and free of reserved words.
    fn display(
        &self,
        component: Option<(&str, &str)>,
        item: Option<&str>,
        field: &str,
        text: &str,
    ) -> Result<ResolvedText, DrawRefusal> {
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(self.cannot_show(
                component,
                format!("{field} is longer than {MAX_TEXT_CHARS} characters"),
            ));
        }
        if let Some(reserved) = reserved(text) {
            return Err(self.refuse(
                RefusalKind::LooksLikeDecision,
                component,
                format!("{field} matches reserved word \"{reserved}\" after normalisation"),
            ));
        }
        Ok(ResolvedText {
            component_id: component.map(|(id, _)| id.to_string()),
            item: item.map(str::to_string),
            field: field.to_string(),
            text: text.to_string(),
        })
    }

    /// Declared actions: unique ids, a label free of reserved words, a tool the agent may be
    /// asked to run, and arguments bound to fields the drawing has.
    fn actions(
        &self,
        draft: &SurfaceDraft,
        components: &BTreeMap<&str, Component<'_>>,
        bindable: Option<&BTreeSet<String>>,
        texts: &mut Vec<ResolvedText>,
    ) -> Result<(), DrawRefusal> {
        if matches!(draft.kind, DrawnKind::Card | DrawnKind::Table) {
            if !draft.actions.is_empty() {
                return Err(self.cannot_show(None, "a card or table declares no actions"));
            }
            if let Some((id, component)) =
                components.iter().find(|(_, component)| INPUTS.contains(&component.name))
            {
                return Err(
                    self.cannot_show(Some((id, component.name)), "a card or table has no fields")
                );
            }
        }
        let fields: BTreeSet<&str> = components
            .values()
            .filter(|component| INPUTS.contains(&component.name))
            .filter_map(|component| component.props.get("value")?.get("path")?.as_str())
            .filter(|path| path.starts_with('/'))
            .collect();
        let mut ids = BTreeSet::new();
        for action in &draft.actions {
            let detail = |what: String| format!("action '{}' {what}", action.id);
            if !is_identifier(&action.id) || !ids.insert(action.id.as_str()) {
                return Err(
                    self.cannot_show(None, detail("has an id that is not unique or valid".into()))
                );
            }
            texts.push(self.display(
                None,
                None,
                &format!("actions/{}/label", action.id),
                &action.label,
            )?);
            if let Some(why) = adk_core::RequestedCall::refused_tool(&action.tool) {
                return Err(
                    self.cannot_show(None, detail(format!("runs '{}': {why}", action.tool)))
                );
            }
            if bindable.is_some_and(|tools| !tools.contains(&action.tool)) {
                return Err(self.cannot_show(
                    None,
                    detail(format!(
                        "runs '{}', which is not one of the agent's tools",
                        action.tool
                    )),
                ));
            }
            for (name, arg) in &action.args {
                if let ActionArg::Field(field) = arg
                    && !fields.contains(field.path.as_str())
                {
                    return Err(self.cannot_show(
                        None,
                        detail(format!("binds '{name}' to {}, which no field fills", field.path)),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The values a choice picker or a slider shows as text: a chosen value that matches
    /// none of the picker's options, or a slider's bound value.
    fn shown_values(&self, scope: &str, name: &str, props: &Map<String, Value>) -> Vec<String> {
        let value = match props.get("value") {
            Some(Value::Object(binding)) => binding
                .get("path")
                .and_then(Value::as_str)
                .and_then(|path| self.lookup(scope, path)),
            other => other,
        };
        match (name, value) {
            ("Slider", Some(value)) => vec![shown(Some(value))],
            ("ChoicePicker", Some(value)) => {
                let options: Vec<&str> = props
                    .get("options")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|option| option.get("value")?.as_str())
                    .collect();
                let chosen = match value {
                    Value::Array(items) => items.iter().collect(),
                    one => vec![one],
                };
                chosen
                    .into_iter()
                    .filter(|chosen| !chosen.as_str().is_some_and(|value| options.contains(&value)))
                    .map(|chosen| shown(Some(chosen)))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// The value at `path`, absolute or relative to the list item `scope`.
    fn lookup(&self, scope: &str, path: &str) -> Option<&Value> {
        if path.starts_with('/') {
            self.model.get(path)
        } else {
            self.model.get(&format!("{scope}/{path}"))
        }
    }

    /// Every string `value` may show, the one shown first: a literal, a binding, or a
    /// `formatString` or `pluralize` result.
    fn texts(
        &self,
        at: (&str, &str),
        scope: &str,
        value: &Value,
        depth: usize,
    ) -> Result<Vec<String>, DrawRefusal> {
        if depth > 8 {
            return Err(self.cannot_show(Some(at), "text functions nest too deeply"));
        }
        match value {
            Value::String(text) => Ok(vec![text.clone()]),
            Value::Object(map) if map.contains_key("path") => {
                let path = map.get("path").and_then(Value::as_str).unwrap_or_default();
                Ok(vec![shown(self.lookup(scope, path))])
            }
            Value::Object(map) if map.contains_key("call") => {
                let call = map.get("call").and_then(Value::as_str).unwrap_or_default();
                let args = map.get("args").and_then(Value::as_object);
                let arg = |name: &str| args.and_then(|args| args.get(name));
                match call {
                    "formatString" => {
                        let mut out = Vec::new();
                        for template in
                            self.texts(at, scope, arg("value").unwrap_or(&Value::Null), depth + 1)?
                        {
                            out.push(self.interpolate(at, scope, &template)?);
                        }
                        Ok(out)
                    }
                    "pluralize" => {
                        let count = match arg("value") {
                            Some(Value::Number(number)) => number.as_f64(),
                            Some(Value::Object(binding)) if binding.contains_key("path") => binding
                                .get("path")
                                .and_then(Value::as_str)
                                .and_then(|path| self.lookup(scope, path))
                                .and_then(Value::as_f64),
                            _ => None,
                        };
                        let category = match count {
                            Some(count) if count == 0.0 && arg("zero").is_some() => "zero",
                            Some(count) if count == 1.0 && arg("one").is_some() => "one",
                            _ => "other",
                        };
                        let mut chosen = Vec::new();
                        let mut others = Vec::new();
                        for name in ["zero", "one", "two", "few", "many", "other"] {
                            if let Some(variant) = arg(name) {
                                let texts = self.texts(at, scope, variant, depth + 1)?;
                                if name == category {
                                    chosen.extend(texts)
                                } else {
                                    others.extend(texts)
                                }
                            }
                        }
                        chosen.extend(others);
                        Ok(chosen)
                    }
                    other => Err(self.cannot_show(
                        Some(at),
                        format!("only formatString and pluralize make text; '{other}' does not"),
                    )),
                }
            }
            Value::Null => Ok(vec![String::new()]),
            other => Ok(vec![shown(Some(other))]),
        }
    }

    /// `template` with every `${path}` replaced by the value at the path; `\${` is a
    /// literal `${`.
    fn interpolate(
        &self,
        at: (&str, &str),
        scope: &str,
        template: &str,
    ) -> Result<String, DrawRefusal> {
        let mut out = String::new();
        let mut rest = template;
        while let Some(start) = rest.find("${") {
            if rest[..start].ends_with('\\') {
                out.push_str(&rest[..start - 1]);
                out.push_str("${");
                rest = &rest[start + 2..];
                continue;
            }
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                return Err(self.cannot_show(Some(at), "formatString has an unclosed '${'"));
            };
            let expression = after[..end].trim();
            if expression.contains("${") || expression.contains('(') {
                return Err(self.cannot_show(
                    Some(at),
                    "formatString interpolates data paths only, never function calls",
                ));
            }
            out.push_str(&shown(self.lookup(scope, expression)));
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// The walk over the component tree from `root`, expanding templates against the data model.
struct Walk<'a> {
    check: &'a Check,
    components: &'a BTreeMap<&'a str, Component<'a>>,
    shown: usize,
    texts: Vec<ResolvedText>,
}

impl<'a> Walk<'a> {
    fn tree(&mut self) -> Result<(), DrawRefusal> {
        let mut visited = BTreeSet::new();
        self.visit("root", "", 0, &mut Vec::new(), &mut visited)?;
        // A component no one shows yet is checked too, so a later change that shows it
        // cannot bring a reserved word in unchecked.
        let unshown: Vec<&str> =
            self.components.keys().copied().filter(|id| !visited.contains(*id)).collect();
        for id in unshown {
            self.visit(id, "", 0, &mut Vec::new(), &mut visited)?;
        }
        Ok(())
    }

    fn visit(
        &mut self,
        id: &'a str,
        scope: &str,
        depth: usize,
        path: &mut Vec<&'a str>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), DrawRefusal> {
        let check = self.check;
        let components = self.components;
        let Some(component) = components.get(id) else {
            let parent =
                path.last().and_then(|parent| components.get(parent).map(|c| (*parent, c.name)));
            return Err(check.cannot_show(parent, format!("child '{id}' does not exist")));
        };
        let at = (id, component.name);
        if depth > MAX_DEPTH {
            return Err(check.cannot_show(Some(at), format!("the tree is deeper than {MAX_DEPTH}")));
        }
        if path.contains(&id) {
            return Err(check.cannot_show(Some(at), "the component tree loops"));
        }
        self.shown += 1;
        if self.shown > MAX_SHOWN {
            return Err(check.cannot_show(
                Some(at),
                format!("the drawing shows more than {MAX_SHOWN} components"),
            ));
        }
        visited.insert(id.to_string());
        let item = (!scope.is_empty()).then_some(scope);
        for (field, value) in display_fields(component.name, component.props) {
            for text in check.texts(at, scope, value, 0)? {
                let resolved = check.display(Some(at), item, &field, &text)?;
                self.texts.push(resolved);
            }
        }
        // A choice that matches no option, or a slider's value, is shown as it is.
        for text in check.shown_values(scope, component.name, component.props) {
            check.display(Some(at), item, "value", &text)?;
        }
        path.push(id);
        for child in children(component.name, component.props) {
            match child {
                Child::Id(child) => self.visit(child, scope, depth + 1, path, visited)?,
                Child::Template { component: child, list } => {
                    let list = if list.starts_with('/') {
                        list.to_string()
                    } else {
                        format!("{scope}/{list}")
                    };
                    let items =
                        check.model.get(&list).and_then(Value::as_array).map_or(0, Vec::len);
                    if items > MAX_LIST_ITEMS {
                        return Err(check.cannot_show(
                            Some(at),
                            format!("a list renders at most {MAX_LIST_ITEMS} items, not {items}"),
                        ));
                    }
                    for index in 0..items {
                        self.visit(child, &format!("{list}/{index}"), depth + 1, path, visited)?;
                    }
                }
            }
        }
        path.pop();
        Ok(())
    }
}

/// A component's child: by id, or a template rendered once per item of a data model list.
pub(super) enum Child<'a> {
    Id(&'a str),
    Template { component: &'a str, list: &'a str },
}

pub(super) fn children<'a>(name: &str, props: &'a Map<String, Value>) -> Vec<Child<'a>> {
    let mut out = Vec::new();
    match name {
        "Row" | "Column" | "List" => match props.get("children") {
            Some(Value::Array(ids)) => {
                out.extend(ids.iter().filter_map(Value::as_str).map(Child::Id))
            }
            Some(Value::Object(template)) => {
                if let (Some(component), Some(list)) = (
                    template.get("componentId").and_then(Value::as_str),
                    template.get("path").and_then(Value::as_str),
                ) {
                    out.push(Child::Template { component, list });
                }
            }
            _ => {}
        },
        "Card" | "Button" => out.extend(props.get("child").and_then(Value::as_str).map(Child::Id)),
        "Tabs" => out.extend(
            props
                .get("tabs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|tab| tab.get("child")?.as_str())
                .map(Child::Id),
        ),
        _ => {}
    }
    out
}

/// The properties of `name` a person reads, by field.
fn display_fields<'a>(name: &str, props: &'a Map<String, Value>) -> Vec<(String, &'a Value)> {
    let mut fields = Vec::new();
    let mut take = |field: &str| {
        if let Some(value) = props.get(field) {
            fields.push((field.to_string(), value));
        }
    };
    match name {
        "Text" => take("text"),
        "TextField" | "DateTimeInput" => {
            take("label");
            take("value");
        }
        "CheckBox" | "Slider" | "ChoicePicker" => take("label"),
        _ => {}
    }
    if name == "ChoicePicker" {
        for (index, option) in
            props.get("options").and_then(Value::as_array).into_iter().flatten().enumerate()
        {
            if let Some(label) = option.get("label") {
                fields.push((format!("options/{index}/label"), label));
            }
        }
    }
    if name == "Tabs" {
        for (index, tab) in
            props.get("tabs").and_then(Value::as_array).into_iter().flatten().enumerate()
        {
            if let Some(title) = tab.get("title") {
                fields.push((format!("tabs/{index}/title"), title));
            }
        }
    }
    if let Some(accessibility) = props.get("accessibility").and_then(Value::as_object) {
        for field in ["label", "description"] {
            if let Some(value) = accessibility.get(field) {
                fields.push((format!("accessibility/{field}"), value));
            }
        }
    }
    for (index, check) in
        props.get("checks").and_then(Value::as_array).into_iter().flatten().enumerate()
    {
        if let Some(message) = check.get("message") {
            fields.push((format!("checks/{index}/message"), message));
        }
    }
    fields
}

/// Every function a component calls, by name.
fn calls(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(call) = map.get("call").and_then(Value::as_str) {
                found.push(call.to_string());
            }
            map.values().for_each(|value| calls(value, found));
        }
        Value::Array(items) => items.iter().for_each(|value| calls(value, found)),
        _ => {}
    }
}

/// The pattern of every `regex` check in `value`.
fn regex_patterns<'a>(value: &'a Value, found: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            if map.get("call").and_then(Value::as_str) == Some("regex")
                && let Some(pattern) =
                    map.get("args").and_then(|args| args.get("pattern")).and_then(Value::as_str)
            {
                found.push(pattern);
            }
            map.values().for_each(|value| regex_patterns(value, found));
        }
        Value::Array(items) => items.iter().for_each(|value| regex_patterns(value, found)),
        _ => {}
    }
}

/// Every string in `value`, keys included.
fn strings_of<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => out.push(text),
        Value::Object(map) => {
            for (key, value) in map {
                out.push(key);
                strings_of(value, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|value| strings_of(value, out)),
        _ => {}
    }
}

/// The text a data model value shows.
fn shown(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(other) => other.to_string(),
    }
}

/// Whether `id` is 1 to 64 ASCII letters, digits, `.`, `_` or `-`.
fn is_identifier(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// `text` cut for a refusal's detail.
fn truncate(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(80).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

/// The words of folded text, apostrophes inside a word kept.
fn words(folded: &str) -> Vec<&str> {
    folded
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|word| word.trim_matches('\''))
        .filter(|word| !word.is_empty())
        .collect()
}

/// The reserved phrases as folded words, each with its wording.
type FoldedPhrases = Vec<(Vec<String>, &'static str)>;

/// The reserved labels as folded words, each with its wording.
type FoldedLabels = Vec<(Vec<String>, &'static str)>;

/// The reserved wording, folded once.
static RESERVED: LazyLock<(FoldedPhrases, FoldedLabels)> = LazyLock::new(|| {
    let fold = |text: &str| {
        words(&fold_display_text(text)).into_iter().map(str::to_string).collect::<Vec<_>>()
    };
    let phrases = RESERVED_PHRASES.iter().map(|phrase| (fold(phrase), *phrase)).collect();
    let labels = RESERVED_LABELS.iter().map(|label| (fold(label), *label)).collect();
    (phrases, labels)
});

/// The reserved wording `text` shows, if any: a phrase anywhere, or a label as the whole
/// text.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::reserved;
///
/// assert_eq!(reserved("Tap Аllow  once to send"), Some("Allow once"));
/// assert_eq!(reserved("Approved ✓"), Some("Approved"));
/// assert_eq!(reserved("Edit address"), None);
/// ```
pub fn reserved(text: &str) -> Option<&'static str> {
    let folded = fold_display_text(text);
    let words = words(&folded);
    let (phrases, labels) = &*RESERVED;
    for (phrase, original) in phrases {
        if words
            .windows(phrase.len())
            .any(|window| window.iter().zip(phrase).all(|(word, part)| *word == part.as_str()))
        {
            return Some(original);
        }
    }
    labels
        .iter()
        .find(|(label, _)| {
            label.len() == words.len() && label.iter().zip(&words).all(|(part, word)| part == word)
        })
        .map(|(_, original)| *original)
}

/// The secret a field's label, id or path names, if any.
fn names_secret(text: &str) -> Option<&'static str> {
    let folded = fold_display_text(&text.replace(['_', '-', '/', '.'], " "));
    let words = words(&folded);
    if let Some(word) = SECRET_WORDS.iter().find(|secret| words.contains(secret)) {
        return Some(word);
    }
    let joined = words.join(" ");
    SECRET_PHRASES
        .iter()
        .find(|phrase| {
            joined == **phrase
                || joined.starts_with(&format!("{phrase} "))
                || joined.ends_with(&format!(" {phrase}"))
                || joined.contains(&format!(" {phrase} "))
        })
        .copied()
}

/// Whether `text` holds a link: `://`, `www.`, or a scheme such as `javascript:` or
/// `mailto:` followed by more than a space.
fn looks_like_link(text: &str) -> bool {
    let folded = fold_display_text(text);
    if folded.contains("://") || folded.contains("www.") || folded.contains(":\\\\") {
        return true;
    }
    const SCHEMES: &[&str] = &[
        "javascript",
        "vbscript",
        "data",
        "mailto",
        "tel",
        "sms",
        "file",
        "blob",
        "intent",
        "ftp",
        "http",
        "https",
    ];
    SCHEMES.iter().any(|scheme| {
        folded.match_indices(&format!("{scheme}:")).any(|(at, matched)| {
            let before = folded[..at].chars().next_back();
            let after = folded[at + matched.len()..].chars().next();
            before.is_none_or(|c| !c.is_alphanumeric()) && after.is_some_and(|c| !c.is_whitespace())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_wording_is_found_whatever_its_spelling() {
        let cases = [
            ("Allow once", Some("Allow once")),
            ("ALLOW ONCE", Some("Allow once")),
            ("Ａllow ｏnce", Some("Allow once")),
            ("A\u{200B}llow\u{2060} once", Some("Allow once")),
            ("\u{202E}Allow once\u{202C}", Some("Allow once")),
            ("Аllоw оnce", Some("Allow once")),
            ("A11ow 0nce", Some("Allow once")),
            ("Allów once", Some("Allow once")),
            ("Always allow for this person", Some("Always allow")),
            ("Confirm it’s you and approve", Some("Confirm it's you")),
            ("Stop all", Some("Stop all")),
            ("Take over", Some("Take over")),
            ("You said yes in Telegram", Some("You said yes")),
            ("Edit", Some("Edit")),
            ("  deny! ", Some("Deny")),
            ("✓ Approved", Some("Approved")),
            ("Edit address", None),
            ("Approve the October invoice?", None),
            ("Done at 10:04", None),
            ("Invoice INV-10", None),
            ("Allowance once a month", None),
        ];
        for (text, expected) in cases {
            assert_eq!(reserved(text), expected, "{text:?}");
        }
    }

    #[test]
    fn links_are_found_in_their_disguises() {
        for link in [
            "https://example.com",
            "see www.example.com",
            "javascript:alert(1)",
            "mailto:ana@example.com",
            "ＨＴＴＰＳ：／／example.com",
            "data:text/html,hi",
        ] {
            assert!(looks_like_link(link), "{link}");
        }
        for text in ["Data: 5 items", "Tel: +254 700", "ana@example.com", "10:03", "Total: 4200"] {
            assert!(!looks_like_link(text), "{text}");
        }
    }

    #[test]
    fn secrets_are_named_by_words_not_by_letters() {
        for name in ["Password", "/user/pin", "api_key", "Card number", "one-time code", "PIN"] {
            assert!(names_secret(name).is_some(), "{name}");
        }
        for name in ["Shipping address", "Spinach", "To", "/amount", "Tokenizer settings"] {
            assert_eq!(names_secret(name), None, "{name}");
        }
    }
}
