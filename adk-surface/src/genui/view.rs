//! The projection of a render tool's result into a thread item (spec 3.3 §6.1 `genui`,
//! §7.4 rule 5): provenance by the reserved state key and the drawing's digest, the
//! drawing checked again, its strings resolved on the server.

use std::collections::HashMap;

use adk_core::{DRAW_TOOL_NAMES, Event};
use serde_json::{Map, Value};

use super::a2ui::DataModel;
use super::catalog::Catalog;
use super::draw::{DRAWN_STATE_KEY, DrawnEnvelope, DrawnRecord, REFUSED_STATE_KEY};
use super::drawn::{
    DrawnActionView, DrawnArg, DrawnCheck, DrawnField, DrawnInput, DrawnKind, DrawnNode,
    DrawnOption, DrawnSurface, RefusalKind, RefusedDrawing,
};
use super::validate::{ActionArg, Child, DrawRefusal, ValidatedSurface, Validator, children};
use crate::model::ThreadItem;
use crate::project::Projector;
use crate::words;

/// How many rows of a list a renderer shows before "Show all".
pub const SHOWN_ROWS: usize = 10;

/// The thread item `event`'s response `response` of `tool` makes when it is a drawing, or
/// `None` for an ordinary result: a tool that is not a render tool, or one whose event does
/// not carry the reserved state key with the drawing's digest (provenance).
pub(crate) fn project(
    projector: &Projector,
    event: &Event,
    tool: &str,
    response: &Value,
    call_id: Option<&String>,
) -> Option<ThreadItem> {
    // An app an MCP server's tool showed (spec 3.3 §6.1 `genui-apps`).
    #[cfg(feature = "genui-apps")]
    if let Some(item) = super::mcp_apps::project(projector, event, response, call_id) {
        return Some(item);
    }
    if !DRAW_TOOL_NAMES.contains(&tool) {
        return None;
    }
    if response.get("error").is_some() {
        let refusal = event.actions.state_delta.get(REFUSED_STATE_KEY)?;
        let refusal: DrawRefusal = serde_json::from_value(refusal.clone()).ok()?;
        return Some(refused(projector, event, tool, call_id, &refusal));
    }
    let Some(envelope) = proven(event, response) else {
        tracing::debug!(
            tool.name = %tool,
            "a render tool's result without its drawing's record is shown as a tool result"
        );
        return None;
    };
    Some(match drawn(projector, event, call_id, &envelope) {
        Ok(surface) => ThreadItem::Drawn { surface: Box::new(surface) },
        Err(refusal) => refused(projector, event, tool, call_id, &refusal),
    })
}

/// The drawing `response` records, when `event` proves a render tool made it: the event
/// writes `adk.surface.drawn` with the drawing's surface, version and digest.
pub(crate) fn proven(event: &Event, response: &Value) -> Option<DrawnEnvelope> {
    let record = event.actions.state_delta.get(DRAWN_STATE_KEY)?;
    let record: DrawnRecord = serde_json::from_value(record.clone()).ok()?;
    let envelope = DrawnEnvelope::from_response(response)?;
    (record.surface_id == envelope.surface_id
        && record.version == envelope.version
        && record.digest == envelope.digest())
    .then_some(envelope)
}

/// The drawing `envelope` records, checked again against its catalog.
fn drawn(
    projector: &Projector,
    event: &Event,
    call_id: Option<&String>,
    envelope: &DrawnEnvelope,
) -> Result<DrawnSurface, DrawRefusal> {
    let catalog = Catalog::forms();
    let agent = projector.agent(&event.author);
    let at = projector.hh_mm(event.timestamp);
    let changed_text = match (envelope.deleted, envelope.kind) {
        (true, _) => words::removed_this(&agent.display_name, &at),
        (false, DrawnKind::Form | DrawnKind::Screen) => {
            words::changed_this_form(&agent.display_name, &at)
        }
        (false, DrawnKind::Card | DrawnKind::Table) => {
            words::changed_this(&agent.display_name, &at)
        }
    };
    let mut surface = DrawnSurface {
        surface_id: envelope.surface_id.clone(),
        version: envelope.version,
        kind: envelope.kind,
        catalog: catalog.label(),
        drawn_text: words::drawn_by(&agent.display_name, &at),
        changed_text,
        earlier_text: words::earlier_version(&at),
        agent,
        drawn_at: event.timestamp,
        at_text: at.clone(),
        title: None,
        root: None,
        actions: Vec::new(),
        deleted: envelope.deleted,
        call_id: call_id.cloned(),
    };
    if envelope.deleted {
        return Ok(surface);
    }
    let unknown = || DrawRefusal {
        kind: RefusalKind::CannotShow,
        catalog: catalog.label(),
        component: None,
        detail: format!("this home does not render the catalog {}", envelope.catalog_id),
    };
    if envelope.catalog_id != catalog.id() {
        return Err(unknown());
    }
    let draft = envelope.draft().ok_or_else(|| DrawRefusal {
        kind: RefusalKind::CannotShow,
        catalog: catalog.label(),
        component: None,
        detail: "its messages do not draw one surface".to_string(),
    })?;
    let validated = Validator::new(catalog).validate(&draft)?;
    surface.title = validated.draft.title.clone();
    surface.actions = validated
        .draft
        .actions
        .iter()
        .map(|action| DrawnActionView {
            id: action.id.clone(),
            label: action.label.clone(),
            tool: action.tool.clone(),
            note: words::sending_runs(&action.tool),
            args: action
                .args
                .iter()
                .map(|(name, arg)| match arg {
                    ActionArg::Field(field) => {
                        DrawnArg { name: name.clone(), path: Some(field.path.clone()), value: None }
                    }
                    ActionArg::Constant(value) => {
                        DrawnArg { name: name.clone(), path: None, value: Some(value.clone()) }
                    }
                })
                .collect(),
        })
        .collect();
    surface.root = Some(tree(&validated));
    Ok(surface)
}

/// The notice of a refused drawing.
fn refused(
    projector: &Projector,
    event: &Event,
    tool: &str,
    call_id: Option<&String>,
    refusal: &DrawRefusal,
) -> ThreadItem {
    let agent = projector.agent(&event.author);
    let text = match refusal.kind {
        RefusalKind::LooksLikeDecision => words::looked_like_a_decision(&agent.display_name),
        RefusalKind::AsksForSecret => words::asked_for_a_password(&agent.display_name),
        RefusalKind::CannotShow => words::cant_show_here(&agent.display_name),
    };
    ThreadItem::DrawnRefused {
        refusal: RefusedDrawing {
            agent,
            kind: refusal.kind,
            text,
            reason: refusal.technical(),
            component: refusal.component.as_ref().map(|component| component.id.clone()),
            at: event.timestamp,
            at_text: event
                .timestamp
                .with_timezone(&projector.offset)
                .format("%H:%M:%S")
                .to_string(),
            tool: tool.to_string(),
            call_id: call_id.cloned(),
        },
    }
}

/// The strings a validated drawing shows, by component, list item and property: the first
/// of each, which is the one shown.
type Texts<'a> = HashMap<(&'a str, &'a str, &'a str), &'a str>;

/// The drawing's tree from `root`, templates expanded and strings resolved.
pub(crate) fn tree(validated: &ValidatedSurface) -> DrawnNode {
    let mut texts: Texts<'_> = HashMap::new();
    for text in &validated.texts {
        if let Some(id) = &text.component_id {
            texts
                .entry((id.as_str(), text.item.as_deref().unwrap_or(""), text.field.as_str()))
                .or_insert(text.text.as_str());
        }
    }
    let components: HashMap<&str, (&str, &Map<String, Value>)> = validated
        .draft
        .components
        .iter()
        .filter_map(|value| {
            let props = value.as_object()?;
            Some((props.get("id")?.as_str()?, (props.get("component")?.as_str()?, props)))
        })
        .collect();
    let builder = Builder {
        texts,
        components,
        model: DataModel::new(validated.draft.data_model.clone()),
        title: validated.draft.title.as_deref(),
    };
    builder.node("root", "", 0).unwrap_or_else(|| DrawnNode {
        key: "root".to_string(),
        component: "Column".to_string(),
        text: None,
        variant: None,
        field: None,
        action: None,
        tabs: Vec::new(),
        show_all_text: None,
        children: Vec::new(),
    })
}

struct Builder<'a> {
    texts: Texts<'a>,
    components: HashMap<&'a str, (&'a str, &'a Map<String, Value>)>,
    model: DataModel,
    title: Option<&'a str>,
}

impl Builder<'_> {
    fn text(&self, id: &str, scope: &str, field: &str) -> Option<String> {
        self.texts.get(&(id, scope, field)).map(|text| (*text).to_string())
    }

    /// `path` from a component in the list item `scope`, absolute.
    fn absolute(scope: &str, path: &str) -> String {
        if path.starts_with('/') { path.to_string() } else { format!("{scope}/{path}") }
    }

    fn node(&self, id: &str, scope: &str, depth: usize) -> Option<DrawnNode> {
        // The validator already refused deeper trees and loops.
        if depth > super::validate::MAX_DEPTH + 1 {
            return None;
        }
        let (component, props) = *self.components.get(id)?;
        let key = if scope.is_empty() { id.to_string() } else { format!("{id}@{scope}") };
        let string = |name: &str| props.get(name).and_then(Value::as_str).map(str::to_string);
        let mut node = DrawnNode {
            key,
            component: component.to_string(),
            text: None,
            variant: None,
            field: None,
            action: None,
            tabs: Vec::new(),
            show_all_text: None,
            children: Vec::new(),
        };
        match component {
            "Text" => {
                node.text = self.text(id, scope, "text");
                node.variant = string("variant");
            }
            "Row" | "Column" => node.variant = string("justify"),
            "List" => node.variant = string("direction"),
            "Divider" => node.variant = string("axis"),
            "Button" => {
                node.variant = string("variant");
                node.action = props
                    .get("action")
                    .and_then(|action| action.get("event"))
                    .and_then(|event| event.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                // A button labelled by a Text is drawn with that label.
                if let Some(child) = props.get("child").and_then(Value::as_str)
                    && let Some(("Text", _)) = self.components.get(child)
                {
                    node.text = self.text(child, scope, "text");
                    return Some(node);
                }
            }
            "Tabs" => {
                node.tabs = props
                    .get("tabs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(index, _)| {
                        self.text(id, scope, &format!("tabs/{index}/title")).unwrap_or_default()
                    })
                    .collect();
            }
            "TextField" | "DateTimeInput" | "CheckBox" | "ChoicePicker" | "Slider" => {
                node.text = self.text(id, scope, "label");
                node.variant = string("variant");
                node.field = self.field(id, component, props, scope);
            }
            _ => {}
        }
        for child in children(component, props) {
            match child {
                Child::Id(child) => {
                    // A titled drawing's own title is drawn once, above it.
                    if child == "title"
                        && scope.is_empty()
                        && self.title.is_some()
                        && self.text(child, "", "text").as_deref() == self.title
                    {
                        continue;
                    }
                    node.children.extend(self.node(child, scope, depth + 1));
                }
                Child::Template { component: child, list } => {
                    let list = Self::absolute(scope, list);
                    let items = self.model.get(&list).and_then(Value::as_array).map_or(0, Vec::len);
                    for index in 0..items {
                        node.children.extend(self.node(
                            child,
                            &format!("{list}/{index}"),
                            depth + 1,
                        ));
                    }
                    if items > SHOWN_ROWS {
                        node.show_all_text = Some(words::show_all(items));
                    }
                }
            }
        }
        Some(node)
    }

    fn field(
        &self,
        id: &str,
        component: &str,
        props: &Map<String, Value>,
        scope: &str,
    ) -> Option<DrawnField> {
        let path = props.get("value")?.get("path")?.as_str()?;
        let path = Self::absolute(scope, path);
        let flag = |name: &str| props.get(name).and_then(Value::as_bool).unwrap_or(false);
        let input = match component {
            "TextField" => match props.get("variant").and_then(Value::as_str) {
                Some("longText") => DrawnInput::LongText,
                Some("number") => DrawnInput::Number,
                _ => DrawnInput::ShortText,
            },
            "DateTimeInput" => match (flag("enableDate"), flag("enableTime")) {
                (true, true) => DrawnInput::DateTime,
                (false, true) => DrawnInput::Time,
                _ => DrawnInput::Date,
            },
            "CheckBox" => DrawnInput::Checkbox,
            "ChoicePicker" => match props.get("variant").and_then(Value::as_str) {
                Some("multipleSelection") => DrawnInput::Choices,
                _ => DrawnInput::Choice,
            },
            _ => DrawnInput::Slider,
        };
        let value = self.model.get(&path).cloned().unwrap_or_else(|| match input {
            DrawnInput::Checkbox => Value::Bool(false),
            DrawnInput::Choice | DrawnInput::Choices => Value::Array(Vec::new()),
            DrawnInput::Number | DrawnInput::Slider => Value::Null,
            _ => Value::String(String::new()),
        });
        let options = props
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, option)| {
                Some(DrawnOption {
                    label: self.text(id, scope, &format!("options/{index}/label"))?,
                    value: option.get("value")?.as_str()?.to_string(),
                })
            })
            .collect();
        let bound = |name: &str| {
            props.get(name).filter(|value| value.is_number() || value.is_string()).cloned()
        };
        let (min, max) = match input {
            DrawnInput::Slider => (Some(bound("min").unwrap_or(Value::from(0))), bound("max")),
            _ => (bound("min"), bound("max")),
        };
        let checks = props
            .get("checks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, check)| {
                Some(DrawnCheck {
                    condition: scoped(check.get("condition")?, scope),
                    message: self.text(id, scope, &format!("checks/{index}/message"))?,
                })
            })
            .collect();
        Some(DrawnField { path, input, value, options, min, max, checks })
    }
}

/// `condition` with every relative binding made absolute in the list item `scope`.
fn scoped(condition: &Value, scope: &str) -> Value {
    match condition {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| match (key.as_str(), value) {
                    ("path", Value::String(path)) if !path.starts_with('/') => {
                        (key.clone(), Value::String(Builder::absolute(scope, path)))
                    }
                    _ => (key.clone(), scoped(value, scope)),
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|item| scoped(item, scope)).collect()),
        other => other.clone(),
    }
}
