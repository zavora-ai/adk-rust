//! End-to-end tests of MCP over ACP and configurable LLM providers: the
//! adk-acp client talking to the adk-acp server over an in-process SDK
//! channel, plus the SDK's own MCP-over-ACP client machinery talking to the
//! adk-acp server.
#![cfg(feature = "server")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_acp::agent_client_protocol::mcp_server::McpServer as SdkMcpServer;
use adk_acp::agent_client_protocol::schema::ProtocolVersion;
use adk_acp::agent_client_protocol::v1::ConnectionTo;
use adk_acp::agent_client_protocol::v1::schema::{
    InitializeRequest, LlmProtocol, NewSessionRequest, SetProviderRequest, StopReason,
};
use adk_acp::agent_client_protocol::{Agent, Channel, Client, NullRun};
use adk_acp::server::{
    AcpServer, AcpServerConfigBuilder, ModelProvider, ModelProviders, SwitchableModel,
};
use adk_acp::{AcpAgentConfig, AcpConnection, AcpError, AcpToolServer, PermissionPolicy};
use adk_core::{
    Agent as AdkAgent, Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Tool,
    ToolContext,
};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

const LIMIT: Duration = Duration::from_secs(20);

// ─── The tool the client serves ─────────────────────────────────────────────

/// Looks up an order in the client application; `missing` fails.
struct LookupOrder {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for LookupOrder {
    fn name(&self) -> &str {
        "lookup_order"
    }

    fn description(&self) -> &str {
        "Look up an order by id."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "order": { "type": "string" } },
            "required": ["order"],
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match args["order"].as_str() {
            Some("missing") => Err(adk_core::AdkError::tool("no such order: missing")),
            Some(order) => Ok(json!({ "order": order, "status": "shipped" })),
            None => Err(adk_core::AdkError::tool("an order id is required")),
        }
    }
}

fn tool_server(calls: &Arc<AtomicUsize>) -> AcpToolServer {
    AcpToolServer::new("app-tools").tool(Arc::new(LookupOrder { calls: calls.clone() }))
}

// ─── The server's model: calls the tool, then reports what it returned ──────

/// Calls `lookup_order` for the order named in the user's text, then answers
/// with the function response it got back.
#[derive(Default)]
struct OrderModel {
    requests: Mutex<Vec<LlmRequest>>,
}

#[async_trait]
impl Llm for OrderModel {
    fn name(&self) -> &str {
        "order-model"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let last = request.contents.last().cloned();
        self.requests.lock().unwrap().push(request);
        let response =
            last.iter().flat_map(|content| content.parts.iter()).find_map(|part| match part {
                Part::FunctionResponse { function_response, .. } => {
                    Some(function_response.response.clone())
                }
                _ => None,
            });
        let content = match response {
            Some(response) => Content::new("model").with_text(format!("tool said {response}")),
            None => {
                let text: String = last
                    .iter()
                    .flat_map(|content| content.parts.iter())
                    .filter_map(Part::text)
                    .collect();
                let order = text.split_whitespace().last().unwrap_or("A-17").to_string();
                Content {
                    role: "model".to_string(),
                    parts: vec![Part::FunctionCall {
                        name: "lookup_order".to_string(),
                        args: json!({ "order": order }),
                        id: Some(format!("call-{order}")),
                        thought_signature: None,
                    }],
                }
            }
        };
        let response =
            LlmResponse { content: Some(content), turn_complete: true, ..Default::default() };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

impl OrderModel {
    /// Whether the last request declared `lookup_order` to the model.
    fn saw_tool_declaration(&self) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.tools.contains_key("lookup_order"))
    }
}

fn agent(model: Arc<dyn Llm>) -> Arc<dyn AdkAgent> {
    Arc::new(
        adk_agent::LlmAgentBuilder::new("mcp-agent")
            .model(model)
            .instruction("Use the tools the client provides.")
            .build()
            .expect("agent"),
    )
}

fn serve(
    agent: Arc<dyn AdkAgent>,
    configure: impl FnOnce(AcpServerConfigBuilder) -> AcpServerConfigBuilder,
) -> Channel {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let config = configure(
        AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(sessions)
            .agent_name("mcp-agent"),
    )
    .build()
    .expect("server config");
    let (server_end, client_end) = Channel::duplex();
    tokio::spawn(AcpServer::serve(config, server_end));
    client_end
}

async fn connect(client_end: Channel, config: AcpAgentConfig) -> AcpConnection {
    tokio::time::timeout(
        LIMIT,
        AcpConnection::connect_to(client_end, config, Arc::new(PermissionPolicy::DenyAll)),
    )
    .await
    .expect("connect hung")
    .expect("connect")
}

fn client_config() -> AcpAgentConfig {
    AcpAgentConfig::new("in-process").working_dir(std::env::current_dir().expect("cwd"))
}

// ─── MCP over ACP ───────────────────────────────────────────────────────────

#[tokio::test]
async fn the_agent_calls_tools_the_client_serves_over_the_connection() {
    let model = Arc::new(OrderModel::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let connection = connect(
        serve(agent(model.clone()), |b| b),
        client_config().tool_server(tool_server(&calls)),
    )
    .await;
    assert!(
        connection.agent_info().capabilities.mcp_capabilities.acp,
        "the adk-acp server advertises mcp.acp"
    );

    let mut session = connection.new_session().await.expect("session");
    let result = tokio::time::timeout(LIMIT, session.prompt("look up A-17"))
        .await
        .expect("prompt hung")
        .expect("prompt");
    assert_eq!(result.stop_reason, StopReason::EndTurn);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the tool ran once, in the client");
    assert!(model.saw_tool_declaration(), "the model was offered the client's tool");
    assert!(result.text.contains("\"status\":\"shipped\""), "got {}", result.text);
    assert!(result.text.contains("A-17"), "got {}", result.text);

    // A failing tool is an MCP result with isError, which the model reads.
    let failed = tokio::time::timeout(LIMIT, session.prompt("look up missing"))
        .await
        .expect("prompt hung")
        .expect("prompt");
    assert!(failed.text.contains("no such order: missing"), "got {}", failed.text);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // A fork declares the tool server again, so its turns reach the tools too.
    let mut fork = session.fork().await.expect("fork");
    let forked = tokio::time::timeout(LIMIT, fork.prompt("look up B-2"))
        .await
        .expect("prompt hung")
        .expect("prompt on the fork");
    assert!(forked.text.contains("B-2"), "got {}", forked.text);
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    fork.close().await.expect("close fork");
    let session_id = session.session_id().clone();
    session.close().await.expect("close");

    // So does a resumed session.
    let mut resumed = connection.resume_session(session_id).await.expect("resume");
    let again = tokio::time::timeout(LIMIT, resumed.prompt("look up C-3"))
        .await
        .expect("prompt hung")
        .expect("prompt after resume");
    assert!(again.text.contains("C-3"), "got {}", again.text);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    resumed.close().await.expect("close");
    connection.close().await.expect("close connection");
}

#[tokio::test]
async fn tool_server_names_must_be_unique_and_valid() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model: Arc<dyn Llm> = Arc::new(OrderModel::default());
    let duplicate =
        client_config().tool_server(tool_server(&calls)).tool_server(tool_server(&calls));
    let error = AcpConnection::connect_to(
        serve(agent(model.clone()), |b| b),
        duplicate,
        Arc::new(PermissionPolicy::DenyAll),
    )
    .await
    .err()
    .expect("duplicate names are refused");
    assert!(matches!(error, AcpError::InvalidConfig(_)), "got {error:?}");

    let unnamed = client_config().tool_server(AcpToolServer::new(""));
    let error = AcpConnection::connect_to(
        serve(agent(model), |b| b),
        unnamed,
        Arc::new(PermissionPolicy::DenyAll),
    )
    .await
    .err()
    .expect("an empty name is refused");
    assert!(matches!(error, AcpError::InvalidConfig(_)), "got {error:?}");
}

/// The SDK's own MCP-over-ACP client machinery (`McpServer::new_service` and
/// `SessionBuilder::with_mcp_server`) serves an `AcpToolServer` to the adk-acp
/// server: the server's requests pass the SDK binding's checks, and the SDK
/// routes them to the service.
#[tokio::test]
async fn the_sdk_mcp_binding_serves_an_acp_tool_server_to_the_adk_acp_server() {
    let model = Arc::new(OrderModel::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let client_end = serve(agent(model.clone()), |b| b);
    let service = tool_server(&calls);
    let cwd = std::env::current_dir().expect("cwd");

    let text = tokio::time::timeout(
        LIMIT,
        Client.v1().connect_with(client_end, async move |connection: ConnectionTo<Agent>| {
            let initialized = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            assert!(initialized.agent_capabilities.mcp_capabilities.acp);
            let mcp = SdkMcpServer::<Agent, _>::new_service("sdk-tools", service, NullRun);
            let setup = connection
                .build_session_from(NewSessionRequest::new(cwd))
                .with_mcp_server(mcp)?
                .block_task()
                .start_session()
                .await?;
            let mut session = setup.session;
            session.send_prompt("look up D-4")?;
            session.read_to_string().await
        }),
    )
    .await
    .expect("SDK client hung")
    .expect("SDK client");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(text.contains("D-4") && text.contains("shipped"), "got {text}");
}

// ─── Configurable LLM providers ─────────────────────────────────────────────

/// Answers every request with its own name.
struct Named(&'static str);

#[async_trait]
impl Llm for Named {
    fn name(&self) -> &str {
        self.0
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let response = LlmResponse {
            content: Some(Content::new("model").with_text(format!("answered by {}", self.0))),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

#[tokio::test]
async fn clients_list_switch_and_disable_the_agents_model_providers() {
    let main = SwitchableModel::new(Arc::new(Named("default-model")));
    let helper = SwitchableModel::new(Arc::new(Named("helper-model")));
    let seen = Arc::new(Mutex::new(Vec::<(String, Vec<String>)>::new()));
    let factory_seen = seen.clone();
    let providers = ModelProviders::new()
        .provider(
            ModelProvider::new("main", main.clone(), move |settings| {
                let mut headers: Vec<String> = settings.headers.keys().cloned().collect();
                headers.sort();
                factory_seen.lock().unwrap().push((settings.base_url.clone(), headers));
                Ok(Arc::new(Named("gateway-model")) as Arc<dyn Llm>)
            })
            .supports(LlmProtocol::OpenAi)
            .required(true)
            .current(LlmProtocol::OpenAi, "https://api.example.com/v1"),
        )
        .provider(
            ModelProvider::new("helper", helper, |_| {
                Ok(Arc::new(Named("helper-model")) as Arc<dyn Llm>)
            })
            .supports(LlmProtocol::Anthropic),
        );
    let client_end = serve(agent(Arc::new(main.clone())), |b| b.providers(Arc::new(providers)));
    let connection = connect(client_end, client_config()).await;
    assert!(connection.agent_info().capabilities.providers.is_some());

    let listed = connection.list_providers().await.expect("list");
    assert_eq!(listed.len(), 2);
    let listed_main = listed.iter().find(|provider| &*provider.provider_id.0 == "main").unwrap();
    assert!(listed_main.required);
    assert_eq!(listed_main.current.as_ref().unwrap().base_url, "https://api.example.com/v1");

    let mut session = connection.new_session().await.expect("session");
    assert_eq!(session.prompt("hi").await.expect("prompt").text, "answered by default-model");

    connection
        .set_provider(
            SetProviderRequest::new("main", LlmProtocol::OpenAi, "https://gateway.example/v1")
                .headers([("authorization".to_string(), "Bearer secret".to_string())].into()),
        )
        .await
        .expect("set");
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [("https://gateway.example/v1".to_string(), vec!["authorization".to_string()])]
    );
    assert_eq!(session.prompt("hi").await.expect("prompt").text, "answered by gateway-model");
    let listed = connection.list_providers().await.expect("list");
    let listed_main = listed.iter().find(|provider| &*provider.provider_id.0 == "main").unwrap();
    assert_eq!(listed_main.current.as_ref().unwrap().base_url, "https://gateway.example/v1");
    assert!(
        !serde_json::to_string(&listed).unwrap().contains("secret"),
        "headers are never listed"
    );

    // The server checks requests against the listing before the hook sees them.
    let wrong_protocol =
        SetProviderRequest::new("main", LlmProtocol::Bedrock, "https://gateway.example/v1");
    assert!(connection.set_provider(wrong_protocol).await.is_err());
    let unknown = SetProviderRequest::new("nope", LlmProtocol::OpenAi, "https://x.example");
    assert!(connection.set_provider(unknown).await.is_err());
    assert!(connection.disable_provider("main").await.is_err(), "required providers stay on");
    connection.disable_provider("unknown").await.expect("disabling an unknown provider succeeds");
    connection.disable_provider("helper").await.expect("disable");
    let listed = connection.list_providers().await.expect("list");
    let listed_helper =
        listed.iter().find(|provider| &*provider.provider_id.0 == "helper").unwrap();
    assert!(listed_helper.current.is_none(), "a disabled provider has no current target");

    session.close().await.expect("close");
    connection.close().await.expect("close connection");
}

#[tokio::test]
async fn providers_are_advertised_only_with_a_hook() {
    let connection = connect(serve(agent(Arc::new(Named("m"))), |b| b), client_config()).await;
    assert!(connection.agent_info().capabilities.providers.is_none());
    let error = connection.list_providers().await.expect_err("not advertised");
    assert!(matches!(error, AcpError::Unsupported(_)), "got {error:?}");

    // A client that skips the capability check is refused by the server.
    let refused = connection
        .sdk_connection()
        .expect("a v1 connection")
        .send_request(adk_acp::agent_client_protocol::v1::schema::ListProvidersRequest::new())
        .block_task()
        .await;
    assert!(refused.is_err());
    connection.close().await.expect("close connection");
}
