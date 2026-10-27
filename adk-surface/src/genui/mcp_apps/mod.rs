//! The MCP Apps host (spec 3.3 §6.1 `genui-apps`, wave 4 decisions 15–17): an MCP server's
//! `_meta.ui` app shown in a thread, sandboxed on its own origin, which can never approve
//! anything.
//!
//! | Step | What the host does |
//! |---|---|
//! | Projection | A tool call whose event carries the MCP tool's own `adk.mcp.app` record becomes [`ThreadItem::App`](crate::ThreadItem::App) |
//! | Open | Reads the `ui://` resource from the server that published the tool, checks its MIME type, checks every declared site (nothing on this computer, its network, the home or the page's own origin, nor a wildcard over them), scans the HTML for a decision's words and undeclared sites, and frames it on the apps origin under a CSP built from what passed, which only the home's page may frame |
//! | `tools/call` | Only a tool the same server publishes for both the model and the app, a tool without `_meta.ui` included, by the name the agent knows that tool by, asked for as a requested call held by the agent whose call showed the app and answered once the agent has held or refused it; an app-only tool is "This app needs a tool this home can't run yet"; the same call again is the same decision, and an app with three calls waiting asks for no more |
//! | `ui/message` | "The app wants to send: …" with **Send it** and **Not now**; sent as the person's message inside an untrusted-content block labelled "Suggested by the Weather app", which every surface shows as the message and that label |
//! | `ui/open-link` | "Open example.com?", opened only by the person's press; a link to this computer or its network is refused |
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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::{Event, Part, RequestedCall, RequestedVia, RequestedViaKind};
use adk_tool::mcp::{
    MCP_APP_STATE_KEY, MCP_APPS_VERSION, McpAppRecord, McpAppResource, McpServerManager,
    McpUiBinding, McpUiVisibility, ResourceContents,
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
use crate::model::{DecisionCard, ThreadItem};
use crate::origin::{Caller, SurfaceKind};
use crate::project::Projector;
use crate::words;

/// Audit record: an app was not framed, with its reason.
pub const SURFACE_AUDIT_APP_BLOCKED: &str = "surface.app.blocked";
/// Audit record: an app's tool call was refused, by the home or by the agent.
pub const SURFACE_AUDIT_APP_CALL_REFUSED: &str = "surface.app.call_refused";
/// Audit record: an app's tool call was asked of the agent whose call showed the app.
pub const SURFACE_AUDIT_APP_CALL_ASKED: &str = "surface.app.call_asked";
/// Audit record: the agent held an app's tool call for the person's decision.
pub const SURFACE_AUDIT_APP_CALL_HELD: &str = "surface.app.call_held";
/// Audit record: requests from the apps origin, or an app's opaque frame, were refused
/// (at most one record a minute, with the count).
pub const SURFACE_AUDIT_APP_ORIGIN_REFUSED: &str = "surface.app.origin_refused";
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
/// The most calls one app may have waiting for the person at once.
const MAX_WAITING: usize = 3;
/// How long an app's tool call waits for the agent to hold or refuse it before the app is
/// answered anyway.
const OUTCOME_WAIT: Duration = Duration::from_secs(10);
/// How often the thread is read for that outcome.
const OUTCOME_POLL: Duration = Duration::from_millis(50);

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

    /// Who may call each tool `server` publishes, by its name on the server: its
    /// `_meta.ui.visibility`, or the model and the app when it declares none; `None` when this
    /// set does not run `server`. The default reads [`ui_bindings`](Self::ui_bindings), which
    /// lists only the tools with an app.
    async fn tool_visibility(
        &self,
        server: &str,
    ) -> Option<adk_core::Result<Vec<(String, McpUiVisibility)>>> {
        let bindings = self.ui_bindings(server).await?;
        Some(bindings.map(|bindings| {
            bindings.into_iter().map(|binding| (binding.tool, binding.visibility)).collect()
        }))
    }

    /// The name the agent's tools give `server`'s tool `tool`: prefixed with the server when
    /// another server publishes the same name, or `None` when the agent is not given that
    /// tool. The default is `tool` itself, for a set that renames nothing.
    ///
    /// # Errors
    ///
    /// The set's error when it cannot list the agent's tools.
    async fn agent_tool_name(&self, _server: &str, tool: &str) -> adk_core::Result<Option<String>> {
        Ok(Some(tool.to_string()))
    }
}

#[async_trait]
impl AppServers for McpServerManager {
    async fn ui_bindings(&self, server: &str) -> Option<adk_core::Result<Vec<McpUiBinding>>> {
        self.server_config(server).await.ok()?;
        Some(self.server_ui_bindings(server).await)
    }

    async fn tool_visibility(
        &self,
        server: &str,
    ) -> Option<adk_core::Result<Vec<(String, McpUiVisibility)>>> {
        self.server_config(server).await.ok()?;
        Some(self.server_tool_visibility(server).await)
    }

    async fn agent_tool_name(&self, server: &str, tool: &str) -> adk_core::Result<Option<String>> {
        McpServerManager::agent_tool_name(self, server, tool).await
    }

    async fn read_resource(
        &self,
        server: &str,
        uri: &str,
    ) -> adk_core::Result<Vec<ResourceContents>> {
        self.read_server_resource(server, uri).await
    }
}

/// One MCP server's toolset: its name is the server's.
#[async_trait]
impl<S> AppServers for adk_tool::mcp::McpToolset<S>
where
    S: adk_tool::mcp::rmcp::service::Service<adk_tool::mcp::rmcp::RoleClient>
        + Send
        + Sync
        + 'static,
{
    async fn ui_bindings(&self, server: &str) -> Option<adk_core::Result<Vec<McpUiBinding>>> {
        if adk_core::Toolset::name(self) != server {
            return None;
        }
        Some(adk_tool::mcp::McpToolset::ui_bindings(self).await)
    }

    async fn read_resource(
        &self,
        _server: &str,
        uri: &str,
    ) -> adk_core::Result<Vec<ResourceContents>> {
        adk_tool::mcp::McpToolset::read_resource(self, uri).await
    }

    async fn tool_visibility(
        &self,
        server: &str,
    ) -> Option<adk_core::Result<Vec<(String, McpUiVisibility)>>> {
        if adk_core::Toolset::name(self) != server {
            return None;
        }
        Some(adk_tool::mcp::McpToolset::tool_visibility(self).await)
    }

    async fn agent_tool_name(&self, server: &str, tool: &str) -> adk_core::Result<Option<String>> {
        if adk_core::Toolset::name(self) != server {
            return Ok(None);
        }
        let listing: Arc<dyn adk_core::ReadonlyContext> =
            Arc::new(adk_tool::SimpleToolContext::new("mcp-apps"));
        let tools = adk_core::Toolset::tools(self, listing).await?;
        Ok(tools.iter().any(|known| known.name() == tool).then(|| tool.to_string()))
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
                        // A held call's placeholder answer carries no record; the answer of
                        // the call once allowed follows under the same id.
                        let Some(record) = record_of(event, call_id) else {
                            continue;
                        };
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

    /// The origin of the home's page that asked, from the request's `Host` and the apps
    /// origin's scheme: a remote home's own origin, or the address a person opened a home
    /// listening on every interface at.
    fn page_origin(&self, host: Option<&str>) -> Option<String> {
        let (scheme, _) = self.inner.apps_origin.as_deref()?.split_once("://")?;
        let origin = format!("{scheme}://{}", host?.trim().to_ascii_lowercase());
        sites::parse(&origin, false).filter(sites::Parsed::is_plain)?;
        Some(origin)
    }

    /// Who may call `server`'s tool `tool`, and the name the agent knows it by when the model
    /// and the app may both call it, from the first set that runs `server`; `None` when the
    /// server does not publish it.
    async fn app_tool(
        &self,
        server: &str,
        tool: &str,
    ) -> adk_core::Result<Option<(McpUiVisibility, Option<String>)>> {
        for servers in &self.inner.servers {
            let Some(listed) = servers.tool_visibility(server).await else {
                continue;
            };
            let Some((_, visibility)) = listed?.into_iter().find(|(name, _)| name == tool) else {
                return Ok(None);
            };
            let name = if visibility.model && visibility.app {
                servers.agent_tool_name(server, tool).await?
            } else {
                None
            };
            return Ok(Some((visibility, name)));
        }
        Err(adk_core::AdkError::tool(format!("no MCP server named '{server}' runs on this home")))
    }

    /// The open decisions of the calls the app `call_id` asked for in `session`.
    async fn waiting_calls(
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        call_id: &str,
    ) -> Result<Vec<DecisionCard>> {
        let asked: HashSet<String> = engine
            .thread(caller, session)
            .await?
            .into_iter()
            .flat_map(|entry| entry.items)
            .filter_map(|item| match item {
                ThreadItem::DrawnSent { sent } if sent.surface_id == call_id => sent.decision_id,
                _ => None,
            })
            .collect();
        Ok(engine
            .session_decisions(caller, session)
            .await?
            .into_iter()
            .filter(|card| card.decided.is_none() && asked.contains(&card.id))
            .collect())
    }

    /// Whether the agent held the requested call `requested` (`true`) or refused it, with the
    /// thread's words for it, once its run took it; `None` when it has not within
    /// [`OUTCOME_WAIT`].
    async fn outcome(
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
        requested: &str,
    ) -> Result<Option<(bool, String)>> {
        let until = Instant::now() + OUTCOME_WAIT;
        loop {
            let sent = engine
                .thread(caller, session)
                .await?
                .into_iter()
                .flat_map(|entry| entry.items)
                .find_map(|item| match item {
                    ThreadItem::DrawnSent { sent } if sent.call_id == requested => Some(sent),
                    _ => None,
                });
            if let Some(sent) = sent {
                return Ok(Some((sent.held, sent.text)));
            }
            if Instant::now() >= until {
                return Ok(None);
            }
            tokio::time::sleep(OUTCOME_POLL).await;
        }
    }

    /// Opens the app `call_id` showed in `session` for `caller` (spec 3.3 §6.1 `genui-apps`):
    /// framed when every check passes, blocked with its reason, or not shown on this surface.
    /// `page_host` is the request's `Host`: the page that frames the app, which its policy
    /// names beside the home's origins and no app may declare.
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
        page_host: Option<&str>,
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
        let mut home_origins = self.inner.home_origins.clone();
        if let Some(page) = self.page_origin(page_host)
            && !home_origins.contains(&page)
        {
            home_origins.push(page);
        }
        // Origins no app may declare or load from: the home's, the page's and the apps origin.
        let refused: Vec<String> =
            home_origins.iter().chain(&self.inner.apps_origin).cloned().collect();
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
                let declared = sites::checked(&resource.csp, &refused)?;
                let refused_hosts: Vec<String> = refused
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
            policy: declared.policy(&home_origins),
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
                // Only a tool's name, so nothing else the app sends reaches the home's words
                // or the audit trail in its place.
                let Some(tool) = request
                    .params
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| is_tool_name(name))
                else {
                    return Ok(error(
                        INVALID_PARAMS,
                        "tools/call needs the name of one of its server's tools".into(),
                        None,
                    ));
                };
                let args = request.params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let server = app.record.server.as_str();
                let found = match self.app_tool(server, tool).await {
                    Ok(found) => found,
                    Err(failure) => {
                        tracing::warn!(app.server = %server, error = %failure, "an app's server did not list its tools");
                        return Ok(error(REFUSED, words::APP_SERVER_DIDNT_ANSWER.into(), None));
                    }
                };
                let record = |reason: &str, extra: Value| {
                    let mut fields = json!({
                        "sessionId": session,
                        "callId": call_id,
                        "server": server,
                        "tool": tool,
                        "via": "app",
                        "reason": reason,
                    });
                    if let (Some(fields), Value::Object(extra)) = (fields.as_object_mut(), extra) {
                        fields.extend(extra);
                    }
                    fields
                };
                let refused =
                    |text: String| error(REFUSED, text.clone(), Some(AppPrompt::Notice { text }));
                let tool_name = match found {
                    Some((visibility, Some(name))) if visibility.model && visibility.app => name,
                    Some((visibility, _)) if visibility.app && !visibility.model => {
                        engine
                            .record(
                                caller,
                                SURFACE_AUDIT_APP_CALL_REFUSED,
                                record("appOnly", json!({})),
                            )
                            .await?;
                        return Ok(refused(words::APP_NEEDS_A_TOOL.to_string()));
                    }
                    // A model-only tool, one the app's own server does not publish, or one the
                    // agent is not given.
                    _ => {
                        engine
                            .record(
                                caller,
                                SURFACE_AUDIT_APP_CALL_REFUSED,
                                record("notTheApps", json!({})),
                            )
                            .await?;
                        return Ok(refused(words::app_cant_use(tool)));
                    }
                };
                // The same call again, as an app asks on every load, is the decision already
                // waiting; an app with enough waiting asks for no more.
                let waiting = Self::waiting_calls(engine, caller, session, call_id).await?;
                if waiting.iter().any(|card| card.call.tool == tool_name && card.call.args == args)
                {
                    return Ok(ok(sent_text(words::APP_CALL_WAITS_FOR_YOU)));
                }
                if waiting.len() >= MAX_WAITING {
                    engine
                        .record(
                            caller,
                            SURFACE_AUDIT_APP_CALL_REFUSED,
                            record("tooManyWaiting", json!({})),
                        )
                        .await?;
                    return Ok(refused(words::app_asked_enough(MAX_WAITING)));
                }
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
                let requested = call.function_call_id.clone();
                let fingerprint = adk_core::ToolConfirmationRequest::new(
                    call.tool_name.clone(),
                    call.args.clone(),
                )
                .fingerprint();
                let asked = json!({ "requestedCallId": requested, "fingerprint": fingerprint });
                engine
                    .record(caller, SURFACE_AUDIT_APP_CALL_ASKED, record("asked", asked.clone()))
                    .await?;
                if let Err(failure) = engine.ask_for_app_call(caller, session, call, &label).await {
                    let reason =
                        json!({ "requestedCallId": requested, "detail": failure.to_string() });
                    engine
                        .record(caller, SURFACE_AUDIT_APP_CALL_REFUSED, record("notAsked", reason))
                        .await?;
                    return Err(failure);
                }
                // Answered for what the agent did with the call, never before it did.
                match Self::outcome(engine, caller, session, &requested).await? {
                    Some((true, _)) => {
                        engine
                            .record(caller, SURFACE_AUDIT_APP_CALL_HELD, record("held", asked))
                            .await?;
                        Ok(ok(sent_text(words::APP_CALL_WAITS_FOR_YOU)))
                    }
                    Some((false, text)) => {
                        let reason = json!({ "requestedCallId": requested, "detail": text });
                        engine
                            .record(
                                caller,
                                SURFACE_AUDIT_APP_CALL_REFUSED,
                                record("byTheAgent", reason),
                            )
                            .await?;
                        Ok(error(REFUSED, text, None))
                    }
                    None => Ok(ok(sent_text(words::APP_CALL_SENT))),
                }
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
                let parsed = sites::parse(url, true).filter(|parsed| {
                    parsed.is_plain() && (url.starts_with("https://") || url.starts_with("http://"))
                });
                match parsed {
                    Some(parsed) if sites::is_this_computer(parsed.host()) => Ok(error(
                        REFUSED,
                        "ui/open-link opens sites on the web, never this computer or its network"
                            .into(),
                        None,
                    )),
                    Some(_) => Ok(AppAnswer {
                        result: Some(json!({})),
                        error: None,
                        prompt: Some(AppPrompt::Link {
                            text: words::open_site(&link_host(url)),
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
    // Never served without its policy.
    let Ok(policy) = HeaderValue::from_str(&policy) else {
        tracing::warn!(
            "an app's content security policy is not a header value; its page was not served"
        );
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mut response = html.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    headers.insert(header::CONTENT_SECURITY_POLICY, policy);
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

/// Whether `name` can be an MCP tool's name: 1 to 128 letters, digits, `_`, `-`, `.` and `/`.
fn is_tool_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
}

/// The answer to an app's `tools/call` that ran nothing: `text` for the app.
fn sent_text(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
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

    #[tokio::test]
    async fn a_page_whose_policy_cannot_be_sent_is_not_served() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt as _;

        let host = McpAppsHost::builder().apps_origin("http://127.0.0.1:41234").build();
        host.inner.frames.lock().unwrap().insert(
            "token".into(),
            Frame {
                html: "<p>Nairobi</p>".into(),
                policy: "default-src 'none'; connect-src https://[a.b\u{1}]".into(),
                until: Instant::now() + FRAME_LIFETIME,
            },
        );
        let response = host
            .sandbox_router()
            .oneshot(Request::builder().uri("/app/token").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("Nairobi"));
    }

    #[test]
    fn the_pages_own_origin_takes_the_apps_origins_scheme() {
        let remote = McpAppsHost::builder().apps_origin("https://apps.example.net").build();
        assert_eq!(
            remote.page_origin(Some("home.example.net")),
            Some("https://home.example.net".to_string())
        );
        let local = McpAppsHost::builder().apps_origin("http://127.0.0.1:41234").build();
        assert_eq!(
            local.page_origin(Some("127.0.0.1:8080")),
            Some("http://127.0.0.1:8080".to_string())
        );
        for host in [
            None,
            Some(""),
            Some("a.example; frame-ancestors *"),
            Some("a b"),
            Some("*.example.net"),
        ] {
            assert_eq!(local.page_origin(host), None, "{host:?}");
        }
        assert!(is_tool_name("set_rain_alert") && is_tool_name("weather__set-alert.v2"));
        assert!(!is_tool_name("set_alert. You said yes") && !is_tool_name(&"a".repeat(129)));
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
