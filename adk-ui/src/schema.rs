use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MIME_TYPE_UI: &str = "application/vnd.adk.ui+json";
pub const MIME_TYPE_UI_UPDATE: &str = "application/vnd.adk.ui.update+json";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Component {
    // Atoms
    Text(Text),
    Button(Button),
    Icon(Icon),
    Image(Image),
    Badge(Badge),

    // Inputs
    TextInput(TextInput),
    NumberInput(NumberInput),
    Select(Select),
    MultiSelect(MultiSelect),
    Switch(Switch),
    DateInput(DateInput),
    Slider(Slider),

    // Layouts
    Stack(Stack),
    Grid(Grid),
    Card(Card),
    Container(Container),
    Divider(Divider),
    Tabs(Tabs),

    // Data Display
    Table(Table),
    List(List),
    KeyValue(KeyValue),
    CodeBlock(CodeBlock),

    // Visualizations
    Chart(Chart),

    // Feedback
    Alert(Alert),
    Progress(Progress),
    Toast(Toast),
    Modal(Modal),
    Spinner(Spinner),
    Skeleton(Skeleton),

    // Extended Inputs
    Textarea(Textarea),
}

// --- Atoms ---

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Text {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub content: String,
    #[serde(default)]
    pub variant: TextVariant,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum TextVariant {
    H1,
    H2,
    H3,
    H4,
    #[default]
    Body,
    Caption,
    Code,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Button {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub label: String,
    pub action_id: String,
    #[serde(default)]
    pub variant: ButtonVariant,
    #[serde(default)]
    pub disabled: bool,
    /// Optional icon name (Lucide icon) to display with the button
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ButtonVariant {
    #[default]
    Primary,
    Secondary,
    Danger,
    Ghost,
    Outline,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Icon {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String, // Lucide icon name
    #[serde(default)]
    pub size: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Image {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub src: String,
    pub alt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Badge {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub label: String,
    #[serde(default)]
    pub variant: BadgeVariant,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum BadgeVariant {
    #[default]
    Default,
    Info,
    Success,
    Warning,
    Error,
    Secondary,
    Outline,
}

// --- Inputs ---

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TextInput {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub label: String,
    /// Input type: text, email, password, tel, url
    #[serde(default = "default_input_type")]
    pub input_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    /// Minimum length for text input validation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    /// Maximum length for text input validation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn default_input_type() -> String {
    "text".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NumberInput {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default)]
    pub required: bool,
    /// Default value for the number input
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Select {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub label: String,
    pub options: Vec<SelectOption>,
    #[serde(default)]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultiSelect {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub label: String,
    pub options: Vec<SelectOption>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SelectOption {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Switch {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub default_checked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DateInput {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Slider {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub label: String,
    pub min: f64,
    pub max: f64,
    pub step: Option<f64>,
    pub default_value: Option<f64>,
}

// --- Layouts ---

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Stack {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub direction: StackDirection,
    pub children: Vec<Component>,
    #[serde(default)]
    pub gap: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StackDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Grid {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub columns: u8,
    pub children: Vec<Component>,
    #[serde(default)]
    pub gap: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Card {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub content: Vec<Component>,
    pub footer: Option<Vec<Component>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Container {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub children: Vec<Component>,
    #[serde(default)]
    pub padding: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Divider {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Tabs {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub tabs: Vec<Tab>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Tab {
    pub label: String,
    pub content: Vec<Component>,
}

// --- Data Display ---

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Table {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub columns: Vec<TableColumn>,
    pub data: Vec<HashMap<String, serde_json::Value>>,
    /// Enable sorting on columns
    #[serde(default)]
    pub sortable: bool,
    /// Enable pagination
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
    /// Striped row styling
    #[serde(default)]
    pub striped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TableColumn {
    pub header: String,
    pub accessor_key: String,
    /// Whether this column is sortable (when table.sortable is true)
    #[serde(default = "default_true")]
    pub sortable: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct List {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub items: Vec<String>,
    #[serde(default)]
    pub ordered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KeyValue {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub pairs: Vec<KeyValuePair>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KeyValuePair {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CodeBlock {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub code: String,
    pub language: Option<String>,
}

// --- Visualizations ---

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Chart {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub kind: ChartKind,
    pub data: Vec<HashMap<String, serde_json::Value>>,
    pub x_key: String,
    pub y_keys: Vec<String>,
    /// X-axis label
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_label: Option<String>,
    /// Y-axis label
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y_label: Option<String>,
    /// Show legend
    #[serde(default = "default_show_legend")]
    pub show_legend: bool,
    /// Custom colors for data series (hex values)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colors: Option<Vec<String>>,
}

fn default_show_legend() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChartKind {
    Bar,
    Line,
    Area,
    Pie,
}

// --- Feedback ---

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Alert {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    #[serde(default)]
    pub variant: AlertVariant,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlertVariant {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Progress {
    /// Optional ID for streaming updates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub value: u8, // 0-100
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Toast {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub message: String,
    #[serde(default)]
    pub variant: AlertVariant,
    /// Duration in ms, default 5000
    #[serde(default = "default_toast_duration")]
    pub duration: u32,
    #[serde(default = "default_true")]
    pub dismissible: bool,
}

fn default_toast_duration() -> u32 {
    5000
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Modal {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
    pub content: Vec<Component>,
    pub footer: Option<Vec<Component>>,
    #[serde(default)]
    pub size: ModalSize,
    #[serde(default = "default_true")]
    pub closable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModalSize {
    Small,
    #[default]
    Medium,
    Large,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Spinner {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub size: SpinnerSize,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpinnerSize {
    Small,
    #[default]
    Medium,
    Large,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Skeleton {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub variant: SkeletonVariant,
    pub width: Option<String>,
    pub height: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkeletonVariant {
    #[default]
    Text,
    Circle,
    Rectangle,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Textarea {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub label: String,
    pub placeholder: Option<String>,
    #[serde(default = "default_textarea_rows")]
    pub rows: u8,
    #[serde(default)]
    pub required: bool,
    pub default_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn default_textarea_rows() -> u8 {
    4
}

// --- Root Response ---

/// Theme configuration for UI rendering
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UiResponse {
    /// Unique ID for this UI response (for updates)
    #[serde(default)]
    pub id: Option<String>,
    /// Theme preference
    #[serde(default)]
    pub theme: Theme,
    /// Components to render
    pub components: Vec<Component>,
}

impl UiResponse {
    pub fn new(components: Vec<Component>) -> Self {
        Self { id: None, theme: Theme::default(), components }
    }

    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn to_content(self) -> adk_core::Content {
        let json = serde_json::to_vec(&self).unwrap_or_default();
        adk_core::Content {
            role: adk_core::types::Role::Model,
            parts: vec![adk_core::Part::InlineData {
                mime_type: MIME_TYPE_UI.parse().unwrap(),
                data: json.into(),
            }],
        }
    }
}

// --- User Events (UI → Agent) ---

/// Event sent from UI to agent when user interacts with components
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum UiEvent {
    /// Form submission with collected field values
    FormSubmit {
        /// The action_id from the submit button
        action_id: String,
        /// Form field values as key-value pairs
        data: HashMap<String, serde_json::Value>,
    },
    /// Button click (non-form)
    ButtonClick {
        /// The action_id from the button
        action_id: String,
    },
    /// Value changed in an input field
    InputChange {
        /// Field name
        name: String,
        /// New value
        value: serde_json::Value,
    },
    /// Tab navigation
    TabChange {
        /// Tab index selected
        index: usize,
    },
}

impl UiEvent {
    /// Convert UI event to a user message for the agent
    pub fn to_user_message(&self) -> String {
        match self {
            UiEvent::FormSubmit { action_id, data } => {
                let json = serde_json::to_string_pretty(data).unwrap_or_default();
                format!("[UI Event: Form submitted]\nAction: {}\nData:\n{}", action_id, json)
            }
            UiEvent::ButtonClick { action_id } => {
                format!("[UI Event: Button clicked]\nAction: {}", action_id)
            }
            UiEvent::InputChange { name, value } => {
                format!("[UI Event: Input changed]\nField: {}\nValue: {}", name, value)
            }
            UiEvent::TabChange { index } => {
                format!("[UI Event: Tab changed]\nIndex: {}", index)
            }
        }
    }

    /// Convert to Content for sending to agent
    pub fn to_content(&self) -> adk_core::Content {
        adk_core::Content::user().with_text(self.to_user_message())
    }
}

// --- Streaming Updates (Agent → UI) ---

/// Operation type for streaming UI updates
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiOperation {
    /// Replace entire component
    Replace,
    /// Merge with existing component data
    Patch,
    /// Append children to a container
    Append,
    /// Remove the component
    Remove,
}

/// Incremental UI update for streaming
///
/// Allows agents to update specific components by ID without
/// re-rendering the entire UI.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UiUpdate {
    /// Target component ID to update
    pub target_id: String,
    /// Operation to perform
    pub operation: UiOperation,
    /// Payload data (component for replace/patch/append, None for remove)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Component>,
}

impl UiUpdate {
    /// Create a replace update
    pub fn replace(target_id: impl Into<String>, component: Component) -> Self {
        Self {
            target_id: target_id.into(),
            operation: UiOperation::Replace,
            payload: Some(component),
        }
    }

    /// Create a remove update
    pub fn remove(target_id: impl Into<String>) -> Self {
        Self { target_id: target_id.into(), operation: UiOperation::Remove, payload: None }
    }

    /// Create an append update (for containers)
    pub fn append(target_id: impl Into<String>, component: Component) -> Self {
        Self {
            target_id: target_id.into(),
            operation: UiOperation::Append,
            payload: Some(component),
        }
    }

    /// Convert to Content for sending via Events
    pub fn to_content(self) -> adk_core::Content {
        let json = serde_json::to_vec(&self).unwrap_or_default();
        adk_core::Content {
            role: adk_core::types::Role::Model,
            parts: vec![adk_core::Part::InlineData {
                mime_type: MIME_TYPE_UI_UPDATE.parse().unwrap(),
                data: json.into(),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_serialization_roundtrip() {
        let text = Component::Text(Text {
            id: Some("text-1".to_string()),
            content: "Hello".to_string(),
            variant: TextVariant::Body,
        });

        let json = serde_json::to_string(&text).unwrap();
        let deserialized: Component = serde_json::from_str(&json).unwrap();

        if let Component::Text(t) = deserialized {
            assert_eq!(t.content, "Hello");
            assert_eq!(t.id, Some("text-1".to_string()));
        } else {
            panic!("Expected Text component");
        }
    }

    #[test]
    fn test_ui_response_with_id() {
        let ui = UiResponse::new(vec![]).with_id("response-123").with_theme(Theme::Dark);

        assert_eq!(ui.id, Some("response-123".to_string()));
        assert!(matches!(ui.theme, Theme::Dark));
    }

    #[test]
    fn test_badge_variants_serialize() {
        let badge = Badge { id: None, label: "Test".to_string(), variant: BadgeVariant::Success };
        let json = serde_json::to_string(&badge).unwrap();
        assert!(json.contains("success"));
    }

    #[test]
    fn test_ui_event_to_message() {
        let event = UiEvent::FormSubmit { action_id: "submit".to_string(), data: HashMap::new() };
        let msg = event.to_user_message();
        assert!(msg.contains("Form submitted"));
        assert!(msg.contains("submit"));
    }

    #[test]
    fn test_ui_update_replace() {
        let update = UiUpdate::replace(
            "target-1",
            Component::Text(Text {
                id: None,
                content: "Updated".to_string(),
                variant: TextVariant::Body,
            }),
        );

        assert_eq!(update.target_id, "target-1");
        assert!(matches!(update.operation, UiOperation::Replace));
        assert!(update.payload.is_some());
    }

    #[test]
    fn test_ui_update_remove() {
        let update = UiUpdate::remove("to-delete");
        assert_eq!(update.target_id, "to-delete");
        assert!(matches!(update.operation, UiOperation::Remove));
        assert!(update.payload.is_none());
    }

    #[test]
    fn test_key_value_pairs() {
        let kv = KeyValue {
            id: Some("kv-1".to_string()),
            pairs: vec![
                KeyValuePair { key: "Name".to_string(), value: "Alice".to_string() },
                KeyValuePair { key: "Age".to_string(), value: "30".to_string() },
            ],
        };

        let json = serde_json::to_string(&kv).unwrap();
        assert!(json.contains("pairs"));
        assert!(json.contains("Alice"));
    }

    #[test]
    fn test_component_with_id_skips_none() {
        let text = Component::Text(Text {
            id: None,
            content: "No ID".to_string(),
            variant: TextVariant::Body,
        });

        let json = serde_json::to_string(&text).unwrap();
        // id should not be present when None due to skip_serializing_if
        assert!(!json.contains("\"id\""));
    }
}
