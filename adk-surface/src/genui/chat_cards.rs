//! Agent-drawn UI in chat apps (spec 3.3 §6.1 `genui`): a projected drawing as Slack Block Kit
//! or a Microsoft Teams Adaptive Card, and a press of one of its buttons back as the same
//! [`Command::SubmitDrawn`](crate::Command::SubmitDrawn) the home sends, which the engine
//! checks against the stored drawing and holds as a requested call for the person's decision.
//!
//! What a chat card draws is what the terminal draws: text and headings, the home's own
//! summary of a chart or scene with a line saying the home shows it, fields, and the drawing's
//! buttons, each with the engine's line under it ("Sending runs send_invoice and asks you
//! first"). Nothing is markdown: Slack text is `plain_text`, and Adaptive Card text has its
//! markdown characters escaped, so a drawing can neither link nor style itself. Buttons carry
//! no style, so they never look like a decision's.
//!
//! The adapters do not post drawings yet; a host that does calls these functions.

use serde_json::{Map, Value, json};

use super::drawn::{DrawnField, DrawnInput, DrawnNode, DrawnSurface};
use crate::engine::Command;

/// The `action_id` prefix of a drawn button in Slack, and the key of its press in a Teams
/// `Action.Submit`'s data.
pub const DRAWN_ACTION_KEY: &str = "adk.drawn";

/// Slack's limit on blocks in one message.
const SLACK_BLOCKS: usize = 50;
/// Slack's limit on a header's text, and on a section's.
const SLACK_HEADER_CHARS: usize = 150;
const SLACK_TEXT_CHARS: usize = 3_000;

/// The line a chat card shows where the home draws more: a chart or a 3D scene.
const IN_THE_HOME: &str = "Open the home to see it drawn.";
/// The line a chat card ends with when the drawing has more than a message holds.
const MORE_IN_THE_HOME: &str = "Open the home to see the rest.";

fn cut(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn plain(text: &str, limit: usize) -> Value {
    json!({ "type": "plain_text", "text": cut(text, limit), "emoji": false })
}

/// What a button's press carries back: the surface, the version shown and the action.
fn press(surface: &DrawnSurface, action: &str) -> Value {
    json!({ "surfaceId": surface.surface_id, "version": surface.version, "actionId": action })
}

/// The label a field shows: its node's text, else its path.
fn label_of(node: &DrawnNode, field: &DrawnField) -> String {
    node.text.clone().unwrap_or_else(|| field.path.clone())
}

/// `surface` as a Slack message: `{ text, blocks }`, `text` the notification's fallback.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{DrawnNode, DrawnSurface, slack_blocks};
///
/// let mut surface: DrawnSurface = serde_json::from_value(serde_json::json!({
///     "surfaceId": "s", "version": 1, "kind": "card", "catalog": "forms 1.0",
///     "agent": { "name": "finance", "displayName": "Finance", "avatar": { "monogram": "F", "color": "#2563eb" } },
///     "drawnAt": "2026-11-19T10:02:00Z", "atText": "10:02", "drawnText": "Drawn by Finance · 10:02",
///     "changedText": "", "earlierText": "", "title": "Invoice INV-10"
/// }))?;
/// surface.root = Some(DrawnNode { key: "t".into(), component: "Text".into(), text: Some("Due Friday".into()), ..DrawnNode::default() });
/// let message = slack_blocks(&surface);
/// assert_eq!(message["blocks"][0]["type"], "header");
/// assert_eq!(message["blocks"][2]["text"]["type"], "plain_text");
/// # Ok::<(), serde_json::Error>(())
/// ```
pub fn slack_blocks(surface: &DrawnSurface) -> Value {
    let mut blocks = Vec::new();
    let title = surface.title.clone().unwrap_or_else(|| surface.drawn_text.clone());
    blocks.push(json!({ "type": "header", "text": plain(&title, SLACK_HEADER_CHARS) }));
    blocks.push(
        json!({ "type": "context", "elements": [plain(&surface.drawn_text, SLACK_TEXT_CHARS)] }),
    );
    if let Some(root) = &surface.root {
        slack_node(surface, root, &mut blocks);
    }
    if blocks.len() > SLACK_BLOCKS {
        blocks.truncate(SLACK_BLOCKS - 1);
        blocks.push(
            json!({ "type": "context", "elements": [plain(MORE_IN_THE_HOME, SLACK_TEXT_CHARS)] }),
        );
    }
    json!({ "text": cut(&title, SLACK_TEXT_CHARS), "blocks": blocks })
}

fn slack_node(surface: &DrawnSurface, node: &DrawnNode, blocks: &mut Vec<Value>) {
    let section = |text: &str| json!({ "type": "section", "text": plain(text, SLACK_TEXT_CHARS) });
    let context =
        |text: &str| json!({ "type": "context", "elements": [plain(text, SLACK_TEXT_CHARS)] });
    if let Some(chart) = &node.chart {
        blocks.push(section(&chart.summary));
        blocks.push(context(IN_THE_HOME));
        return;
    }
    if let Some(scene) = &node.scene {
        blocks.push(section(&scene.summary));
        blocks.push(context(IN_THE_HOME));
        return;
    }
    if let Some(field) = &node.field {
        blocks.push(slack_input(node, field));
        return;
    }
    match node.component.as_str() {
        "Text" => {
            let text = node.text.as_deref().unwrap_or_default();
            match node.variant.as_deref() {
                Some("h1" | "h2" | "h3") => {
                    blocks
                        .push(json!({ "type": "header", "text": plain(text, SLACK_HEADER_CHARS) }));
                }
                Some("caption") => blocks.push(context(text)),
                _ if !text.is_empty() => blocks.push(section(text)),
                _ => {}
            }
        }
        "Divider" => blocks.push(json!({ "type": "divider" })),
        "Button" => {
            let Some(action) = &node.action else { return };
            let label = node.text.as_deref().unwrap_or(action);
            blocks.push(json!({
                "type": "actions",
                "elements": [{
                    "type": "button",
                    "text": plain(label, 75),
                    "action_id": format!("{DRAWN_ACTION_KEY}:{action}"),
                    "value": press(surface, action).to_string(),
                }],
            }));
            if let Some(view) = surface.actions.iter().find(|view| &view.id == action) {
                blocks.push(context(&view.note));
            }
        }
        "Tabs" => {
            for (index, child) in node.children.iter().enumerate() {
                if let Some(title) = node.tabs.get(index) {
                    blocks.push(section(title));
                }
                slack_node(surface, child, blocks);
            }
        }
        _ => {
            for child in &node.children {
                slack_node(surface, child, blocks);
            }
            if let Some(more) = &node.show_all_text {
                blocks.push(context(more));
            }
        }
    }
}

fn slack_input(node: &DrawnNode, field: &DrawnField) -> Value {
    let options = || {
        field
            .options
            .iter()
            .map(|option| json!({ "text": plain(&option.label, 75), "value": option.value }))
            .collect::<Vec<_>>()
    };
    let text = field.value.as_str().filter(|text| !text.is_empty());
    let element = match field.input {
        DrawnInput::ShortText | DrawnInput::LongText => {
            let mut element = json!({
                "type": "plain_text_input",
                "multiline": field.input == DrawnInput::LongText,
            });
            if let Some(text) = text {
                element["initial_value"] = json!(text);
            }
            element
        }
        DrawnInput::Number | DrawnInput::Slider => {
            let mut element = json!({ "type": "number_input", "is_decimal_allowed": true });
            if let Some(min) = &field.min {
                element["min_value"] = json!(min.to_string());
            }
            if let Some(max) = &field.max {
                element["max_value"] = json!(max.to_string());
            }
            if field.value.is_number() {
                element["initial_value"] = json!(field.value.to_string());
            }
            element
        }
        DrawnInput::Date => {
            let mut element = json!({ "type": "datepicker" });
            if let Some(text) = text {
                element["initial_date"] = json!(text);
            }
            element
        }
        DrawnInput::Time => {
            let mut element = json!({ "type": "timepicker" });
            if let Some(text) = text {
                element["initial_time"] = json!(text);
            }
            element
        }
        DrawnInput::DateTime => json!({ "type": "datetimepicker" }),
        DrawnInput::Checkbox => {
            let only = json!({ "text": plain(&label_of(node, field), 75), "value": "yes" });
            let mut element = json!({ "type": "checkboxes", "options": [only.clone()] });
            if field.value == Value::Bool(true) {
                element["initial_options"] = json!([only]);
            }
            element
        }
        DrawnInput::Choice => json!({ "type": "static_select", "options": options() }),
        DrawnInput::Choices => json!({ "type": "multi_static_select", "options": options() }),
    };
    let mut element = element;
    element["action_id"] = json!("value");
    json!({
        "type": "input",
        "block_id": field.path,
        "optional": true,
        "label": plain(&label_of(node, field), 2_000),
        "element": element,
    })
}

/// The fields a drawing shows, by data path.
fn fields_of(node: &DrawnNode, out: &mut Vec<(String, DrawnField)>) {
    if let Some(field) = &node.field {
        out.push((field.path.clone(), field.clone()));
    }
    for child in &node.children {
        fields_of(child, out);
    }
}

/// The surface, version and action a chat press names, if it names a drawn button.
fn pressed(data: &Value) -> Option<(String, u64, String)> {
    Some((
        data.get("surfaceId")?.as_str()?.to_string(),
        data.get("version")?.as_u64()?,
        data.get("actionId")?.as_str()?.to_string(),
    ))
}

/// The surface and version a Slack `block_actions` payload's press names, so the host can
/// find the drawing it was pressed on; `None` for a press that is not a drawn button.
pub fn slack_pressed(payload: &Value) -> Option<(String, u64)> {
    let action = payload.get("actions")?.as_array()?.first()?;
    action.get("action_id")?.as_str()?.strip_prefix(DRAWN_ACTION_KEY)?;
    let data: Value = serde_json::from_str(action.get("value")?.as_str()?).ok()?;
    pressed(&data).map(|(surface, version, _)| (surface, version))
}

/// A Slack `block_actions` press of a drawn button on `surface` as the command the home
/// sends: the surface, the version shown, the action and the fields' values from the
/// message's `state`. `None` when the press is not of a button of this version of `surface`.
pub fn submit_from_slack(
    session_id: &str,
    surface: &DrawnSurface,
    payload: &Value,
) -> Option<Command> {
    let action = payload.get("actions")?.as_array()?.first()?;
    action.get("action_id")?.as_str()?.strip_prefix(DRAWN_ACTION_KEY)?;
    let data: Value = serde_json::from_str(action.get("value")?.as_str()?).ok()?;
    let (surface_id, version, action_id) = pressed(&data)?;
    if surface_id != surface.surface_id || version != surface.version {
        return None;
    }
    let state = payload.pointer("/state/values").and_then(Value::as_object);
    let mut fields = Vec::new();
    if let Some(root) = &surface.root {
        fields_of(root, &mut fields);
    }
    let mut values = Map::new();
    for (path, field) in fields {
        let Some(element) =
            state.and_then(|state| state.get(&path)).and_then(|block| block.get("value"))
        else {
            continue;
        };
        let value = match field.input {
            DrawnInput::Checkbox => json!(
                element
                    .get("selected_options")
                    .and_then(Value::as_array)
                    .is_some_and(|picked| !picked.is_empty())
            ),
            DrawnInput::Choice => match element.pointer("/selected_option/value") {
                Some(value) => value.clone(),
                None => continue,
            },
            DrawnInput::Choices => json!(
                element
                    .get("selected_options")
                    .and_then(Value::as_array)
                    .map(|picked| picked
                        .iter()
                        .filter_map(|o| o.get("value").cloned())
                        .collect::<Vec<_>>())
                    .unwrap_or_default()
            ),
            DrawnInput::Date => element.get("selected_date").cloned().unwrap_or(Value::Null),
            DrawnInput::Time => element.get("selected_time").cloned().unwrap_or(Value::Null),
            DrawnInput::DateTime => element
                .get("selected_date_time")
                .and_then(Value::as_i64)
                .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                .map_or(Value::Null, |at| json!(at.to_rfc3339())),
            _ => element.get("value").cloned().unwrap_or(Value::Null),
        };
        if !value.is_null() {
            values.insert(path, value);
        }
    }
    Some(Command::SubmitDrawn {
        session_id: session_id.to_string(),
        surface_id,
        version,
        action_id,
        values,
    })
}

/// Adaptive Card text with its markdown characters escaped, so a drawing neither links nor
/// styles itself.
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '\\' | '*' | '_' | '[' | ']' | '(' | ')' | '#' | '>' | '`' | '~') {
            out.push('\\');
        }
        out.push(character);
    }
    out
}

fn text_block(text: &str) -> Value {
    json!({ "type": "TextBlock", "text": escaped(text), "wrap": true })
}

/// `surface` as a Microsoft Teams Adaptive Card (schema 1.5): its body, and an
/// `Action.Submit` per drawn button whose data names the surface, version and action.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{DrawnNode, DrawnSurface, adaptive_card};
///
/// let mut surface: DrawnSurface = serde_json::from_value(serde_json::json!({
///     "surfaceId": "s", "version": 1, "kind": "card", "catalog": "forms 1.0",
///     "agent": { "name": "finance", "displayName": "Finance", "avatar": { "monogram": "F", "color": "#2563eb" } },
///     "drawnAt": "2026-11-19T10:02:00Z", "atText": "10:02", "drawnText": "Drawn by Finance · 10:02",
///     "changedText": "", "earlierText": "", "title": "Q3 [draft]"
/// }))?;
/// surface.root = Some(DrawnNode { key: "t".into(), component: "Text".into(), text: Some("**bold?**".into()), ..DrawnNode::default() });
/// let card = adaptive_card(&surface);
/// assert_eq!(card["type"], "AdaptiveCard");
/// assert_eq!(card["body"][0]["text"], "Q3 \\[draft\\]");
/// assert_eq!(card["body"][2]["text"], "\\*\\*bold?\\*\\*");
/// # Ok::<(), serde_json::Error>(())
/// ```
pub fn adaptive_card(surface: &DrawnSurface) -> Value {
    let title = surface.title.clone().unwrap_or_else(|| surface.drawn_text.clone());
    let mut body = vec![
        json!({ "type": "TextBlock", "text": escaped(&title), "wrap": true, "size": "Large", "weight": "Bolder" }),
        json!({ "type": "TextBlock", "text": escaped(&surface.drawn_text), "wrap": true, "isSubtle": true, "size": "Small" }),
    ];
    let mut actions = Vec::new();
    if let Some(root) = &surface.root {
        card_node(surface, root, &mut body, &mut actions);
    }
    json!({
        "type": "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "version": "1.5",
        "body": body,
        "actions": actions,
    })
}

fn card_node(
    surface: &DrawnSurface,
    node: &DrawnNode,
    body: &mut Vec<Value>,
    actions: &mut Vec<Value>,
) {
    let subtle = |text: &str| json!({ "type": "TextBlock", "text": escaped(text), "wrap": true, "isSubtle": true, "size": "Small" });
    if let Some(chart) = &node.chart {
        body.push(text_block(&chart.summary));
        body.push(subtle(IN_THE_HOME));
        return;
    }
    if let Some(scene) = &node.scene {
        body.push(text_block(&scene.summary));
        body.push(subtle(IN_THE_HOME));
        return;
    }
    if let Some(field) = &node.field {
        body.push(card_input(node, field));
        return;
    }
    match node.component.as_str() {
        "Text" => {
            let text = node.text.as_deref().unwrap_or_default();
            match node.variant.as_deref() {
                Some("h1" | "h2" | "h3") => body.push(json!({
                    "type": "TextBlock", "text": escaped(text), "wrap": true, "size": "Medium", "weight": "Bolder"
                })),
                Some("caption") => body.push(subtle(text)),
                _ if !text.is_empty() => body.push(text_block(text)),
                _ => {}
            }
        }
        "Divider" => body.push(json!({ "type": "Container", "separator": true, "items": [] })),
        "Button" => {
            let Some(action) = &node.action else { return };
            actions.push(json!({
                "type": "Action.Submit",
                "title": node.text.as_deref().unwrap_or(action),
                "data": { DRAWN_ACTION_KEY: press(surface, action) },
            }));
            if let Some(view) = surface.actions.iter().find(|view| &view.id == action) {
                body.push(subtle(&view.note));
            }
        }
        "Row" => {
            let mut columns = Vec::new();
            for child in &node.children {
                let mut items = Vec::new();
                card_node(surface, child, &mut items, actions);
                columns.push(json!({ "type": "Column", "width": "stretch", "items": items }));
            }
            body.push(json!({ "type": "ColumnSet", "columns": columns }));
        }
        "Tabs" => {
            for (index, child) in node.children.iter().enumerate() {
                if let Some(title) = node.tabs.get(index) {
                    body.push(json!({ "type": "TextBlock", "text": escaped(title), "wrap": true, "weight": "Bolder" }));
                }
                card_node(surface, child, body, actions);
            }
        }
        _ => {
            for child in &node.children {
                card_node(surface, child, body, actions);
            }
            if let Some(more) = &node.show_all_text {
                body.push(subtle(more));
            }
        }
    }
}

fn card_input(node: &DrawnNode, field: &DrawnField) -> Value {
    let label = escaped(&label_of(node, field));
    let choices = || {
        field
            .options
            .iter()
            .map(|option| json!({ "title": option.label, "value": option.value }))
            .collect::<Vec<_>>()
    };
    let mut input = match field.input {
        DrawnInput::ShortText | DrawnInput::LongText => json!({
            "type": "Input.Text", "isMultiline": field.input == DrawnInput::LongText
        }),
        DrawnInput::Number | DrawnInput::Slider => {
            let mut input = json!({ "type": "Input.Number" });
            if let Some(min) = &field.min {
                input["min"] = min.clone();
            }
            if let Some(max) = &field.max {
                input["max"] = max.clone();
            }
            input
        }
        DrawnInput::Date => json!({ "type": "Input.Date" }),
        DrawnInput::Time => json!({ "type": "Input.Time" }),
        DrawnInput::DateTime => json!({ "type": "Input.Text", "placeholder": "2026-11-15T10:30" }),
        DrawnInput::Checkbox => json!({
            "type": "Input.Toggle", "title": label, "valueOn": "true", "valueOff": "false"
        }),
        DrawnInput::Choice => {
            json!({ "type": "Input.ChoiceSet", "choices": choices(), "style": "compact" })
        }
        DrawnInput::Choices => {
            json!({ "type": "Input.ChoiceSet", "choices": choices(), "isMultiSelect": true })
        }
    };
    input["id"] = json!(field.path);
    input["label"] = json!(label);
    let initial = match (&field.input, &field.value) {
        (DrawnInput::Checkbox, Value::Bool(on)) => Some(json!(on.to_string())),
        (_, Value::String(text)) if !text.is_empty() => Some(json!(text)),
        (_, Value::Number(number)) => Some(Value::Number(number.clone())),
        _ => None,
    };
    if let Some(initial) = initial {
        input["value"] = initial;
    }
    input
}

/// The surface and version a Teams `Action.Submit` value's press names; `None` for a submit
/// that is not a drawn button.
pub fn teams_pressed(value: &Value) -> Option<(String, u64)> {
    pressed(value.get(DRAWN_ACTION_KEY)?).map(|(surface, version, _)| (surface, version))
}

/// A Teams `Action.Submit` of a drawn button on `surface` (the activity's `value`: the
/// button's data merged with the inputs, keyed by data path) as the command the home sends.
/// `None` when it is not a button of this version of `surface`.
pub fn submit_from_teams(
    session_id: &str,
    surface: &DrawnSurface,
    value: &Value,
) -> Option<Command> {
    let (surface_id, version, action_id) = pressed(value.get(DRAWN_ACTION_KEY)?)?;
    if surface_id != surface.surface_id || version != surface.version {
        return None;
    }
    let mut fields = Vec::new();
    if let Some(root) = &surface.root {
        fields_of(root, &mut fields);
    }
    let mut values = Map::new();
    for (path, field) in fields {
        let Some(given) = value.get(&path) else { continue };
        let typed = match (field.input, given) {
            (DrawnInput::Checkbox, Value::String(text)) => json!(text == "true"),
            (DrawnInput::Choices, Value::String(text)) => {
                json!(text.split(',').filter(|part| !part.is_empty()).collect::<Vec<_>>())
            }
            (_, given) => given.clone(),
        };
        values.insert(path, typed);
    }
    Some(Command::SubmitDrawn {
        session_id: session_id.to_string(),
        surface_id,
        version,
        action_id,
        values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genui::{DrawnActionView, DrawnOption};

    fn field(component: &str, label: &str, path: &str, input: DrawnInput) -> DrawnNode {
        DrawnNode {
            key: path.trim_start_matches('/').to_string(),
            component: component.into(),
            text: Some(label.into()),
            field: Some(DrawnField {
                path: path.into(),
                input,
                value: Value::Null,
                options: if input == DrawnInput::Choices {
                    vec![
                        DrawnOption { label: "PDF".into(), value: "pdf".into() },
                        DrawnOption { label: "CSV".into(), value: "csv".into() },
                    ]
                } else {
                    Vec::new()
                },
                min: None,
                max: None,
                checks: Vec::new(),
            }),
            ..DrawnNode::default()
        }
    }

    fn invoice() -> DrawnSurface {
        let mut surface: DrawnSurface = serde_json::from_value(json!({
            "surfaceId": "invoice-10", "version": 3, "kind": "form", "catalog": "forms 1.0",
            "agent": { "name": "finance", "displayName": "Finance", "avatar": { "monogram": "F", "color": "#2563eb" } },
            "drawnAt": "2026-11-19T10:02:00Z", "atText": "10:02", "drawnText": "Drawn by Finance · 10:02",
            "changedText": "", "earlierText": "", "title": "Invoice INV-10"
        }))
        .unwrap();
        surface.root = Some(DrawnNode {
            key: "root".into(),
            component: "Column".into(),
            children: vec![
                DrawnNode {
                    key: "t".into(),
                    component: "Text".into(),
                    text: Some("See https://x.test *now*".into()),
                    ..DrawnNode::default()
                },
                field("TextField", "To", "/to", DrawnInput::ShortText),
                field("CheckBox", "Copy me", "/copy", DrawnInput::Checkbox),
                field("ChoicePicker", "Formats", "/formats", DrawnInput::Choices),
                DrawnNode {
                    key: "send".into(),
                    component: "Button".into(),
                    text: Some("Send".into()),
                    action: Some("send".into()),
                    ..DrawnNode::default()
                },
            ],
            ..DrawnNode::default()
        });
        surface.actions = vec![DrawnActionView {
            id: "send".into(),
            label: "Send".into(),
            tool: "send_invoice".into(),
            note: "Sending runs send_invoice and asks you first".into(),
            args: Vec::new(),
        }];
        surface
    }

    #[test]
    fn slack_draws_plain_text_fields_and_unstyled_buttons_and_reads_a_press_back() {
        let surface = invoice();
        let message = slack_blocks(&surface);
        let blocks = message["blocks"].as_array().unwrap();
        assert!(
            blocks
                .iter()
                .all(|block| block.pointer("/text/type").is_none_or(|t| t == "plain_text"))
        );
        let button = blocks.iter().find(|b| b["type"] == "actions").unwrap();
        assert!(button["elements"][0].get("style").is_none(), "never a decision's look");
        assert!(blocks.iter().any(|b| b.pointer("/elements/0/text")
            == Some(&json!("Sending runs send_invoice and asks you first"))));

        let payload = json!({
            "type": "block_actions",
            "actions": [{ "action_id": "adk.drawn:send", "value": button["elements"][0]["value"] }],
            "state": { "values": {
                "/to": { "value": { "type": "plain_text_input", "value": "ana@example.com" } },
                "/copy": { "value": { "type": "checkboxes", "selected_options": [{ "value": "yes" }] } },
                "/formats": { "value": { "type": "multi_static_select", "selected_options": [{ "value": "pdf" }] } }
            } }
        });
        assert_eq!(slack_pressed(&payload), Some(("invoice-10".to_string(), 3)));
        let Some(Command::SubmitDrawn { surface_id, version, action_id, values, .. }) =
            submit_from_slack("thread-1", &surface, &payload)
        else {
            panic!("a send")
        };
        assert_eq!((surface_id.as_str(), version, action_id.as_str()), ("invoice-10", 3, "send"));
        assert_eq!(values["/to"], "ana@example.com");
        assert_eq!(values["/copy"], true);
        assert_eq!(values["/formats"], json!(["pdf"]));

        let mut newer = surface.clone();
        newer.version = 4;
        assert!(
            submit_from_slack("thread-1", &newer, &payload).is_none(),
            "a press of an older version"
        );
    }

    #[test]
    fn teams_escapes_markdown_and_reads_a_submit_back() {
        let surface = invoice();
        let card = adaptive_card(&surface);
        assert_eq!(card["body"][2]["text"], "See https://x.test \\*now\\*");
        let submit = &card["actions"][0];
        assert_eq!(submit["type"], "Action.Submit");
        let mut value = submit["data"].clone();
        value["/to"] = json!("ana@example.com");
        value["/copy"] = json!("false");
        value["/formats"] = json!("pdf,csv");
        assert_eq!(teams_pressed(&value), Some(("invoice-10".to_string(), 3)));
        let Some(Command::SubmitDrawn { values, .. }) =
            submit_from_teams("thread-1", &surface, &value)
        else {
            panic!("a send")
        };
        assert_eq!(values["/copy"], false);
        assert_eq!(values["/formats"], json!(["pdf", "csv"]));
        assert!(submit_from_teams("thread-1", &surface, &json!({ "/to": "x" })).is_none());
    }

    #[test]
    fn a_long_drawing_ends_with_where_to_see_the_rest() {
        let mut surface = invoice();
        surface.root = Some(DrawnNode {
            key: "root".into(),
            component: "Column".into(),
            children: (0..80)
                .map(|i| DrawnNode {
                    key: format!("t{i}"),
                    component: "Text".into(),
                    text: Some(format!("Line {i}")),
                    ..DrawnNode::default()
                })
                .collect(),
            ..DrawnNode::default()
        });
        let blocks = slack_blocks(&surface)["blocks"].as_array().unwrap().clone();
        assert_eq!(blocks.len(), SLACK_BLOCKS);
        assert_eq!(blocks.last().unwrap()["elements"][0]["text"], MORE_IN_THE_HOME);
    }
}
