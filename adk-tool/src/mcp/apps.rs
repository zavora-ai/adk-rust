//! MCP Apps on the client side (spec 3.3 §6.1 `genui-apps`; MCP Apps 2026-01-26): what an MCP
//! server says about the interactive app a tool shows, read so a host can show it sandboxed.
//!
//! | Part | What |
//! |---|---|
//! | [`MCP_APPS_EXTENSION_ID`] | The extension [`AdkClientHandler::with_apps`](super::AdkClientHandler::with_apps) declares, with [`MCP_APP_MIME_TYPE`] |
//! | [`McpUiBinding`] | A tool's `_meta.ui`: the `ui://` resource it shows and who may call it |
//! | [`McpAppResource`] | A `ui://` resource read back: its HTML and the sites its `_meta.ui.csp` declares |
//! | [`McpAppRecord`] | What a tool with an app writes under [`MCP_APP_STATE_KEY`] when its call succeeds |
//!
//! A tool only an app may call (`visibility: ["app"]`) is left out of
//! [`McpToolset::tools`](adk_core::Toolset::tools): a model never sees it.
//! [`McpToolset::ui_bindings`](super::McpToolset::ui_bindings) lists every binding, those
//! included, so a host can refuse what an app asks for.

use adk_core::{AdkError, Result};
use rmcp::model::{ResourceContents, Tool};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The MCP Apps extension a client declares in its capabilities (spec 3.3 §6.1 `genui-apps`).
pub const MCP_APPS_EXTENSION_ID: &str = "io.modelcontextprotocol/ui";

/// The MIME type of an MCP App's HTML resource.
pub const MCP_APP_MIME_TYPE: &str = "text/html;profile=mcp-app";

/// The MCP Apps version this client reads.
pub const MCP_APPS_VERSION: &str = "2026-01-26";

/// Session state key a tool with an app writes when its call succeeds: an [`McpAppRecord`],
/// so a host shows the app only for a call an MCP server's tool provably made.
pub const MCP_APP_STATE_KEY: &str = "adk.mcp.app";

/// Who may call a tool with an app: the model, the app, or both (`_meta.ui.visibility`).
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::McpUiVisibility;
///
/// assert_eq!(McpUiVisibility::default(), McpUiVisibility { model: true, app: true });
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpUiVisibility {
    /// The model may call it.
    pub model: bool,
    /// The app may call it.
    pub app: bool,
}

impl Default for McpUiVisibility {
    /// Both, as MCP Apps reads a tool that declares no visibility.
    fn default() -> Self {
        Self { model: true, app: true }
    }
}

/// A tool's `_meta.ui` (spec 3.3 §6.1 `genui-apps`): the `ui://` resource it shows, and who
/// may call it.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::{McpUiBinding, McpUiVisibility};
/// use rmcp::model::{JsonObject, MetaObject, Tool};
/// use std::sync::Arc;
///
/// let mut tool = Tool::new("weather_show", "Shows the forecast", Arc::new(JsonObject::new()));
/// tool.meta = Some(MetaObject(
///     serde_json::from_value(serde_json::json!({
///         "ui": { "resourceUri": "ui://weather/app", "visibility": ["model"] }
///     }))
///     .unwrap(),
/// ));
/// let binding = McpUiBinding::of_tool(&tool).unwrap();
/// assert_eq!(binding.resource_uri, "ui://weather/app");
/// assert_eq!(binding.visibility, McpUiVisibility { model: true, app: false });
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiBinding {
    /// The tool's name on its server.
    pub tool: String,
    /// The `ui://` resource it shows.
    pub resource_uri: String,
    /// Who may call it.
    pub visibility: McpUiVisibility,
}

impl McpUiBinding {
    /// The binding `tool` declares in `_meta.ui`, or `None` for a tool without an app or with a
    /// `resourceUri` that is not a `ui://` URI.
    pub fn of_tool(tool: &Tool) -> Option<Self> {
        let ui = tool.meta.as_ref()?.0.get("ui")?;
        let resource_uri = ui.get("resourceUri")?.as_str()?;
        if !resource_uri.starts_with("ui://") {
            return None;
        }
        let visibility = match ui.get("visibility").and_then(Value::as_array) {
            Some(listed) => {
                let has = |name: &str| listed.iter().any(|entry| entry.as_str() == Some(name));
                McpUiVisibility { model: has("model"), app: has("app") }
            }
            None => McpUiVisibility::default(),
        };
        Some(Self {
            tool: tool.name.to_string(),
            resource_uri: resource_uri.to_string(),
            visibility,
        })
    }
}

/// The sites an app's resource declares in `_meta.ui.csp`, as the server sent them: a host
/// builds the app's content security policy from them, after its own checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiCsp {
    /// Origins the app may fetch from or open sockets to.
    #[serde(default)]
    pub connect_domains: Vec<String>,
    /// Origins the app may load scripts, styles, images, fonts and media from.
    #[serde(default)]
    pub resource_domains: Vec<String>,
    /// Origins the app may frame.
    #[serde(default)]
    pub frame_domains: Vec<String>,
    /// Origins the app's `<base>` may name.
    #[serde(default)]
    pub base_uri_domains: Vec<String>,
}

/// A `ui://` resource read back (spec 3.3 §6.1 `genui-apps`): the app's HTML and what its
/// `_meta.ui` declares.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::{MCP_APP_MIME_TYPE, McpAppResource};
/// use rmcp::model::ResourceContents;
///
/// let contents = vec![
///     ResourceContents::text("<p>Nairobi 24°</p>", "ui://weather/app").with_mime_type(MCP_APP_MIME_TYPE),
/// ];
/// let app = McpAppResource::from_contents("ui://weather/app", &contents).unwrap();
/// assert_eq!(app.html, "<p>Nairobi 24°</p>");
/// assert!(app.csp.connect_domains.is_empty());
///
/// let plain = vec![ResourceContents::text("hi", "ui://weather/app")];
/// assert!(McpAppResource::from_contents("ui://weather/app", &plain).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppResource {
    /// The resource's URI.
    pub uri: String,
    /// Its MIME type, [`MCP_APP_MIME_TYPE`].
    pub mime_type: String,
    /// The app's HTML.
    pub html: String,
    /// The sites it declares.
    pub csp: McpUiCsp,
    /// The device permissions it asks for (`camera`, `microphone`, `geolocation`, ...), by name.
    pub permissions: Vec<String>,
    /// Whether it asks the host to draw a border around it.
    pub prefers_border: Option<bool>,
}

impl McpAppResource {
    /// The app `contents` hold for `uri`: the first text content whose MIME type is
    /// [`MCP_APP_MIME_TYPE`].
    ///
    /// # Errors
    ///
    /// Returns a tool error when no content of `uri` is an MCP App's HTML, or when its
    /// `_meta.ui.csp` is not a list of sites per directive.
    pub fn from_contents(uri: &str, contents: &[ResourceContents]) -> Result<Self> {
        let found = contents.iter().find_map(|content| match content {
            ResourceContents::TextResourceContents { uri: at, mime_type, text, meta }
                if at == uri && mime_type.as_deref().is_some_and(is_app_mime) =>
            {
                Some((mime_type.clone().unwrap_or_default(), text, meta))
            }
            _ => None,
        });
        let Some((mime_type, html, meta)) = found else {
            let found: Vec<String> = contents
                .iter()
                .map(|content| match content {
                    ResourceContents::TextResourceContents { mime_type, .. }
                    | ResourceContents::BlobResourceContents { mime_type, .. } => {
                        mime_type.clone().unwrap_or_else(|| "no MIME type".to_string())
                    }
                    _ => "unknown content".to_string(),
                })
                .collect();
            return Err(AdkError::tool(format!(
                "resource '{uri}' is not an MCP App: expected text with MIME type \
                 {MCP_APP_MIME_TYPE}, found {}",
                if found.is_empty() { "nothing".to_string() } else { found.join(", ") }
            )));
        };
        let ui = meta.as_ref().and_then(|meta| meta.0.get("ui"));
        let csp = match ui.and_then(|ui| ui.get("csp")) {
            Some(csp) => serde_json::from_value(csp.clone()).map_err(|error| {
                AdkError::tool(format!(
                    "resource '{uri}' declares a _meta.ui.csp that is not lists of sites: {error}"
                ))
            })?,
            None => McpUiCsp::default(),
        };
        let mut permissions: Vec<String> = ui
            .and_then(|ui| ui.get("permissions"))
            .and_then(Value::as_object)
            .map(|asked| asked.keys().cloned().collect())
            .unwrap_or_default();
        permissions.sort();
        Ok(Self {
            uri: uri.to_string(),
            mime_type,
            html: html.clone(),
            csp,
            permissions,
            prefers_border: ui.and_then(|ui| ui.get("prefersBorder")).and_then(Value::as_bool),
        })
    }
}

/// Whether `mime` is [`MCP_APP_MIME_TYPE`], ignoring case and spaces around `;`.
fn is_app_mime(mime: &str) -> bool {
    let normalized: String = mime.chars().filter(|c| !c.is_whitespace()).collect();
    normalized.eq_ignore_ascii_case(MCP_APP_MIME_TYPE)
}

/// What a tool with an app writes under [`MCP_APP_STATE_KEY`] when its call succeeds
/// (spec 3.3 §6.1 `genui-apps`): the server, the tool, the resource, and the call.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::McpAppRecord;
///
/// let record = McpAppRecord {
///     server: "weather".into(),
///     tool: "weather_show".into(),
///     resource_uri: "ui://weather/app".into(),
///     call_id: "call-1".into(),
/// };
/// assert_eq!(serde_json::to_value(&record).unwrap()["resourceUri"], "ui://weather/app");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppRecord {
    /// The MCP server: its toolset's name.
    pub server: String,
    /// The tool's name on that server.
    pub tool: String,
    /// The `ui://` resource it shows.
    pub resource_uri: String,
    /// The call's ID.
    pub call_id: String,
}
