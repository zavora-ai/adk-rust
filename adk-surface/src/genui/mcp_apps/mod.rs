//! The MCP Apps host (spec 3.3 §6.1 `genui-apps`, wave 4 decisions 15–17): an MCP server's
//! `_meta.ui` app shown in a thread, sandboxed on its own origin, which can never approve
//! anything.
//!
//! | Step | What the host does |
//! |---|---|
//! | Projection | A tool call whose event carries the MCP tool's own `adk.mcp.app` record becomes [`ThreadItem::App`](crate::ThreadItem::App) |
//! | Open | Reads the `ui://` resource from the server that published the tool, checks its MIME type, checks every declared site (nothing on this computer, its network or the home), scans the HTML for a decision's words and undeclared sites, and frames it on the apps origin under a CSP built from what passed |
//! | `tools/call` | Only a tool the same server publishes for both the model and the app, asked for as a requested call held by the agent whose call showed the app; an app-only tool is "This app needs a tool this home can't run yet" |
//! | `ui/message` | "The app wants to send: …" with **Send it** and **Not now**; sent as the person's message inside an untrusted-content block labelled "Suggested by the Weather app" |
//! | `ui/open-link` | "Open example.com?", opened only by the person's press |
//! | `ui/update-model-context` | Logged, never forwarded to the model |
//! | Anything else | "not supported here" |
//!
//! The apps origin is a second loopback listener ([`McpAppsHost::sandbox_router`]) or a remote
//! home's `--apps-origin`; the surface API refuses every request from it
//! ([`refuse_apps_origin`](crate::web::genui::refuse_apps_origin)). Without an apps origin, on
//! the phone and in the terminal, an app is its text result and "Open the home on this
//! computer to see this app".

mod scan;
mod sites;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::{Event, Part, RequestedCall, RequestedVia, RequestedViaKind};
use adk_tool::mcp::{
    MCP_APP_STATE_KEY, MCP_APPS_VERSION, McpAppRecord, McpAppResource, McpServerManager,
    McpUiBinding, ResourceContents,
};
use async_trait::async_trait;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};

use super::app_view::app_name;
use super::{AppAnswer, AppBlockReason, AppError, AppOpen, AppPrompt, AppRequest, AppView};
use crate::engine::{Command, CommandOutcome, SurfaceEngine};
use crate::error::{Result, SurfaceError};
use crate::model::ThreadItem;
use crate::origin::{Caller, SurfaceKind};
use crate::project::Projector;
use crate::words;

/// Audit record: an app was not framed, with its reason.
pub const SURFACE_AUDIT_APP_BLOCKED: &str = "surface.app.blocked";
/// Audit record: an app asked for a tool it may not use.
pub const SURFACE_AUDIT_APP_CALL_REFUSED: &str = "surface.app.call_refused";
/// Audit record: an app's tool call was held for the person's decision.
pub const SURFACE_AUDIT_APP_CALL_HELD: &str = "surface.app.call_held";
/// Audit record: the person sent a message an app suggested.
pub const SURFACE_AUDIT_APP_MESSAGE_SENT: &str = "surface.app.message_sent";

/// The tallest an app's frame grows, in CSS pixels.
const MAX_HEIGHT: u32 = 640;
/// How long a framed app's page stays available on the apps origin.
const FRAME_LIFETIME: Duration = Duration::from_secs(15 * 60);
/// The longest message an app may suggest, in characters.
const MAX_MESSAGE_CHARS: usize = 4_000;
/// JSON-RPC: the method is not offered here.
const METHOD_NOT_FOUND: i64 = -32_601;
/// JSON-RPC: the parameters are not what the method takes.
const INVALID_PARAMS: i64 = -32_602;
/// JSON-RPC: the home refused the request.
const REFUSED: i64 = -32_000;

/// The MCP servers whose apps a home shows (spec 3.3 §6.1 `genui-apps`): each server's tool
/// bindings and its `ui://` resources. [`McpServerManager`] is one.
#[async_trait]
pub trait AppServers: Send + Sync {
    /// Every `_meta.ui` binding `server` publishes, the app-only tools included; `None` when
    /// this set does not run `server`.
    async fn ui_bindings(&self, server: &str) -> Option<adk_core::Result<Vec<McpUiBinding>>>;

    /// The contents of `server`'s resource `uri`.
    async fn read_resource(
        &self,
        server: &str,
        uri: &str,
    ) -> adk_core::Result<Vec<ResourceContents>>;
}

#[async_trait]
impl AppServers for McpServerManager {
    async fn ui_bindings(&self, server: &str) -> Option<adk_core::Result<Vec<McpUiBinding>>> {
        self.server_config(server).await.ok()?;
        Some(self.server_ui_bindings(server).await)
    }

    async fn read_resource(
        &self,
        server: &str,
        uri: &str,
    ) -> adk_core::Result<Vec<ResourceContents>> {
        self.read_server_resource(server, uri).await
    }
}

/// An app's page on the apps origin.
struct Frame {
    html: String,
    policy: String,
    until: Instant,
}

/// The MCP Apps host (spec 3.3 §6.1 `genui-apps`). Mount its routes with
/// [`web::genui::apps_router`](crate::web::genui::apps_router), serve
/// [`sandbox_router`](Self::sandbox_router) on the apps origin, and wrap the home's API in
/// [`refuse_apps_origin`](crate::web::genui::refuse_apps_origin).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::genui::McpAppsHost;
///
/// # fn example(manager: Arc<adk_tool::mcp::McpServerManager>) {
/// let host = McpAppsHost::builder()
///     .servers(manager)
///     .apps_origin("http://127.0.0.1:41234")
///     .home_origin("http://127.0.0.1:8080")
///     .build();
/// assert_eq!(host.apps_origin(), Some("http://127.0.0.1:41234"));
/// # }
/// ```
#[derive(Clone)]
pub struct McpAppsHost {
    inner: Arc<Inner>,
}

struct Inner {
    servers: Vec<Arc<dyn AppServers>>,
    apps_origin: Option<String>,
    home_origins: Vec<String>,
    frames: Mutex<HashMap<String, Frame>>,
}

impl std::fmt::Debug for McpAppsHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpAppsHost")
            .field("servers", &self.inner.servers.len())
            .field("apps_origin", &self.inner.apps_origin)
            .field("home_origins", &self.inner.home_origins)
            .finish()
    }
}

/// Builds an [`McpAppsHost`].
#[derive(Default)]
pub struct McpAppsHostBuilder {
    servers: Vec<Arc<dyn AppServers>>,
    apps_origin: Option<String>,
    home_origins: Vec<String>,
}

impl McpAppsHostBuilder {
    /// Shows the apps of the servers `servers` runs; may be called for several sets, which are
    /// asked in order.
    #[must_use]
    pub fn servers(mut self, servers: Arc<dyn AppServers>) -> Self {
        self.servers.push(servers);
        self
    }

    /// Frames apps on `origin`: `http://127.0.0.1:<port>` of the second loopback listener, or
    /// a remote home's `--apps-origin`. Without one, apps are their text result.
    #[must_use]
    pub fn apps_origin(mut self, origin: impl Into<String>) -> Self {
        self.apps_origin = Some(origin.into().trim_end_matches('/').to_string());
        self
    }

    /// An origin the home is served on: the only one that may frame apps, and one no app may
    /// declare.
    #[must_use]
    pub fn home_origin(mut self, origin: impl Into<String>) -> Self {
        self.home_origins.push(origin.into().trim_end_matches('/').to_string());
        self
    }

    /// The host.
    pub fn build(self) -> McpAppsHost {
        McpAppsHost {
            inner: Arc::new(Inner {
                servers: self.servers,
                apps_origin: self.apps_origin,
                home_origins: self.home_origins,
                frames: Mutex::new(HashMap::new()),
            }),
        }
    }
}

/// The app a session's tool call showed, as its events record it.
pub(crate) struct ShownApp {
    record: McpAppRecord,
    /// The tool's name as the agent called it, prefixed when two servers share it.
    called_as: String,
    args: Value,
    response: Value,
}

impl McpAppsHost {
    /// A builder.
    pub fn builder() -> McpAppsHostBuilder {
        McpAppsHostBuilder::default()
    }

    /// The origin apps are framed on, when there is one.
    pub fn apps_origin(&self) -> Option<&str> {
        self.inner.apps_origin.as_deref()
    }

    /// The routes of the apps origin (spec 3.3 §6.1 `genui-apps`): `GET /app/{token}` serves a
    /// checked app's page under its content security policy, sandboxed, framed only by the
    /// home; nothing else is served. Serve it on its own listener, never beside the home.
    pub fn sandbox_router(&self) -> Router {
        Router::new().route("/app/{token}", get(frame)).with_state(self.clone())
    }

    /// The app `call_id` showed in `session`, from its events.
    fn shown(events: &[Event], call_id: &str) -> Option<ShownApp> {
        let mut args = Value::Null;
        for event in events {
            let Some(content) = &event.llm_response.content else {
                continue;
            };
            for part in &content.parts {
                match part {
                    Part::FunctionCall { id: Some(id), args: called, .. } if id == call_id => {
                        args = called.clone();
                    }
                    Part::FunctionResponse { function_response, id: Some(id), .. }
                        if id == call_id =>
                    {
                        let record = record_of(event, call_id)?;
                        return Some(ShownApp {
                            record,
                            called_as: function_response.name.clone(),
                            args,
                            response: function_response.response.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }
        None
    }

    async fn app(
        &self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        call_id: &str,
    ) -> Result<ShownApp> {
        let events = engine.app_transcript(caller, session).await?;
        Self::shown(&events, call_id).ok_or_else(|| {
            SurfaceError::not_found(format!("app '{call_id}' in session '{session}'"))
        })
    }

    /// The bindings `server` publishes, from the first server set that runs it.
    async fn bindings(&self, server: &str) -> adk_core::Result<Vec<McpUiBinding>> {
        for servers in &self.inner.servers {
            if let Some(bindings) = servers.ui_bindings(server).await {
                return bindings;
            }
        }
        Err(adk_core::AdkError::tool(format!("no MCP server named '{server}' runs on this home")))
    }

    async fn resource(&self, server: &str, uri: &str) -> adk_core::Result<Vec<ResourceContents>> {
        for servers in &self.inner.servers {
            if servers.ui_bindings(server).await.is_some() {
                return servers.read_resource(server, uri).await;
            }
        }
        Err(adk_core::AdkError::tool(format!("no MCP server named '{server}' runs on this home")))
    }

    /// Origins no app may declare or load from: the home's and the apps origin.
    fn refused_origins(&self) -> Vec<String> {
        self.inner.home_origins.iter().chain(&self.inner.apps_origin).cloned().collect()
    }

    /// Opens the app `call_id` showed in `session` for `caller` (spec 3.3 §6.1 `genui-apps`):
    /// framed when every check passes, blocked with its reason, or not shown on this surface.
    ///
    /// # Errors
    ///
    /// `surface.not_found` when the session is not the caller's or holds no such app (S-I8).
    pub(crate) async fn open(
        &self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        call_id: &str,
    ) -> Result<AppOpen> {
        let app = self.app(engine, caller, session, call_id).await?;
        let result_text = result_text(&app.response);
        let elsewhere = |text: &str| AppOpen::Elsewhere {
            text: text.to_string(),
            result_text: result_text.clone(),
        };
        // Phones never render apps: the apps origin is a loopback listener (decision 17).
        let Some(origin) =
            self.inner.apps_origin.clone().filter(|_| caller.surface == SurfaceKind::Web)
        else {
            return Ok(elsewhere(words::OPEN_THE_HOME_TO_SEE_THIS_APP));
        };
        let record = &app.record;
        let published = match self.bindings(&record.server).await {
            Ok(bindings) => bindings,
            Err(error) => {
                tracing::warn!(app.server = %record.server, error = %error, "an app's server did not list its tools");
                return Ok(elsewhere(words::APP_CANT_BE_OPENED));
            }
        };
        if !published.iter().any(|binding| {
            binding.tool == record.tool && binding.resource_uri == record.resource_uri
        }) {
            return Ok(elsewhere(words::APP_CANT_BE_OPENED));
        }
        let contents = match self.resource(&record.server, &record.resource_uri).await {
            Ok(contents) => contents,
            Err(error) => {
                tracing::warn!(app.server = %record.server, error = %error, "an app's resource could not be read");
                return Ok(elsewhere(words::APP_CANT_BE_OPENED));
            }
        };
        let checked = McpAppResource::from_contents(&record.resource_uri, &contents)
            .map_err(|error| (AppBlockReason::NotAnApp, error.message.clone()))
            .and_then(|resource| {
                let declared = sites::checked(&resource.csp, &self.refused_origins())?;
                let refused_hosts: Vec<String> = self
                    .refused_origins()
                    .iter()
                    .filter_map(|origin| origin.split_once("://").map(|(_, rest)| rest))
                    .map(|rest| rest.rsplit_once(':').map_or(rest, |(host, _)| host).to_string())
                    .collect();
                match scan::scan(&resource.html, &declared, &refused_hosts) {
                    Some(refusal) => Err(refusal),
                    None => Ok((resource, declared)),
                }
            });
        let (resource, declared) = match checked {
            Ok(passed) => passed,
            Err((reason, detail)) => {
                let text = match reason {
                    AppBlockReason::ApprovalButtons => words::APP_BLOCKED_APPROVAL_BUTTONS,
                    AppBlockReason::UndeclaredSites => words::APP_BLOCKED_UNDECLARED_SITES,
                    AppBlockReason::ThisComputer => words::APP_BLOCKED_THIS_COMPUTER,
                    AppBlockReason::NotAnApp => words::APP_BLOCKED_NOT_AN_APP,
                };
                engine
                    .record(
                        caller,
                        SURFACE_AUDIT_APP_BLOCKED,
                        json!({
                            "sessionId": session,
                            "callId": call_id,
                            "server": record.server,
                            "tool": record.tool,
                            "resourceUri": record.resource_uri,
                            "reason": reason,
                            "detail": detail,
                        }),
                    )
                    .await?;
                return Ok(AppOpen::Blocked { reason, text: text.to_string(), detail });
            }
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let frame = Frame {
            html: resource.html.clone(),
            policy: declared.policy(&self.inner.home_origins),
            until: Instant::now() + FRAME_LIFETIME,
        };
        {
            let mut frames =
                self.inner.frames.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            frames.retain(|_, frame| frame.until > Instant::now());
            frames.insert(token.clone(), frame);
        }
        Ok(AppOpen::Shown {
            frame_url: format!("{origin}/app/{token}"),
            frame_origin: origin,
            max_height: MAX_HEIGHT,
            prefers_border: resource.prefers_border.unwrap_or(true),
            tool_input: app.args.clone(),
            tool_result: call_tool_result(&app.response),
            refused_permissions: resource.permissions,
        })
    }

    /// Answers a request the app `call_id` in `session` made (spec 3.3 §6.1 `genui-apps`).
    ///
    /// # Errors
    ///
    /// `surface.not_found` when the session is not the caller's or holds no such app (S-I8);
    /// the source's error when a held call could not be asked for.
    pub(crate) async fn request(
        &self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        call_id: &str,
        request: AppRequest,
    ) -> Result<AppAnswer> {
        let app = self.app(engine, caller, session, call_id).await?;
        let label = app_name(&app.record.server);
        let ok = |result: Value| AppAnswer { result: Some(result), error: None, prompt: None };
        let error = |code: i64, message: String, prompt: Option<AppPrompt>| AppAnswer {
            result: None,
            error: Some(AppError { code, message }),
            prompt,
        };
        match request.method.as_str() {
            "ui/initialize" => Ok(ok(json!({
                "protocolVersion": MCP_APPS_VERSION,
                "hostInfo": { "name": "adk-rust", "version": env!("CARGO_PKG_VERSION") },
                "hostCapabilities": { "openLinks": {}, "serverTools": {}, "logging": {} },
                "hostContext": {
                    "displayMode": "inline",
                    "availableDisplayModes": ["inline"],
                    "platform": "web",
                },
            }))),
            "ping" => Ok(ok(json!({}))),
            "tools/call" => {
                let Some(tool) = request.params.get("name").and_then(Value::as_str) else {
                    return Ok(error(INVALID_PARAMS, "tools/call needs a tool name".into(), None));
                };
                let args = request.params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let bindings = self.bindings(&app.record.server).await.unwrap_or_default();
                let binding = bindings.iter().find(|binding| binding.tool == tool);
                let refusal = match binding {
                    Some(binding) if binding.visibility.app && binding.visibility.model => None,
                    Some(binding) if binding.visibility.app => {
                        Some(words::APP_NEEDS_A_TOOL.to_string())
                    }
                    // A model-only tool, or one the app's own server does not publish.
                    _ => Some(words::app_cant_use(tool)),
                };
                if let Some(text) = refusal {
                    engine
                        .record(
                            caller,
                            SURFACE_AUDIT_APP_CALL_REFUSED,
                            json!({
                                "sessionId": session,
                                "callId": call_id,
                                "server": app.record.server,
                                "tool": tool,
                                "via": "app",
                            }),
                        )
                        .await?;
                    return Ok(error(REFUSED, text.clone(), Some(AppPrompt::Notice { text })));
                }
                // The agent knows the tool by the name it called the app's own tool by: with
                // the server's prefix when another server publishes the same name.
                let tool_name = if app.called_as == app.record.tool {
                    tool.to_string()
                } else {
                    format!("{}__{tool}", app.record.server)
                };
                let call = RequestedCall::new(
                    tool_name,
                    args,
                    caller.subject.clone(),
                    RequestedVia {
                        kind: RequestedViaKind::App,
                        surface_id: call_id.to_string(),
                        version: 1,
                        action_id: tool.to_string(),
                    },
                );
                engine
                    .record(
                        caller,
                        SURFACE_AUDIT_APP_CALL_HELD,
                        json!({
                            "sessionId": session,
                            "callId": call_id,
                            "server": app.record.server,
                            "tool": tool,
                            "requestedCallId": call.function_call_id,
                            "fingerprint": adk_core::ToolConfirmationRequest::new(
                                call.tool_name.clone(),
                                call.args.clone(),
                            )
                            .fingerprint(),
                            "via": "app",
                        }),
                    )
                    .await?;
                engine.ask_for_app_call(caller, session, call, &label).await?;
                Ok(ok(json!({
                    "content": [{ "type": "text", "text": words::APP_CALL_WAITS_FOR_YOU }],
                    "isError": false,
                })))
            }
            "ui/message" => {
                let message = message_text(&request.params);
                if message.trim().is_empty() {
                    return Ok(error(INVALID_PARAMS, "ui/message needs text content".into(), None));
                }
                let message: String = message.chars().take(MAX_MESSAGE_CHARS).collect();
                Ok(AppAnswer {
                    result: Some(json!({})),
                    error: None,
                    prompt: Some(AppPrompt::Message {
                        text: words::app_wants_to_send(&message),
                        message,
                        send_label: words::SEND_IT.to_string(),
                        dismiss_label: words::NOT_NOW.to_string(),
                        note: words::MARKED_AS_SUGGESTED_BY_THE_APP.to_string(),
                    }),
                })
            }
            "ui/open-link" => {
                let url = request.params.get("url").and_then(Value::as_str).unwrap_or_default();
                let host = sites::parse(url, true)
                    .filter(|_| url.starts_with("https://") || url.starts_with("http://"))
                    .map(|_| link_host(url));
                match host {
                    Some(host) => Ok(AppAnswer {
                        result: Some(json!({})),
                        error: None,
                        prompt: Some(AppPrompt::Link {
                            text: words::open_site(&host),
                            url: url.to_string(),
                            open_label: words::OPEN.to_string(),
                            dismiss_label: words::CANCEL.to_string(),
                        }),
                    }),
                    None => Ok(error(
                        INVALID_PARAMS,
                        "ui/open-link opens http and https links only".into(),
                        None,
                    )),
                }
            }
            "ui/update-model-context" => {
                // Logged, never forwarded: an app does not steer the model (decision 15).
                tracing::info!(
                    app.server = %app.record.server,
                    app.call_id = %call_id,
                    context.bytes = request.params.to_string().len(),
                    "an app's model context update was logged and not forwarded"
                );
                Ok(ok(json!({})))
            }
            other => Ok(error(METHOD_NOT_FOUND, format!("{other} is not supported here"), None)),
        }
    }

    /// Sends `message`, which the app `call_id` suggested, as `caller`'s message in `session`,
    /// inside an untrusted-content block labelled as the app's suggestion (spec 3.3 §6.1
    /// `genui-apps`). It never answers a decision: it is the session's next message.
    ///
    /// # Errors
    ///
    /// `surface.not_found` when the session is not the caller's or holds no such app;
    /// `surface.invalid` for an empty or overlong message; the engine's error for the send.
    pub(crate) async fn send_message(
        &self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        call_id: &str,
        message: &str,
    ) -> Result<CommandOutcome> {
        let app = self.app(engine, caller, session, call_id).await?;
        if message.trim().is_empty() || message.chars().count() > MAX_MESSAGE_CHARS {
            return Err(SurfaceError::Invalid(format!(
                "a suggested message has 1 to {MAX_MESSAGE_CHARS} characters"
            )));
        }
        let label = app_name(&app.record.server);
        let suggested = words::suggested_by_the_app(&label);
        let text = adk_harness::web::untrusted_block(
            "mcp-app",
            &[("app", app.record.server.as_str()), ("label", suggested.as_str())],
            message,
        );
        engine
            .record(
                caller,
                SURFACE_AUDIT_APP_MESSAGE_SENT,
                json!({ "sessionId": session, "callId": call_id, "server": app.record.server }),
            )
            .await?;
        engine.execute(caller, Command::Send { session_id: session.to_string(), text }).await
    }
}

/// `GET /app/{token}` on the apps origin.
async fn frame(State(host): State<McpAppsHost>, Path(token): Path<String>) -> Response {
    let page = {
        let frames = host.inner.frames.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        frames
            .get(&token)
            .filter(|frame| frame.until > Instant::now())
            .map(|frame| (frame.html.clone(), frame.policy.clone()))
    };
    let Some((html, policy)) = page else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = html.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    if let Ok(policy) = HeaderValue::from_str(&policy) {
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
    }
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), display-capture=()"),
    );
    headers.insert("cross-origin-resource-policy", HeaderValue::from_static("cross-origin"));
    response
}

/// The app record `event` carries for `call_id`.
fn record_of(event: &Event, call_id: &str) -> Option<McpAppRecord> {
    let record = event.actions.state_delta.get(MCP_APP_STATE_KEY)?;
    let record: McpAppRecord = serde_json::from_value(record.clone()).ok()?;
    (record.call_id == call_id).then_some(record)
}

/// The text of an MCP tool's result as `McpToolset` records it: `{"output": ...}`.
fn result_text(response: &Value) -> String {
    let output = response.get("response").unwrap_or(response);
    if let Some(text) = output.get("text").and_then(Value::as_array) {
        return text.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n");
    }
    match output.get("output") {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// The MCP `CallToolResult` an app's `ui/notifications/tool-result` carries.
fn call_tool_result(response: &Value) -> Value {
    let output = response.get("response").unwrap_or(response);
    let mut result = json!({
        "content": [{ "type": "text", "text": result_text(response) }],
        "isError": false,
    });
    if let Some(structured) = output.get("output").filter(|output| output.is_object()) {
        result["structuredContent"] = structured.clone();
    }
    result
}

/// The text of a `ui/message`: its content's text blocks.
fn message_text(params: &Value) -> String {
    let content = params.get("content").unwrap_or(params);
    let blocks: Vec<&Value> = match content {
        Value::Array(blocks) => blocks.iter().collect(),
        other => vec![other],
    };
    blocks
        .iter()
        .filter_map(|block| match block {
            Value::String(text) => Some(text.as_str()),
            block => block.get("text").and_then(Value::as_str),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The host a link opens, without `www.`, as "Open example.com?" names it.
fn link_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    authority.trim_start_matches("www.").to_string()
}

/// The thread item of `event`'s response to `call_id` when an MCP server's tool with an app
/// made it (spec 3.3 §6.1 `genui-apps`): the event carries the tool's own record for that
/// call. A same-named tool without it is an ordinary result.
pub(crate) fn project(
    projector: &Projector,
    event: &Event,
    response: &Value,
    call_id: Option<&String>,
) -> Option<ThreadItem> {
    let record = record_of(event, call_id?)?;
    let label = app_name(&record.server);
    Some(ThreadItem::App {
        app: Box::new(AppView {
            call_id: record.call_id.clone(),
            server: record.server.clone(),
            app_name: label.clone(),
            tool: record.tool.clone(),
            agent: projector.agent(&event.author),
            at: event.timestamp,
            header_text: words::app_from(&label),
            result_text: result_text(response),
            elsewhere_text: words::OPEN_THE_HOME_TO_SEE_THIS_APP.to_string(),
            terminal_text: words::app_in_the_terminal(&label),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suggested_message_reads_its_text_blocks() {
        assert_eq!(
            message_text(
                &json!({ "role": "user", "content": [{ "type": "text", "text": "Approved" }] })
            ),
            "Approved"
        );
        assert_eq!(message_text(&json!({ "content": { "type": "text", "text": "hi" } })), "hi");
    }

    #[test]
    fn a_tools_text_result_is_what_every_surface_shows() {
        assert_eq!(result_text(&json!({ "output": "Nairobi: 24°" })), "Nairobi: 24°");
        assert_eq!(
            call_tool_result(&json!({ "output": { "temp": 24 }, "text": ["24°"] })),
            json!({
                "content": [{ "type": "text", "text": "24°" }],
                "isError": false,
                "structuredContent": { "temp": 24 },
            })
        );
    }
}
