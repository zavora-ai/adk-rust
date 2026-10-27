//! Apps from MCP servers in a thread (spec 3.3 §6.1 `genui-apps`), as every surface reads
//! them: the thread item, what opening it answers, and the app's requests to the home. Wire
//! types of every `web` build, so the page is the same for every server; only a server built
//! with `genui-apps` projects an app or answers its routes.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::AgentRef;

/// An app an MCP server's tool showed in a thread (spec 3.3 §6.1 `genui-apps`): on the normal
/// card background, labelled as unable to approve anything. The page opens it sandboxed on a
/// second origin; the phone, the terminal and chat show its text result instead.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::AppView;
///
/// let wire = serde_json::json!({
///     "callId": "call-1",
///     "server": "weather",
///     "appName": "Weather",
///     "tool": "weather_show",
///     "agent": {
///         "name": "chief",
///         "displayName": "Chief of staff",
///         "avatar": { "monogram": "C", "color": "#C2410C" },
///     },
///     "at": "2026-10-27T10:05:00Z",
///     "headerText": "App from Weather · it can't approve anything",
///     "resultText": "Nairobi: 24°, light rain from 15:00",
///     "elsewhereText": "Open the home on this computer to see this app",
///     "terminalText": "app from Weather · open it in the home",
/// });
/// let app: AppView = serde_json::from_value(wire).unwrap();
/// assert_eq!(app.app_name, "Weather");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppView {
    /// The call that showed it: the app's id in its session.
    pub call_id: String,
    /// The MCP server, by its name on this home.
    pub server: String,
    /// The server as a person reads it: "Weather".
    pub app_name: String,
    /// The tool that showed it, by its name on the server.
    pub tool: String,
    /// The agent whose call showed it.
    pub agent: AgentRef,
    /// When.
    pub at: DateTime<Utc>,
    /// "App from Weather · it can't approve anything".
    pub header_text: String,
    /// The tool's text result, which every surface can show.
    pub result_text: String,
    /// "Open the home on this computer to see this app": where the app is not shown.
    pub elsewhere_text: String,
    /// "app from Weather · open it in the home": the terminal's line.
    pub terminal_text: String,
}

/// Why an app was blocked (spec 3.3 §6.1 `genui-apps`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AppBlockReason {
    /// Its HTML shows a decision's words: "This app was blocked: it shows approval buttons".
    ApprovalButtons,
    /// It loads or connects to a site its declaration does not list, or declares one in a form
    /// this home does not accept.
    UndeclaredSites,
    /// It asks to reach this computer, its network, or the home itself.
    ThisComputer,
    /// Its resource is not an MCP App's HTML.
    NotAnApp,
}

/// What `GET /surface/genui/sessions/{session}/apps/{call}` answers (spec 3.3 §6.1
/// `genui-apps`): the app to frame, why it was blocked, or that it is not shown here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum AppOpen {
    /// Frame `frameUrl`, on the apps origin, sandboxed.
    #[serde(rename_all = "camelCase")]
    Shown {
        /// The app's page on the apps origin.
        frame_url: String,
        /// The apps origin, which the page frames and nothing else.
        frame_origin: String,
        /// The tallest the frame grows, in CSS pixels.
        max_height: u32,
        /// Whether the app asked for a border around it.
        prefers_border: bool,
        /// The arguments of the call that showed it: `ui/notifications/tool-input`.
        tool_input: Value,
        /// Its result as an MCP `CallToolResult`: `ui/notifications/tool-result`.
        tool_result: Value,
        /// The device permissions it asked for, all refused here.
        refused_permissions: Vec<String>,
    },
    /// Not shown: "This app was blocked: it shows approval buttons".
    #[serde(rename_all = "camelCase")]
    Blocked {
        /// Why.
        reason: AppBlockReason,
        /// What the thread says.
        text: String,
        /// The technical reason, for the developer panel.
        detail: String,
    },
    /// Not shown on this surface or without an apps origin: "Open the home on this computer
    /// to see this app", and the tool's text result.
    #[serde(rename_all = "camelCase")]
    Elsewhere {
        /// What the thread says.
        text: String,
        /// The tool's text result.
        result_text: String,
    },
}

/// A request an app made of the home, as the page relays it: `POST
/// /surface/genui/sessions/{session}/apps/{call}/requests` (spec 3.3 §6.1 `genui-apps`). Only
/// requests with an `id` reach the home; the page answers notifications itself.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::AppRequest;
///
/// let request: AppRequest = serde_json::from_value(serde_json::json!({
///     "method": "tools/call",
///     "params": { "name": "set_rain_alert", "arguments": { "city": "Nairobi" } },
/// }))
/// .unwrap();
/// assert_eq!(request.method, "tools/call");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppRequest {
    /// The JSON-RPC method: `ui/initialize`, `tools/call`, `ui/message`, `ui/open-link`,
    /// `ui/update-model-context`, `ping`.
    pub method: String,
    /// Its parameters.
    #[serde(default)]
    pub params: Value,
}

/// What the page shows a person for an app's request (spec 3.3 §6.1 `genui-apps`): the app
/// only ever asks; the person's own press does what it asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AppPrompt {
    /// A plain notice under the app: "The app tried to use transfer_funds. Apps can't use it."
    #[serde(rename_all = "camelCase")]
    Notice {
        /// The notice.
        text: String,
    },
    /// "The app wants to send: “…”", with **Send it** and **Not now**.
    #[serde(rename_all = "camelCase")]
    Message {
        /// "The app wants to send: “Remind me to take an umbrella tomorrow.”"
        text: String,
        /// The message the app suggests.
        message: String,
        /// "Send it".
        send_label: String,
        /// "Not now".
        dismiss_label: String,
        /// "It will be marked as suggested by the app".
        note: String,
    },
    /// "Open example.com?", with **Open** and **Cancel**.
    #[serde(rename_all = "camelCase")]
    Link {
        /// "Open example.com?"
        text: String,
        /// The link, an `https` or `http` URL.
        url: String,
        /// "Open".
        open_label: String,
        /// "Cancel".
        dismiss_label: String,
    },
}

/// The home's answer to an [`AppRequest`]: the JSON-RPC `result` or `error` the page passes
/// back to the app, and what it shows the person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppAnswer {
    /// The JSON-RPC result, when the request succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The JSON-RPC error, when it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AppError>,
    /// What the page shows the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<AppPrompt>,
}

/// A JSON-RPC error for an app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    /// The JSON-RPC code.
    pub code: i64,
    /// What happened.
    pub message: String,
}

/// `POST /surface/genui/sessions/{session}/apps/{call}/message`: the person pressed **Send
/// it** on what an app suggested (spec 3.3 §6.1 `genui-apps`). The home sends it as the
/// person's message, inside an untrusted-content block labelled as the app's suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppMessageRequest {
    /// The message the app suggested.
    pub message: String,
}

/// An MCP server's name as a person reads it: "weather" is "Weather", "acme_crm" is
/// "Acme crm".
#[cfg(feature = "genui")]
pub(crate) fn app_name(server: &str) -> String {
    let spaced: String =
        server.chars().map(|c| if c == '_' || c == '-' { ' ' } else { c }).collect();
    let mut chars = spaced.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => server.to_string(),
    }
}
