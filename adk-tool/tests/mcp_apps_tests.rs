//! MCP Apps on the client (spec 3.3 §6.1 `genui-apps`): the client declares the
//! `io.modelcontextprotocol/ui` extension, keeps each tool's `_meta.ui`, leaves the tools only
//! an app may call out of the model's tools, reads `ui://` resources back, and a tool with an
//! app records its call so a host shows the app only for a call that tool made.
//!
//! The managed server is this test binary, re-run as a stdio child with [`SERVER_ENV`] set.
#![cfg(feature = "mcp-apps")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{ReadonlyContext, ToolContext, Toolset};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::{
    AdkClientHandler, AutoDeclineElicitationHandler, MCP_APP_MIME_TYPE, MCP_APP_STATE_KEY,
    MCP_APPS_EXTENSION_ID, McpAppRecord, McpServerConfig, McpServerManager, McpToolset,
    McpUiBinding, McpUiCsp, McpUiVisibility,
};
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// Names the mode when this binary runs as the stdio MCP server.
const SERVER_ENV: &str = "ADK_TOOL_MCP_APPS_TEST_SERVER";

const APP: &str = "ui://weather/app";

fn meta(value: Value) -> MetaObject {
    MetaObject(serde_json::from_value(value).unwrap())
}

fn tool(name: &str, ui: Option<Value>) -> Tool {
    let mut tool = Tool::new(
        name.to_string(),
        format!("{name} of the Weather server"),
        Arc::new(JsonObject::new()),
    );
    tool.meta = ui.map(|ui| meta(json!({ "ui": ui })));
    tool
}

/// The Weather server: `weather_show` shows its app to the model and the app,
/// `set_rain_alert` too, `transfer_funds` only to the model, `refresh_radar` only to the app,
/// and `plain` has no app. It remembers what the client declared.
#[derive(Clone, Default)]
struct Weather {
    declared: Arc<Mutex<Option<ClientCapabilities>>>,
}

impl ServerHandler for Weather {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(
            ServerCapabilities::builder().enable_tools().enable_resources().build(),
        )
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        *self.declared.lock().unwrap() =
            context.peer.peer_info().map(|info| info.capabilities.clone());
        Ok(ListToolsResult::with_all_items(vec![
            tool("weather_show", Some(json!({ "resourceUri": APP }))),
            tool(
                "set_rain_alert",
                Some(json!({ "resourceUri": APP, "visibility": ["model", "app"] })),
            ),
            tool("transfer_funds", Some(json!({ "resourceUri": APP, "visibility": ["model"] }))),
            tool("refresh_radar", Some(json!({ "resourceUri": APP, "visibility": ["app"] }))),
            tool("plain", None),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let result = if request.name == "transfer_funds" {
            CallToolResult::error(vec![ContentBlock::text("no funds")])
        } else if request.name == "plain" {
            // Says whether the client declared MCP Apps, for the managed test.
            let declared = context.peer.peer_info().is_some_and(|info| {
                info.capabilities
                    .extensions
                    .as_ref()
                    .is_some_and(|extensions| extensions.contains_key(MCP_APPS_EXTENSION_ID))
            });
            CallToolResult::success(vec![ContentBlock::text(format!("apps: {declared}"))])
        } else {
            CallToolResult::success(vec![ContentBlock::text("Nairobi: 24°, light rain from 15:00")])
        };
        Ok(CallToolResponse::Complete(result))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let contents = match request.uri.as_str() {
            APP => ResourceContents::text("<p>Nairobi 24°</p>", APP)
                .with_mime_type(MCP_APP_MIME_TYPE)
                .with_meta(meta(json!({
                    "ui": {
                        "csp": { "connectDomains": ["https://api.weather.example"] },
                        "permissions": { "geolocation": {} },
                        "prefersBorder": true
                    }
                }))),
            "ui://weather/plain" => ResourceContents::text("hi", "ui://weather/plain"),
            _ => return Err(ErrorData::resource_not_found("no such resource", None)),
        };
        Ok(ReadResourceResult::new(vec![contents]).into())
    }
}

async fn connect(server: Weather, client: AdkClientHandler) -> McpToolset<AdkClientHandler> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    McpToolset::new(client.serve(client_io).await.unwrap()).with_name("weather")
}

fn handler() -> AdkClientHandler {
    AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler))
}

fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(SimpleToolContext::new("mcp-apps"))
}

async fn tool_names(toolset: &dyn Toolset) -> Vec<String> {
    let mut names: Vec<String> =
        toolset.tools(ctx()).await.unwrap().iter().map(|tool| tool.name().to_string()).collect();
    names.sort();
    names
}

#[tokio::test]
async fn the_client_declares_mcp_apps_only_when_asked() {
    let with_apps = Weather::default();
    let declared = Arc::clone(&with_apps.declared);
    tool_names(&connect(with_apps, handler().with_apps()).await).await;
    let extensions = declared.lock().unwrap().clone().unwrap().extensions.unwrap();
    assert_eq!(
        extensions.get(MCP_APPS_EXTENSION_ID).cloned(),
        Some(serde_json::from_value(json!({ "mimeTypes": [MCP_APP_MIME_TYPE] })).unwrap())
    );

    let without = Weather::default();
    let declared = Arc::clone(&without.declared);
    tool_names(&connect(without, handler()).await).await;
    let capabilities = declared.lock().unwrap().clone().unwrap();
    assert!(
        capabilities
            .extensions
            .is_none_or(|extensions| !extensions.contains_key(MCP_APPS_EXTENSION_ID))
    );
}

#[tokio::test]
async fn a_tool_only_an_app_may_call_is_never_the_models() {
    let toolset = connect(Weather::default(), handler().with_apps()).await;
    assert_eq!(
        tool_names(&toolset).await,
        ["plain", "set_rain_alert", "transfer_funds", "weather_show"]
    );
}

#[tokio::test]
async fn ui_bindings_keep_every_tools_meta_ui() {
    let toolset = connect(Weather::default(), handler().with_apps()).await;
    let binding = |tool: &str, model: bool, app: bool| McpUiBinding {
        tool: tool.to_string(),
        resource_uri: APP.to_string(),
        visibility: McpUiVisibility { model, app },
    };
    assert_eq!(
        toolset.ui_bindings().await.unwrap(),
        vec![
            binding("weather_show", true, true),
            binding("set_rain_alert", true, true),
            binding("transfer_funds", true, false),
            binding("refresh_radar", false, true),
        ]
    );
}

#[tokio::test]
async fn an_app_resource_reads_back_with_what_it_declares() {
    let toolset = connect(Weather::default(), handler().with_apps()).await;
    let app = toolset.read_app(APP).await.unwrap();
    assert_eq!(
        app,
        adk_tool::mcp::McpAppResource {
            uri: APP.to_string(),
            mime_type: MCP_APP_MIME_TYPE.to_string(),
            html: "<p>Nairobi 24°</p>".to_string(),
            csp: McpUiCsp {
                connect_domains: vec!["https://api.weather.example".to_string()],
                ..McpUiCsp::default()
            },
            permissions: vec!["geolocation".to_string()],
            prefers_border: Some(true),
        }
    );
    let plain = toolset.read_app("ui://weather/plain").await.unwrap_err();
    assert!(plain.to_string().contains("is not an MCP App"), "{plain}");
    let not_ui = toolset.read_app("https://weather.example/app").await.unwrap_err();
    assert!(not_ui.to_string().contains("ui:// resources"), "{not_ui}");
}

#[tokio::test]
async fn a_call_of_a_tool_with_an_app_records_itself() {
    let toolset = connect(Weather::default(), handler().with_apps()).await;
    let tools = toolset.tools(ctx()).await.unwrap();
    let call = |name: &str, id: &str| {
        let tool = tools.iter().find(|tool| tool.name() == name).cloned().unwrap();
        let ctx = Arc::new(SimpleToolContext::new("mcp-apps").with_function_call_id(id));
        async move {
            tool.execute(ctx.clone() as Arc<dyn ToolContext>, json!({})).await.unwrap();
            ctx.actions().state_delta.get(MCP_APP_STATE_KEY).cloned()
        }
    };
    assert_eq!(
        call("weather_show", "call-1").await,
        Some(json!(McpAppRecord {
            server: "weather".into(),
            tool: "weather_show".into(),
            resource_uri: APP.into(),
            call_id: "call-1".into(),
        }))
    );
    // A tool without an app, and a call the server answered with an error, record nothing.
    assert_eq!(call("plain", "call-2").await, None);
    assert_eq!(call("transfer_funds", "call-3").await, None);
}

/// Serves [`Weather`] over stdio when [`SERVER_ENV`] is set.
#[tokio::test]
#[ignore = "runs only as the stdio MCP server the manager test starts"]
async fn stdio_server() {
    if std::env::var(SERVER_ENV).is_err() {
        return;
    }
    let running =
        Weather::default().serve((tokio::io::stdin(), tokio::io::stdout())).await.unwrap();
    let _ = running.waiting().await;
}

#[tokio::test]
async fn the_managed_connection_declares_apps_and_reads_them() {
    let config = McpServerConfig {
        command: std::env::current_exe().unwrap().display().to_string(),
        args: ["stdio_server", "--exact", "--ignored", "--quiet", "--test-threads=1"]
            .map(str::to_string)
            .to_vec(),
        env: HashMap::from([(SERVER_ENV.to_string(), "weather".to_string())]),
        ..Default::default()
    };
    let manager = McpServerManager::new(HashMap::from([("weather".to_string(), config)]));
    manager.start_server("weather").await.unwrap();
    let names = tool_names(&manager).await;
    let plain = manager.tools(ctx()).await.unwrap().into_iter().find(|tool| tool.name() == "plain");
    let declared = match plain {
        Some(plain) => plain.execute(Arc::new(SimpleToolContext::new("mcp-apps")), json!({})).await,
        None => Err(adk_core::AdkError::tool("no plain tool")),
    };
    let bindings = manager.server_ui_bindings("weather").await;
    let app = manager.read_server_app("weather", APP).await;
    manager.shutdown().await.unwrap();

    assert_eq!(names, ["plain", "set_rain_alert", "transfer_funds", "weather_show"]);
    assert_eq!(declared.unwrap(), json!({ "output": "apps: true" }));
    assert_eq!(bindings.unwrap().len(), 4);
    assert_eq!(app.unwrap().html, "<p>Nairobi 24°</p>");
}
