//! Agent-drawn UI in the terminal (spec 3.3 §5.2, §7.4 rule 5): the bordered box a drawing
//! is shown in, in the normal border colour and never the decision's; the plain notice of a
//! refused drawing with its technical reason on a muted second line; the inline block; and
//! the fill-in pane `ctrl-f` opens, where Tab moves between fields, Enter previews the call, a
//! second Enter sends it, Esc leaves, and the decision keys type into the field. Every string
//! passes [`terminal_safe`](super::terminal_safe).

use std::collections::{HashMap, HashSet};

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use serde_json::{Map, Value, json};
use unicode_width::UnicodeWidthStr;

use super::safe::safe_line;
use super::theme::Theme;
use super::view::fit;
use crate::engine::Command;
use crate::genui::{
    AppView, DrawnActionView, DrawnField, DrawnInput, DrawnNode, DrawnSurface, FormState,
    FormStatus, RefusedDrawing,
};
use crate::model::{FeedEntry, ThreadItem};
use crate::words;

/// The widest a drawn box is drawn.
const BOX_WIDTH: usize = 76;

/// How many rows of a long list the terminal shows before "show all".
const SHOWN_ROWS: usize = 10;

/// The footer of a drawn form that can be sent.
pub(crate) const FILL_IN_HINT: &str = "ctrl-f fill in · sending asks you first";

/// The fill-in pane's line under its title.
const FILL_IN_INTRO: &str = "one field per line · tab to the next";

/// The fill-in pane's keys while editing.
const FILL_IN_KEYS: &str =
    "tab next field · enter preview · esc leave · decision keys do nothing here";

/// The fill-in pane's keys on the preview.
const PREVIEW_KEYS: &str = "enter send · esc back · decision keys do nothing here";

/// What the terminal knows of the drawings of the thread it shows: the latest version of each
/// surface, and the state of each form sent.
#[derive(Debug, Clone, Default)]
pub(crate) struct Drawings {
    /// The latest version of each surface.
    pub latest: HashMap<String, u64>,
    /// The first version of each surface the thread shows.
    pub first: HashMap<String, u64>,
    /// Each sent form's state, by surface and version.
    pub forms: HashMap<(String, u64), FormState>,
    /// The render calls whose drawing, or refused drawing, the thread shows in their place.
    pub calls: HashSet<String>,
}

impl Drawings {
    /// What `entries` say, with each decision's answer from `decided`.
    pub(crate) fn of(
        entries: &[FeedEntry],
        decided: impl Fn(&str) -> Option<crate::model::DecidedAnswer>,
    ) -> Self {
        let mut latest: HashMap<String, u64> = HashMap::new();
        let mut first: HashMap<String, u64> = HashMap::new();
        let mut calls = HashSet::new();
        for item in entries.iter().flat_map(|entry| &entry.items) {
            match item {
                ThreadItem::Drawn { surface } => {
                    let version = latest.entry(surface.surface_id.clone()).or_default();
                    *version = (*version).max(surface.version);
                    let version =
                        first.entry(surface.surface_id.clone()).or_insert(surface.version);
                    *version = (*version).min(surface.version);
                    calls.extend(surface.call_id.clone());
                }
                ThreadItem::DrawnRefused { refusal } => calls.extend(refusal.call_id.clone()),
                // The app stands for the call that showed it, as a drawing does.
                ThreadItem::App { app } => {
                    calls.insert(app.call_id.clone());
                }
                ThreadItem::Message { .. }
                | ThreadItem::Activity { .. }
                | ThreadItem::Result { .. }
                | ThreadItem::Decision { .. }
                | ThreadItem::DrawnSent { .. } => {}
            }
        }
        let forms = crate::genui::form_states(entries, decided)
            .into_iter()
            .map(|state| ((state.surface_id.clone(), state.version), state))
            .collect();
        Self { latest, first, forms, calls }
    }

    /// Whether `surface` is the latest version of its surface.
    pub(crate) fn live(&self, surface: &DrawnSurface) -> bool {
        self.latest.get(&surface.surface_id).is_none_or(|latest| *latest == surface.version)
    }

    /// The state of `surface`, once sent.
    pub(crate) fn state(&self, surface: &DrawnSurface) -> Option<&FormState> {
        self.forms.get(&(surface.surface_id.clone(), surface.version))
    }
}

/// `text` with its first letter in lower case: the terminal's own lines are lower case.
fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// One row of a drawing as text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    /// A label and its value, aligned with the other pairs.
    Pair(String, String),
    /// A line of text; muted when it is a caption.
    Text(String, bool),
    /// Cells of a table row; the header's are muted.
    Cells(Vec<String>, bool),
    /// A divider.
    Rule,
}

/// What `field` holds, as the terminal shows it: `value` when given, else its own.
pub(crate) fn shown_value(field: &DrawnField, value: Option<&Value>) -> String {
    let value = value.unwrap_or(&field.value);
    match (field.input, value) {
        (DrawnInput::Checkbox, Value::Bool(true)) => "yes".to_string(),
        (DrawnInput::Checkbox, _) => "no".to_string(),
        (DrawnInput::Choice | DrawnInput::Choices, Value::Array(chosen)) => chosen
            .iter()
            .filter_map(Value::as_str)
            .map(|chosen| {
                field
                    .options
                    .iter()
                    .find(|option| option.value == chosen)
                    .map_or_else(|| chosen.to_string(), |option| option.label.clone())
            })
            .collect::<Vec<_>>()
            .join(", "),
        (_, Value::String(text)) => text.clone(),
        (_, Value::Null) => String::new(),
        (_, other) => other.to_string(),
    }
}

/// The rows of `node`, in order.
fn rows(node: &DrawnNode, out: &mut Vec<Row>) {
    if let Some(field) = &node.field {
        out.push(Row::Pair(node.text.clone().unwrap_or_default(), shown_value(field, None)));
        return;
    }
    match node.component.as_str() {
        "Text" => {
            let caption = node.variant.as_deref() == Some("caption");
            out.push(Row::Text(node.text.clone().unwrap_or_default(), caption));
        }
        "Divider" => out.push(Row::Rule),
        // A form's buttons are sent from the fill-in pane.
        "Button" => {}
        "Row"
            if !node.children.is_empty()
                && node.children.iter().all(|child| child.component == "Text") =>
        {
            let cells: Vec<String> =
                node.children.iter().map(|child| child.text.clone().unwrap_or_default()).collect();
            let header =
                node.children.iter().all(|child| child.variant.as_deref() == Some("caption"));
            match cells.as_slice() {
                [label, value] if !header => out.push(Row::Pair(label.clone(), value.clone())),
                _ => out.push(Row::Cells(cells, header)),
            }
        }
        "Tabs" => {
            for (title, child) in node.tabs.iter().zip(&node.children) {
                out.push(Row::Text(title.clone(), true));
                rows(child, out);
            }
        }
        _ => {
            let shown = if node.component == "List" { SHOWN_ROWS } else { usize::MAX };
            for child in node.children.iter().take(shown) {
                rows(child, out);
            }
            if node.children.len() > shown
                && let Some(more) = &node.show_all_text
            {
                out.push(Row::Text(lower_first(more), true));
            }
        }
    }
}

/// The lines of `rows`, `width` cells wide, pairs aligned and table columns sized.
fn row_lines(theme: &Theme, rows: &[Row], width: usize) -> Vec<Vec<Span<'static>>> {
    let label_width = rows
        .iter()
        .filter_map(|row| match row {
            Row::Pair(label, _) => Some(safe_line(label).width()),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .min(width / 3);
    let columns = rows.iter().fold(Vec::<usize>::new(), |mut widths, row| {
        if let Row::Cells(cells, _) = row {
            for (index, cell) in cells.iter().enumerate() {
                let cell = safe_line(cell).width();
                match widths.get_mut(index) {
                    Some(known) => *known = (*known).max(cell),
                    None => widths.push(cell),
                }
            }
        }
        widths
    });
    let column = (width / columns.len().max(1)).saturating_sub(2).max(4);
    rows.iter()
        .map(|row| match row {
            Row::Pair(label, value) => {
                let label = fit(&safe_line(label), label_width);
                let pad = label_width.saturating_sub(label.width()) + 2;
                let value = fit(&safe_line(value), width.saturating_sub(label_width + 2));
                vec![
                    Span::styled(label, theme.label()),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(value, theme.text()),
                ]
            }
            Row::Text(text, muted) => {
                let style = if *muted { theme.label() } else { theme.text() };
                vec![Span::styled(fit(&safe_line(text), width), style)]
            }
            Row::Cells(cells, header) => {
                let style = if *header { theme.label() } else { theme.text() };
                let mut spans = Vec::new();
                for (index, cell) in cells.iter().enumerate() {
                    let room = columns.get(index).copied().unwrap_or(column).min(column);
                    let cell = fit(&safe_line(cell), room);
                    let pad = room.saturating_sub(cell.width()) + 2;
                    spans.push(Span::styled(cell, style));
                    spans.push(Span::raw(" ".repeat(pad)));
                }
                spans
            }
            Row::Rule => vec![Span::styled("─".repeat(width), theme.border())],
        })
        .collect()
}

/// The line over a drawing: "drawn by Finance · forms · rev 3".
fn byline(surface: &DrawnSurface) -> String {
    let catalog = surface.catalog.split_whitespace().next().unwrap_or("forms");
    format!(
        "drawn by {} · {catalog} · rev {}",
        safe_line(&surface.agent.display_name),
        surface.version
    )
}

/// What a drawing's box says at its foot: its state once sent, else how to fill it in.
fn foot(surface: &DrawnSurface, state: Option<&FormState>, live: bool) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    if let Some(state) = state {
        out.push((lower_first(&state.text), state.status == FormStatus::Refused));
    }
    let sendable = !surface.actions.is_empty() && live;
    if sendable && state.is_none_or(|state| !state.locked) {
        out.push((FILL_IN_HINT.to_string(), false));
    }
    out
}

/// The bordered box of `surface` in the full-screen thread (spec 3.3 §5.2): the normal border
/// colour, its title left and "drawn by Finance · forms · rev 3" right, aligned label and
/// value pairs, and its state or "ctrl-f fill in · sending asks you first" at its foot. A
/// version a later one replaced is one muted line.
pub(crate) fn box_lines(
    theme: &Theme,
    surface: &DrawnSurface,
    drawings: &Drawings,
    width: usize,
) -> Vec<Line<'static>> {
    let live = drawings.live(surface);
    if !live {
        let earlier = format!("{} · rev {}", lower_first(&surface.earlier_text), surface.version);
        return vec![Line::from(Span::styled(fit(&earlier, width), theme.label()))];
    }
    let mut lines = Vec::new();
    // "changed 10:05 · rev 4" over a drawing whose earlier version the thread shows.
    if drawings.first.get(&surface.surface_id).is_some_and(|first| *first < surface.version) {
        let changed = if surface.deleted {
            lower_first(&safe_line(&surface.changed_text))
        } else {
            format!("changed {} · rev {}", surface.at_text, surface.version)
        };
        lines.push(Line::from(Span::styled(fit(&changed, width), theme.label())));
    }
    if surface.deleted {
        return lines;
    }
    let outer = width.clamp(24, BOX_WIDTH);
    let inner = outer - 4;
    let border = theme.border();
    let edge = |left: &str, right: &str| {
        Line::from(Span::styled(format!("{left}{}{right}", "─".repeat(outer - 2)), border))
    };
    let framed = |spans: Vec<Span<'static>>| {
        let used: usize = spans.iter().map(|span| span.content.width()).sum();
        let mut line = vec![Span::styled("│ ", border)];
        line.extend(spans);
        line.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        line.push(Span::styled(" │", border));
        Line::from(line)
    };
    lines.push(edge("┌", "┐"));
    let title = safe_line(surface.title.as_deref().unwrap_or_default());
    let by = byline(surface);
    let title = fit(&title, inner.saturating_sub(by.width() + 2));
    let gap = inner.saturating_sub(title.width() + by.width());
    lines.push(framed(vec![
        Span::styled(title, theme.text().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(gap)),
        Span::styled(fit(&by, inner), theme.label()),
    ]));
    let mut drawn = Vec::new();
    if let Some(root) = &surface.root {
        rows(root, &mut drawn);
    }
    for spans in row_lines(theme, &drawn, inner) {
        lines.push(framed(spans));
    }
    for (text, problem) in foot(surface, drawings.state(surface), live) {
        let style = if problem { theme.accent() } else { theme.label() };
        lines.push(framed(vec![Span::styled(fit(&safe_line(&text), inner), style)]));
    }
    lines.push(edge("└", "┘"));
    lines
}

/// The plain block of `surface` in the scrollback (`--ui inline`): a rule with its title and
/// who drew it, its fields, and "sending asks you first".
pub(crate) fn inline_lines(
    theme: &Theme,
    surface: &DrawnSurface,
    width: usize,
) -> Vec<Line<'static>> {
    if surface.deleted {
        return vec![Line::from(Span::styled(
            fit(&lower_first(&safe_line(&surface.changed_text)), width),
            theme.label(),
        ))];
    }
    let title = safe_line(surface.title.as_deref().unwrap_or_default());
    let head = format!("── {title} · drawn by {} ──", safe_line(&surface.agent.display_name));
    let mut lines = vec![Line::from(Span::styled(fit(&head, width), theme.label()))];
    let mut drawn = Vec::new();
    if let Some(root) = &surface.root {
        rows(root, &mut drawn);
    }
    lines.extend(row_lines(theme, &drawn, width).into_iter().map(Line::from));
    if !surface.actions.is_empty() {
        lines.push(Line::from(Span::styled(
            lower_first(words::SENDING_ASKS_YOU_FIRST),
            theme.label(),
        )));
    }
    lines
}

/// The notice of a refused drawing, and its technical reason on a muted second line.
pub(crate) fn refused_lines(
    theme: &Theme,
    refusal: &RefusedDrawing,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = super::markdown::wrap_spans(
        vec![
            Span::styled("ⓘ ", theme.label()),
            Span::styled(safe_line(&refusal.text), theme.text()),
        ],
        width,
        2,
    );
    lines.extend(super::markdown::wrap_spans(
        vec![Span::styled(format!("  refused: {}", safe_line(&refusal.reason)), theme.label())],
        width,
        2,
    ));
    lines
}

/// An app from an MCP server (spec 3.3 §6.1 `genui-apps`): never framed in a terminal, so
/// "app from Weather · open it in the home" and the tool's text result.
pub(crate) fn app_lines(theme: &Theme, app: &AppView, width: usize) -> Vec<Line<'static>> {
    let mut lines = super::markdown::wrap_spans(
        vec![Span::styled(safe_line(&app.terminal_text), theme.label())],
        width,
        2,
    );
    if !app.result_text.trim().is_empty() {
        for line in app.result_text.lines() {
            lines.extend(super::markdown::wrap_spans(
                vec![Span::styled(safe_line(line), theme.text())],
                width,
                2,
            ));
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// The fill-in pane
// ---------------------------------------------------------------------------

/// One field of the fill-in pane.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FillField {
    /// The field.
    pub field: DrawnField,
    /// Its label.
    pub label: String,
    /// What the person typed, for a text field.
    pub text: String,
    /// What it holds now, for a checkbox or a choice.
    pub value: Value,
}

/// The fill-in pane of a drawn form (spec 3.3 §5.2, wave 4 decision 19).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FillIn {
    /// The session the form is in.
    pub session: String,
    /// The form.
    pub surface: DrawnSurface,
    /// Its fields, in order.
    pub fields: Vec<FillField>,
    /// The field the keys type into.
    pub focus: usize,
    /// Enter showed the call; a second Enter sends it.
    pub preview: bool,
    /// A field's problem: its index and what is wrong.
    pub error: Option<(usize, String)>,
    /// The send is on its way.
    pub sending: bool,
    /// Why the last send failed.
    pub problem: Option<String>,
}

fn fields(node: &DrawnNode, out: &mut Vec<FillField>) {
    if let Some(field) = &node.field {
        let text = match (&field.input, &field.value) {
            (DrawnInput::Checkbox | DrawnInput::Choice | DrawnInput::Choices, _) => String::new(),
            (_, Value::String(text)) => text.clone(),
            (_, Value::Null) => String::new(),
            (_, other) => other.to_string(),
        };
        out.push(FillField {
            field: field.clone(),
            label: node.text.clone().unwrap_or_default(),
            text,
            value: field.value.clone(),
        });
    }
    for child in &node.children {
        fields(child, out);
    }
}

/// What a key did in the fill-in pane.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FillAsk {
    /// Nothing more.
    Stay,
    /// Leave the pane.
    Leave,
    /// Send the form.
    Send(Command),
}

impl FillIn {
    /// The pane of `surface` in `session`, or `None` when it has nothing to send.
    pub(crate) fn of(session: &str, surface: &DrawnSurface) -> Option<Self> {
        if surface.actions.is_empty() || surface.deleted {
            return None;
        }
        let mut out = Vec::new();
        if let Some(root) = &surface.root {
            fields(root, &mut out);
        }
        Some(Self {
            session: session.to_string(),
            surface: surface.clone(),
            fields: out,
            focus: 0,
            preview: false,
            error: None,
            sending: false,
            problem: None,
        })
    }

    /// The action the pane sends: the form's first.
    fn action(&self) -> Option<&DrawnActionView> {
        self.surface.actions.first()
    }

    /// What field `index` holds, as it is sent.
    fn value(&self, index: usize) -> Value {
        let field = &self.fields[index];
        match field.field.input {
            DrawnInput::Checkbox | DrawnInput::Choice | DrawnInput::Choices => field.value.clone(),
            // A number is sent as one when it reads as one; the server says when it does not.
            DrawnInput::Number | DrawnInput::Slider => {
                let text = field.text.trim();
                text.parse::<i64>()
                    .map(Value::from)
                    .ok()
                    .or_else(|| {
                        text.parse::<f64>()
                            .ok()
                            .and_then(serde_json::Number::from_f64)
                            .map(Value::Number)
                    })
                    .unwrap_or_else(|| Value::String(field.text.clone()))
            }
            _ => Value::String(field.text.clone()),
        }
    }

    /// Every field's value, by data path.
    pub(crate) fn values(&self) -> Map<String, Value> {
        (0..self.fields.len())
            .map(|index| (self.fields[index].field.path.clone(), self.value(index)))
            .collect()
    }

    /// The first field whose value its checks refuse, with what is wrong.
    fn problem(&self) -> Option<(usize, String)> {
        #[cfg(feature = "genui")]
        {
            let mut model = crate::genui::DataModel::new(json!({}));
            for (index, field) in self.fields.iter().enumerate() {
                match crate::genui::typed_value(&field.field, &self.value(index)) {
                    Ok(value) => {
                        let _ = model.apply(Some(&field.field.path), Some(value));
                    }
                    Err(why) => return Some((index, why)),
                }
            }
            for (index, field) in self.fields.iter().enumerate() {
                if let Some(check) = field
                    .field
                    .checks
                    .iter()
                    .find(|check| !crate::genui::check_holds(&check.condition, &model))
                {
                    return Some((index, check.message.clone()));
                }
            }
        }
        None
    }

    /// The call a send asks for, as the preview shows it:
    /// `send_invoice(to: "ana@example.com", amount: 4200) · asks you first`.
    pub(crate) fn preview_text(&self) -> String {
        let Some(action) = self.action() else {
            return String::new();
        };
        let values = self.values();
        let args: Vec<String> = action
            .args
            .iter()
            .map(|arg| {
                let value = match (&arg.path, &arg.value) {
                    (Some(path), _) => values.get(path).cloned().unwrap_or(Value::Null),
                    (None, Some(value)) => value.clone(),
                    (None, None) => Value::Null,
                };
                format!("{}: {value}", arg.name)
            })
            .collect();
        format!("{}({}) · asks you first", action.tool, args.join(", "))
    }

    /// Takes `key`: Tab and the arrows move between fields, typing goes into the field — the
    /// decision keys included — Space ticks a checkbox and picks an option, Enter previews and
    /// then sends, Esc goes back and then leaves.
    pub(crate) fn key(&mut self, key: crossterm::event::KeyEvent) -> FillAsk {
        use crossterm::event::KeyCode;
        if self.sending {
            return FillAsk::Stay;
        }
        let count = self.fields.len();
        match key.code {
            KeyCode::Esc if self.preview => self.preview = false,
            KeyCode::Esc => return FillAsk::Leave,
            KeyCode::Enter if self.preview => {
                let Some(action) = self.action().map(|action| action.id.clone()) else {
                    return FillAsk::Leave;
                };
                self.sending = true;
                self.problem = None;
                return FillAsk::Send(Command::SubmitDrawn {
                    session_id: self.session.clone(),
                    surface_id: self.surface.surface_id.clone(),
                    version: self.surface.version,
                    action_id: action,
                    values: self.values(),
                });
            }
            KeyCode::Enter => match self.problem() {
                Some((index, why)) => {
                    self.focus = index;
                    self.error = Some((index, why));
                }
                None => {
                    self.error = None;
                    self.preview = true;
                }
            },
            _ if self.preview => {}
            KeyCode::Tab | KeyCode::Down if count > 0 => self.focus = (self.focus + 1) % count,
            KeyCode::BackTab | KeyCode::Up if count > 0 => {
                self.focus = (self.focus + count - 1) % count;
            }
            KeyCode::Backspace => {
                if let Some(field) = self.fields.get_mut(self.focus) {
                    field.text.pop();
                    self.error = None;
                }
            }
            KeyCode::Char(c) => {
                if let Some(field) = self.fields.get_mut(self.focus) {
                    self.error = None;
                    match field.field.input {
                        DrawnInput::Checkbox if c == ' ' => {
                            field.value = Value::Bool(!field.value.as_bool().unwrap_or(false));
                        }
                        DrawnInput::Choice | DrawnInput::Choices if c == ' ' => {
                            field.value = next_choice(&field.field, &field.value);
                        }
                        DrawnInput::Checkbox | DrawnInput::Choice | DrawnInput::Choices => {}
                        _ => field.text.push(c),
                    }
                }
            }
            _ => {}
        }
        FillAsk::Stay
    }

    /// The pane's lines, `width` cells wide.
    pub(crate) fn lines(&self, theme: &Theme, width: usize) -> Vec<Line<'static>> {
        let title =
            format!("Fill in · {}", safe_line(self.surface.title.as_deref().unwrap_or_default()));
        let by = format!("drawn by {}", safe_line(&self.surface.agent.display_name));
        let title = fit(&title, width.saturating_sub(by.width() + 2));
        let gap = width.saturating_sub(title.width() + by.width());
        let mut lines = vec![
            Line::from(vec![
                Span::styled(title, theme.text().add_modifier(Modifier::BOLD)),
                Span::raw(" ".repeat(gap)),
                Span::styled(by, theme.label()),
            ]),
            Line::from(Span::styled(FILL_IN_INTRO, theme.label())),
            Line::default(),
        ];
        let label_width = self
            .fields
            .iter()
            .map(|field| safe_line(&field.label).width())
            .max()
            .unwrap_or(0)
            .min(width / 3);
        for (index, field) in self.fields.iter().enumerate() {
            let label = fit(&safe_line(&field.label), label_width);
            let pad = label_width.saturating_sub(label.width()) + 2;
            let value = match field.field.input {
                DrawnInput::Checkbox => {
                    if field.value.as_bool().unwrap_or(false) { "[x]" } else { "[ ]" }.to_string()
                }
                DrawnInput::Choice | DrawnInput::Choices => {
                    format!("‹ {} ›", shown_value(&field.field, Some(&field.value)))
                }
                _ => safe_line(&field.text),
            };
            let focused = index == self.focus && !self.preview;
            let style =
                if focused { theme.text().add_modifier(Modifier::REVERSED) } else { theme.text() };
            let mut spans = vec![
                Span::styled(label, theme.label()),
                Span::raw(" ".repeat(pad)),
                Span::styled(fit(&value, width.saturating_sub(label_width + 3)), style),
            ];
            if focused {
                spans.push(Span::styled(" ", theme.text().add_modifier(Modifier::REVERSED)));
            }
            lines.push(Line::from(spans));
            if let Some((at, why)) = &self.error
                && *at == index
            {
                lines.push(Line::from(Span::styled(
                    fit(&lower_first(&safe_line(why)), width),
                    theme.accent(),
                )));
            }
        }
        lines.push(Line::default());
        if self.preview {
            lines.push(Line::from(Span::styled("─".repeat(width), theme.border())));
            lines.extend(super::markdown::wrap_spans(
                vec![Span::styled(safe_line(&self.preview_text()), theme.text())],
                width,
                0,
            ));
            let line = if self.sending {
                lower_first(words::SENDING)
            } else {
                "enter again to send · esc to go back".to_string()
            };
            lines.push(Line::from(Span::styled(line, theme.accent())));
        }
        if let Some(problem) = &self.problem {
            lines.extend(super::markdown::wrap_spans(
                vec![Span::styled(safe_line(problem), theme.accent())],
                width,
                0,
            ));
        }
        lines.push(Line::from(Span::styled(
            if self.preview { PREVIEW_KEYS } else { FILL_IN_KEYS },
            theme.label(),
        )));
        lines
    }
}

/// The value after `value` in `field`'s options, round to none.
fn next_choice(field: &DrawnField, value: &Value) -> Value {
    let current = value.as_array().and_then(|chosen| chosen.first()).and_then(Value::as_str);
    let index =
        current.and_then(|current| field.options.iter().position(|option| option.value == current));
    match index.map_or(0, |index| index + 1) {
        next if next < field.options.len() => json!([field.options[next].value]),
        _ => json!([]),
    }
}

// ---------------------------------------------------------------------------
// JSON lines
// ---------------------------------------------------------------------------

/// The `drawn`, `drawn.submitted`, `drawn.refused` and `app` lines of `entry` in `session`
/// (spec 3.3 §5.2 `--output json`, §6.1 `genui-apps`).
pub(crate) fn json_lines(session: &str, entry: &FeedEntry) -> Vec<Value> {
    entry
        .items
        .iter()
        .filter_map(|item| match item {
            ThreadItem::Drawn { surface } => Some(json!({
                "type": "drawn",
                "session": session,
                "surface": surface.surface_id,
                "version": surface.version,
                "by": surface.agent.name,
                "catalog": surface.catalog.replace(' ', "/"),
                "kind": surface.kind,
                "title": surface.title,
                "deleted": surface.deleted,
            })),
            ThreadItem::DrawnSent { sent } if sent.via == crate::genui::SentVia::App => {
                Some(json!({
                    "type": "app.asked",
                    "session": session,
                    "call": sent.surface_id,
                    "tool": sent.tool,
                    "held": sent.held,
                    "decision": sent.decision_id,
                }))
            }
            ThreadItem::DrawnSent { sent } => Some(json!({
                "type": "drawn.submitted",
                "session": session,
                "surface": sent.surface_id,
                "version": sent.version,
                "action": sent.action_id,
                "tool": sent.tool,
                "held": sent.held,
                "decision": sent.decision_id,
            })),
            ThreadItem::DrawnRefused { refusal } => Some(json!({
                "type": "drawn.refused",
                "session": session,
                "by": refusal.agent.name,
                "reason": refusal.kind,
                "component": refusal.component,
                "detail": refusal.reason,
            })),
            ThreadItem::App { app } => Some(json!({
                "type": "app",
                "session": session,
                "call": app.call_id,
                "server": app.server,
                "tool": app.tool,
                "by": app.agent.name,
                "text": app.result_text,
            })),
            ThreadItem::Message { .. }
            | ThreadItem::Activity { .. }
            | ThreadItem::Result { .. }
            | ThreadItem::Decision { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use proptest::prelude::*;

    use super::*;
    use crate::genui::{DrawnArg, DrawnCheck, DrawnKind};
    use crate::model::{AgentRef, Avatar};
    use crate::tui::theme::Appearance;

    fn field(
        key: &str,
        label: &str,
        input: DrawnInput,
        value: Value,
        checks: Vec<DrawnCheck>,
    ) -> DrawnNode {
        DrawnNode {
            key: key.into(),
            component: "TextField".into(),
            text: Some(label.into()),
            variant: None,
            field: Some(DrawnField {
                path: format!("/{key}"),
                input,
                value,
                options: Vec::new(),
                min: None,
                max: None,
                checks,
            }),
            action: None,
            tabs: Vec::new(),
            show_all_text: None,
            children: Vec::new(),
        }
    }

    /// The invoice form of the concept, titled `title`, its To field labelled `label`.
    fn form(title: &str, label: &str) -> DrawnSurface {
        let email = DrawnCheck {
            condition: json!({ "call": "email", "args": { "value": { "path": "/to" } } }),
            message: "Enter an email address".into(),
        };
        DrawnSurface {
            surface_id: "invoice-10".into(),
            version: 3,
            kind: DrawnKind::Form,
            catalog: "forms 1.0".into(),
            agent: AgentRef {
                name: "finance".into(),
                display_name: "Finance".into(),
                avatar: Avatar { monogram: "F".into(), color: "#15803D".into() },
            },
            drawn_at: chrono::Utc::now(),
            at_text: "10:02".into(),
            drawn_text: "Drawn by Finance · 10:02".into(),
            changed_text: "Finance changed this form at 10:02".into(),
            earlier_text: "Earlier version · 10:02".into(),
            title: Some(title.into()),
            root: Some(DrawnNode {
                key: "body".into(),
                component: "Column".into(),
                text: None,
                variant: None,
                field: None,
                action: None,
                tabs: Vec::new(),
                show_all_text: None,
                children: vec![
                    field("to", label, DrawnInput::ShortText, json!(""), vec![email]),
                    field("amount", "Amount", DrawnInput::Number, json!(4200), Vec::new()),
                    field("copy", "Copy to me", DrawnInput::Checkbox, json!(false), Vec::new()),
                ],
            }),
            actions: vec![DrawnActionView {
                id: "send".into(),
                label: "Send".into(),
                tool: "send_invoice".into(),
                note: words::sending_runs("send_invoice"),
                args: vec![
                    DrawnArg { name: "to".into(), path: Some("/to".into()), value: None },
                    DrawnArg { name: "amount".into(), path: Some("/amount".into()), value: None },
                    DrawnArg { name: "invoice".into(), path: None, value: Some(json!("INV-10")) },
                ],
            }],
            deleted: false,
            call_id: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Spec 3.3 §5.2, wave 4 decision 19: in the fill-in pane the decision keys type into the
    /// field, Enter previews the call, a second Enter sends it, and Esc goes back, then leaves.
    #[test]
    fn the_fill_in_pane_types_previews_and_sends() {
        let mut fill = FillIn::of("s1", &form("Invoice INV-10", "To")).unwrap();
        for c in "yana@example.com".chars() {
            assert_eq!(fill.key(key(KeyCode::Char(c))), FillAsk::Stay);
        }
        fill.key(key(KeyCode::Home));
        // `y`, `n`, `e` and `a` answer nothing here: they are what the person typed.
        assert_eq!(fill.fields[0].text, "yana@example.com");
        fill.key(key(KeyCode::Tab));
        fill.key(key(KeyCode::Tab));
        fill.key(key(KeyCode::Char(' ')));
        assert_eq!(fill.fields[2].value, json!(true));
        assert_eq!(fill.key(key(KeyCode::Enter)), FillAsk::Stay);
        assert!(fill.preview);
        assert_eq!(
            fill.preview_text(),
            "send_invoice(to: \"yana@example.com\", amount: 4200, invoice: \"INV-10\") · asks you first"
        );
        assert_eq!(fill.key(key(KeyCode::Esc)), FillAsk::Stay, "back to the fields");
        assert!(!fill.preview);
        fill.key(key(KeyCode::Enter));
        let FillAsk::Send(command) = fill.key(key(KeyCode::Enter)) else {
            panic!("a second Enter sends");
        };
        assert_eq!(
            command,
            Command::SubmitDrawn {
                session_id: "s1".into(),
                surface_id: "invoice-10".into(),
                version: 3,
                action_id: "send".into(),
                values: json!({ "/to": "yana@example.com", "/amount": 4200, "/copy": true })
                    .as_object()
                    .cloned()
                    .unwrap(),
            }
        );
        assert!(fill.sending);
        let mut leave = FillIn::of("s1", &form("Invoice INV-10", "To")).unwrap();
        assert_eq!(leave.key(key(KeyCode::Esc)), FillAsk::Leave);
    }

    /// A field whose check fails says why under it, and nothing is previewed.
    #[cfg(feature = "genui")]
    #[test]
    fn a_field_error_is_shown_under_the_field() {
        let mut fill = FillIn::of("s1", &form("Invoice INV-10", "To")).unwrap();
        for c in "ana@".chars() {
            fill.key(key(KeyCode::Char(c)));
        }
        fill.key(key(KeyCode::Enter));
        assert!(!fill.preview);
        assert_eq!(fill.error, Some((0, "Enter an email address".to_string())));
        let theme = Theme::new(Appearance::Dark, true);
        assert!(text(&fill.lines(&theme, 60)).contains("enter an email address"));
    }

    /// A render call is shown as its drawing; a same-named call without one stays a step.
    #[test]
    fn a_render_call_is_shown_as_its_drawing() {
        let activity = |id: &str| ThreadItem::Activity {
            tool: "render_form".into(),
            text: "Render form Invoice INV-10".into(),
            call_id: Some(id.into()),
        };
        let drawn = DrawnSurface { call_id: Some("c-draw".into()), ..form("Invoice INV-10", "To") };
        let entry = |seq: u64, items: Vec<ThreadItem>| FeedEntry {
            seq,
            at: chrono::Utc::now(),
            at_text: "10:02".into(),
            items,
        };
        let entries = [
            entry(1, vec![activity("c-draw")]),
            entry(2, vec![ThreadItem::Drawn { surface: Box::new(drawn) }]),
            entry(3, vec![activity("c-plain")]),
        ];
        let drawings = Drawings::of(&entries, |_| None);
        assert_eq!(drawings.calls, HashSet::from(["c-draw".to_string()]));
    }

    /// The box of a drawing: the normal border, its title and "drawn by Finance · forms ·
    /// rev 3", aligned pairs, and how to fill it in (spec 3.3 §5.2).
    #[test]
    fn a_drawing_is_a_bordered_box_with_its_fields() {
        let theme = Theme::new(Appearance::Dark, true);
        let lines = box_lines(&theme, &form("Invoice INV-10", "To"), &Drawings::default(), 80);
        let shown = text(&lines);
        assert!(shown.starts_with('┌'), "{shown}");
        assert!(shown.contains("Invoice INV-10"), "{shown}");
        assert!(shown.contains("drawn by Finance · forms · rev 3"), "{shown}");
        assert!(shown.contains("Amount      4200"), "{shown}");
        assert!(shown.contains("Copy to me  no"), "{shown}");
        assert!(shown.contains(FILL_IN_HINT), "{shown}");
        let border = lines[0].spans[0].style;
        assert_eq!(border, theme.border(), "never the decision's colour");
    }

    /// The JSON lines of a drawing, a send and a refusal (spec 3.3 §5.2 `--output json`).
    #[test]
    fn the_json_lines_name_the_drawing_the_send_and_the_refusal() {
        let entry = FeedEntry {
            seq: 4,
            at: chrono::Utc::now(),
            at_text: "10:02".into(),
            items: vec![ThreadItem::Drawn { surface: Box::new(form("Invoice INV-10", "To")) }],
        };
        let lines = json_lines("s-41c2", &entry);
        assert_eq!(lines[0]["type"], "drawn");
        assert_eq!(lines[0]["surface"], "invoice-10");
        assert_eq!(lines[0]["version"], 3);
        assert_eq!(lines[0]["by"], "finance");
        assert_eq!(lines[0]["catalog"], "forms/1.0");
    }

    /// The call that showed an app is shown as the app, and a call the app asked for is the
    /// app's, never a form's send (spec 3.3 §6.1 `genui-apps`).
    #[test]
    fn an_apps_call_is_the_apps_not_a_forms_send() {
        let app: crate::genui::AppView = serde_json::from_value(json!({
            "callId": "c-weather",
            "server": "weather",
            "appName": "Weather",
            "tool": "weather_show",
            "agent": {
                "name": "chief",
                "displayName": "Chief of staff",
                "avatar": { "monogram": "C", "color": "#C2410C" },
            },
            "at": "2026-10-27T10:05:00Z",
            "headerText": "App from Weather · it can't approve anything",
            "resultText": "Nairobi: 24°",
            "elsewhereText": "Open the home on this computer to see this app",
            "terminalText": "app from Weather · open it in the home",
        }))
        .unwrap();
        let asked = crate::genui::DrawnSent {
            surface_id: "c-weather".into(),
            version: 1,
            action_id: "set_rain_alert".into(),
            tool: "set_rain_alert".into(),
            call_id: "adk-call-requested-1".into(),
            decision_id: Some("apr-1".into()),
            held: true,
            at: chrono::Utc::now(),
            text: "The app asked · 10:06".into(),
            via: crate::genui::SentVia::App,
        };
        let entry = FeedEntry {
            seq: 2,
            at: chrono::Utc::now(),
            at_text: "10:06".into(),
            items: vec![
                ThreadItem::Activity {
                    tool: "weather_show".into(),
                    text: "Weather show city: Nairobi".into(),
                    call_id: Some("c-weather".into()),
                },
                ThreadItem::App { app: Box::new(app) },
                ThreadItem::DrawnSent { sent: asked },
            ],
        };
        let drawings = Drawings::of(std::slice::from_ref(&entry), |_| None);
        assert_eq!(drawings.calls, HashSet::from(["c-weather".to_string()]));
        assert!(drawings.forms.is_empty(), "an app's call is not a form");
        let lines = json_lines("s1", &entry);
        assert_eq!(
            lines[1],
            json!({
                "type": "app.asked",
                "session": "s1",
                "call": "c-weather",
                "tool": "set_rain_alert",
                "held": true,
                "decision": "apr-1",
            })
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// Whatever an agent draws, the terminal never writes a control sequence
        /// (spec 3.3 §11 S-I9).
        #[test]
        fn a_drawing_never_writes_a_control_sequence(
            title in "\\PC{0,8}[\u{1b}\u{7}\u{9b}\r\u{202e}]\\PC{0,8}",
            label in "\\PC{0,4}\u{1b}\\[31m\\PC{0,4}",
        ) {
            let theme = Theme::new(Appearance::Light, false);
            let surface = form(&title, &label);
            let mut fill = FillIn::of("s1", &surface).unwrap();
            fill.preview = true;
            let mut lines = box_lines(&theme, &surface, &Drawings::default(), 72);
            lines.extend(inline_lines(&theme, &surface, 72));
            lines.extend(fill.lines(&theme, 44));
            let shown = text(&lines);
            prop_assert!(!shown.chars().any(|c| c.is_control() && c != '\n'), "{shown:?}");
            prop_assert!(!shown.contains('\u{202e}'), "{shown:?}");
        }
    }
}
