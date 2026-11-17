//! What a thread shows of agent-drawn UI (spec 3.3 §6.1 `genui`, §7.4 rule 5).
//!
//! | Thread item | Projected from | Shows |
//! |---|---|---|
//! | [`DrawnSurface`] | A render tool's result whose event writes `adk.surface.drawn` with the digest of the result's drawing, checked again by the validator | The drawing, every string resolved on the server; a field carries only the value the person types |
//! | [`RefusedDrawing`] | A render tool's refusal whose event writes `adk.surface.refused` | A plain notice; the technical reason for the developer panel and the terminal |
//! | [`DrawnSent`] | The events an agent writes for a call a person asked for (`adk.requested`) | "Sent at 10:03", or why the agent cannot use the tool |
//!
//! A tool of the same name without the state key — an MCP server's `render_form`, say — is
//! shown as an ordinary tool result. Only the latest version of a surface is live; a
//! drawing's status ([`FormState`]) comes from its decision and its call's result, never from
//! the click.
//!
//! These are wire types of every `web` build, whatever the features, so the page is the same
//! for every server; a server built without `genui` never projects them.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::scene::{DrawnDepth, DrawnScene};
use crate::model::AgentRef;

/// What a drawing is: a form a person fills in and sends, a card or table to read, or a
/// screen of catalog components.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::DrawnKind;
///
/// assert_eq!(serde_json::to_value(DrawnKind::Form).unwrap(), "form");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DrawnKind {
    /// Fields and actions.
    Form,
    /// Label and value rows; no buttons.
    Card,
    /// Columns and rows; no buttons.
    Table,
    /// Any catalog components, with declared actions.
    Screen,
}

/// Why a drawing was refused, in a person's terms (spec 3.3 §7.4 rule 5).
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::RefusalKind;
///
/// assert_eq!(serde_json::to_value(RefusalKind::AsksForSecret).unwrap(), "asksForSecret");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RefusalKind {
    /// It shows a decision's words or controls.
    LooksLikeDecision,
    /// It asks for a password or another secret.
    AsksForSecret,
    /// It uses something this home does not show: a component outside the catalog, a link,
    /// a function, or a malformed tree.
    CannotShow,
}

/// A drawing in a thread (spec 3.3 §6.1 `genui`): drawn on the normal card background,
/// never the decision's, with its strings already resolved on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnSurface {
    /// The surface; a later drawing of it is its next version.
    pub surface_id: String,
    /// This version. Only the latest version of a surface is live.
    pub version: u64,
    /// What it is.
    pub kind: DrawnKind,
    /// The catalog it was checked against, such as `forms 1.0`.
    pub catalog: String,
    /// The agent that drew it.
    pub agent: AgentRef,
    /// When it was drawn.
    pub drawn_at: DateTime<Utc>,
    /// Local `HH:MM` it was drawn at.
    pub at_text: String,
    /// "Drawn by Finance · 10:02".
    pub drawn_text: String,
    /// What a newer version says above itself: "Finance changed this form at 10:05", or
    /// "Finance removed this at 10:06" for a removal.
    pub changed_text: String,
    /// What this version says once a newer one replaces it: "Earlier version · 10:02".
    pub earlier_text: String,
    /// The title, agent text checked by rule 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What it shows; `None` for a removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<DrawnNode>,
    /// What its buttons ask for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<DrawnActionView>,
    /// This version removes the surface.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    /// The render tool's call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

/// One component of a drawing, resolved for a renderer: templates expanded, every string a
/// person reads already in place (spec 3.3 §7.4 rule 5).
///
/// | Component | `text` | `variant` | Other |
/// |---|---|---|---|
/// | `Text` | The text | `h1`–`h5`, `caption`, `body` | — |
/// | `Row`, `Column`, `List` | — | `justify` (Row, Column) or `direction` (List) | `children`; a long `List` has `showAllText` |
/// | `Card` | — | — | `children` |
/// | `Tabs` | — | — | `tabs` titles and one child per tab |
/// | `Divider` | — | `axis` | — |
/// | `Button` | Its label | `primary`, `default`, `borderless` | `action` |
/// | `TextField`, `DateTimeInput`, `CheckBox`, `ChoicePicker`, `Slider` | The label | the catalog's variant | `field` |
/// | `Chart` | — | — | `chart`: a chart an extension component's home view draws |
/// | `Scene` | — | — | `scene`: a 3D scene an extension component's home view draws; its children are its panels, then its text view |
/// | `Depth` | — | — | `depth`: the children an extension component's home view holds, presented with CSS 3D |
/// | An extension component, such as `Metric` | — | — | `extension`, and one child: what the home shows for it |
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnNode {
    /// Unique within the drawing: the component's id, and for a template's copy the list
    /// item it was drawn for, `cell-0@/rows/3`.
    pub key: String,
    /// The catalog component.
    pub component: String,
    /// What it reads, or its label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// A style hint from the catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// A field the person fills in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<DrawnField>,
    /// The declared action a button asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// The titles of a `Tabs`' tabs, one per child.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<String>,
    /// A long list's "Show all 42", when a renderer cuts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_all_text: Option<String>,
    /// Its children, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<DrawnNode>,
    /// A chart the home draws natively, from an extension component's home view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart: Option<DrawnChart>,
    /// A 3D scene the home draws natively, from an extension component's home view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<DrawnScene>,
    /// How the home presents this node's children in depth, from an extension component's
    /// home view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<DrawnDepth>,
    /// The extension component this node draws, for a host that draws it with its own
    /// renderer; every other surface draws its one child, what the home shows for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<DrawnExtension>,
}

/// A component of an extension catalog as a host renderer reads it (spec 3.3 §6.1 `genui`,
/// extension catalogs): its properties with every binding and text function resolved and
/// every string already checked by rule 5. A renderer draws it from these and nothing else,
/// and draws no control: the component only shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnExtension {
    /// The catalog's id.
    pub catalog: String,
    /// The component's type, such as `Metric`.
    pub component: String,
    /// Its properties, resolved and checked.
    pub props: Value,
    /// The keys of the nodes its slots hold, in slot order: each is in the subtree of the
    /// node's child.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,
}

/// A chart a component of an extension catalog shows in the home (spec 3.3 §6.1 `genui`,
/// extension catalogs): numbers and words the server already checked by rule 5, never
/// colours, links or markup. The home draws it in its own chart colours.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{ChartAxis, ChartKind, ChartSeries, DrawnChart};
///
/// let chart = DrawnChart {
///     kind: ChartKind::Bar,
///     x: ChartAxis::Category { labels: vec!["Q1".into(), "Q2".into()] },
///     x_label: None,
///     y_label: Some("Revenue".into()),
///     ticks: Vec::new(),
///     series: vec![ChartSeries { name: "2026".into(), x: Vec::new(), y: vec![4.into(), 7.into()] }],
///     summary: String::new(),
/// };
/// let wire = serde_json::to_value(&chart).unwrap();
/// assert_eq!(wire["x"]["type"], "category");
/// assert_eq!(wire["series"][0]["y"][1], 7);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnChart {
    /// How it is drawn.
    pub kind: ChartKind,
    /// What the horizontal axis holds.
    pub x: ChartAxis,
    /// The horizontal axis's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_label: Option<String>,
    /// The vertical axis's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_label: Option<String>,
    /// Labelled points on a number or time axis, such as "10:00" at its epoch milliseconds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ticks: Vec<ChartTick>,
    /// The series, at most eight: one per chart colour.
    pub series: Vec<ChartSeries>,
    /// One sentence that says what the chart shows, for screen readers and the terminal,
    /// written by the server: "Line chart of Revenue, 12 points, from 3100 to 9800".
    #[serde(default)]
    pub summary: String,
}

/// How a chart is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChartKind {
    /// Points joined by lines.
    Line,
    /// Lines with the area under them filled.
    Area,
    /// Bars side by side.
    Bar,
    /// Points only.
    Scatter,
    /// Slices of one whole: one series of at most eight values, none negative.
    Pie,
    /// One small line, no axes.
    Sparkline,
}

/// What a chart's horizontal axis holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChartAxis {
    /// Named categories, one value per category in every series.
    Category {
        /// The categories' names, in order.
        labels: Vec<String>,
    },
    /// Numbers: every series carries its `x` values.
    Number,
    /// Times as epoch milliseconds: every series carries its `x` values.
    Time,
}

/// One series of a chart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChartSeries {
    /// Its name, in the legend.
    pub name: String,
    /// Its horizontal values on a number or time axis; empty on a category axis.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub x: Vec<serde_json::Number>,
    /// Its values.
    pub y: Vec<serde_json::Number>,
}

/// A labelled point on a number or time axis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChartTick {
    /// Where, in the axis's values.
    pub at: serde_json::Number,
    /// What it reads.
    pub text: String,
}

/// The kind of value a drawn field takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DrawnInput {
    /// One line of text.
    ShortText,
    /// Several lines of text.
    LongText,
    /// A number.
    Number,
    /// A date, `2026-11-15`.
    Date,
    /// A time, `10:30`.
    Time,
    /// A date and a time, ISO 8601.
    DateTime,
    /// Yes or no.
    Checkbox,
    /// One of the options.
    Choice,
    /// Any of the options.
    Choices,
    /// A number between `min` and `max`.
    Slider,
}

/// A field of a drawing: where its value goes, what it takes and what it checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnField {
    /// The data model path its value is at, such as `/to`: the key of its value when the
    /// form is sent.
    pub path: String,
    /// What it takes.
    pub input: DrawnInput,
    /// The value it starts with.
    pub value: Value,
    /// The options of a choice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<DrawnOption>,
    /// The lowest value of a slider, or the earliest date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<Value>,
    /// The highest value of a slider, or the latest date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<Value>,
    /// What it checks before the form is sent, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<DrawnCheck>,
}

/// One option of a drawn choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnOption {
    /// What the person reads.
    pub label: String,
    /// The value sent.
    pub value: String,
}

/// A check a drawn field makes: the catalog's `required`, `regex`, `length`, `numeric`,
/// `email`, `and`, `or` and `not` functions, and the message shown when it fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnCheck {
    /// The A2UI function call that must be true, its bindings at data model paths.
    pub condition: Value,
    /// "Enter an email address".
    pub message: String,
}

/// A declared action of a drawing, as its button shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnActionView {
    /// The action's id.
    pub id: String,
    /// The button's label, agent text checked by rule 5.
    pub label: String,
    /// The tool sending runs.
    pub tool: String,
    /// The engine's line under the button: "Sending runs send_invoice and asks you first".
    pub note: String,
    /// The call's arguments: a field's value, or a constant the drawing fixed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<DrawnArg>,
}

/// One argument of a drawn action's call, as a preview of the call shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnArg {
    /// The argument's name.
    pub name: String,
    /// The field whose value it takes, by data path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The constant it takes, when it takes no field's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

/// A drawing that was not shown (spec 3.3 §7.4 rule 5): a plain notice in the thread; the
/// technical reason only in the developer panel and the terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefusedDrawing {
    /// The agent that tried to draw it.
    pub agent: AgentRef,
    /// Why, in a person's terms.
    pub kind: RefusalKind,
    /// "Finance tried to show something that looks like a decision. It wasn't shown."
    pub text: String,
    /// The technical reason: "catalog forms 1.0 · component c7 Button · label matches
    /// reserved word \"Allow once\" after normalisation".
    pub reason: String,
    /// The component at fault, when one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// When.
    pub at: DateTime<Utc>,
    /// Local `HH:MM:SS`.
    pub at_text: String,
    /// The render tool.
    pub tool: String,
    /// The render tool's call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

/// A person sent a drawn form: the call they asked for, held as the drawing agent for their
/// decision, or refused by its rules before it was held (spec 3.3 §7.4 rule 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DrawnSent {
    /// The surface sent.
    pub surface_id: String,
    /// The version sent.
    pub version: u64,
    /// The action used.
    pub action_id: String,
    /// The tool asked for.
    pub tool: String,
    /// The call's id: its result names it.
    pub call_id: String,
    /// The decision it waits on, once held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    /// Held for a decision; `false` when the agent's rules refused the call first.
    pub held: bool,
    /// When.
    pub at: DateTime<Utc>,
    /// "Sent at 10:03", "The app asked · 10:06", or "Finance can't use send_invoice here".
    pub text: String,
    /// What asked for it: a form the person sent, or an app from an MCP server (spec 3.3 §6.1
    /// `genui-apps`).
    #[serde(default)]
    pub via: SentVia,
}

/// What asked for a [`DrawnSent`] call (spec 3.3 §6.1 `genui`, `genui-apps`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SentVia {
    /// The person pressed Send on a form an agent drew.
    #[default]
    Form,
    /// An app from an MCP server asked, as its `tools/call`.
    App,
}

/// Where a drawn form is after it was sent, from its decision and its call's result
/// (spec 3.3 §7.4 rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FormStatus {
    /// Sent, and the call is held: its decision is not answered here yet.
    Waiting,
    /// Sent and answered yes; the call has not finished yet.
    Sent,
    /// The call ran.
    Done,
    /// The person said no.
    SaidNo,
    /// The call ran and failed.
    Failed,
    /// Allowed, then refused when it resumed because a rule changed.
    NotRun,
    /// The drawing agent's rules refused the call before it was held; the form can be
    /// changed and sent again.
    Refused,
    /// The decision ended without an answer.
    Ended,
}

/// A drawn form's state after it was sent (spec 3.3 §7.4 rule 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FormState {
    /// The surface.
    pub surface_id: String,
    /// The version sent.
    pub version: u64,
    /// Where it is.
    pub status: FormStatus,
    /// "Sent at 10:03 · waiting for you", "Sent · done at 10:04", …
    pub text: String,
    /// Its fields can no longer be changed: it was sent and the call was not refused.
    pub locked: bool,
    /// The decision it waits on or waited on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
}
