//! `adk-rust registry` — Google Agent Registry registration and discovery.
//!
//! Registers the systems manual registration exists for — self-hosted
//! agents, external MCP servers, and bare endpoints — and searches the
//! registry. Every registration is an idempotent upsert: re-running a
//! command patches the existing service instead of duplicating it.
//!
//! Agent Runtime deployments register themselves automatically with
//! lifecycle sync, so `deploy agent-engine` deliberately has no registration
//! flag — manually registering a deployed engine would duplicate it under a
//! different URN namespace. Manual entries are **not** lifecycle-synced;
//! re-run the register command whenever the registered system changes.

use std::fs;

use adk_tool::vertex::agent_registry::{
    AgentRegistryClient, AgentRegistryConfig, AgentSpec, Interface, McpServerSpec, ProtocolBinding,
    SearchComponent, SearchRequest, Service, ServiceUpsert,
};
use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::cli::{
    RegistryCommands, RegistryProtocol, RegistryRegisterAgentArgs, RegistryRegisterEndpointArgs,
    RegistryRegisterMcpArgs, RegistryScopeArgs, RegistrySearchArgs, RegistrySearchType,
};

/// The registry's spec-content size limit (A2A cards and MCP tool specs).
const MAX_SPEC_CONTENT_BYTES: usize = 10 * 1024;

pub async fn run(command: RegistryCommands) -> Result<()> {
    match command {
        RegistryCommands::RegisterAgent(args) => {
            let upsert = build_agent_upsert(&args)?;
            upsert_and_print(&args.scope, upsert).await
        }
        RegistryCommands::RegisterMcp(args) => {
            let upsert = build_mcp_upsert(&args)?;
            upsert_and_print(&args.scope, upsert).await
        }
        RegistryCommands::RegisterEndpoint(args) => {
            let upsert = build_endpoint_upsert(&args);
            upsert_and_print(&args.scope, upsert).await
        }
        RegistryCommands::Search(args) => search(args).await,
    }
}

/// Maps `register-agent` arguments onto a service upsert: an
/// `A2A_AGENT_CARD` spec from `--card`, or `NO_SPEC` plus one interface from
/// `--url`.
fn build_agent_upsert(args: &RegistryRegisterAgentArgs) -> Result<ServiceUpsert> {
    let display_name = args.display_name.clone().unwrap_or_else(|| args.service_id.clone());
    let mut upsert = if let Some(card_path) = &args.card {
        let card = load_spec_content(card_path)?;
        ServiceUpsert::agent(&args.service_id, display_name, AgentSpec::a2a_agent_card(card))
    } else {
        let url = args
            .url
            .as_deref()
            .ok_or_else(|| anyhow!("provide --card <agent-card.json> or --url <endpoint>"))?;
        ServiceUpsert::agent(&args.service_id, display_name, AgentSpec::no_spec())
            .with_interfaces(vec![interface(url, args.protocol)])
    };
    if let Some(description) = &args.description {
        upsert = upsert.with_description(description);
    }
    Ok(upsert)
}

/// Maps `register-mcp` arguments onto a service upsert: a `TOOL_SPEC` spec
/// from the caller-supplied `tools/list` JSON plus the server interface. No
/// introspection of the server is performed.
fn build_mcp_upsert(args: &RegistryRegisterMcpArgs) -> Result<ServiceUpsert> {
    let display_name = args.display_name.clone().unwrap_or_else(|| args.service_id.clone());
    let tools = load_spec_content(&args.tool_spec)?;
    let mut upsert =
        ServiceUpsert::mcp_server(&args.service_id, display_name, McpServerSpec::tool_spec(tools))
            .with_interfaces(vec![interface(&args.url, args.protocol)]);
    if let Some(description) = &args.description {
        upsert = upsert.with_description(description);
    }
    Ok(upsert)
}

/// Maps `register-endpoint` arguments onto a `NO_SPEC` endpoint upsert.
fn build_endpoint_upsert(args: &RegistryRegisterEndpointArgs) -> ServiceUpsert {
    let display_name = args.display_name.clone().unwrap_or_else(|| args.service_id.clone());
    let mut upsert = ServiceUpsert::endpoint(&args.service_id, display_name)
        .with_interfaces(vec![interface(&args.url, args.protocol)]);
    if let Some(description) = &args.description {
        upsert = upsert.with_description(description);
    }
    upsert
}

fn interface(url: &str, protocol: Option<RegistryProtocol>) -> Interface {
    let mut interface = Interface::new(url);
    if let Some(protocol) = protocol {
        interface = interface.with_protocol_binding(match protocol {
            RegistryProtocol::Jsonrpc => ProtocolBinding::Jsonrpc,
            RegistryProtocol::Grpc => ProtocolBinding::Grpc,
            RegistryProtocol::HttpJson => ProtocolBinding::HttpJson,
        });
    }
    interface
}

/// Reads and parses a spec-content file, enforcing the registry's 10 KB
/// serialized-size limit before any request is sent.
fn load_spec_content(path: &str) -> Result<Value> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read spec content file '{path}'"))?;
    let content: Value =
        serde_json::from_str(&raw).with_context(|| format!("'{path}' is not valid JSON"))?;
    let bytes = content.to_string().len();
    if bytes > MAX_SPEC_CONTENT_BYTES {
        return Err(anyhow!(
            "'{path}' serializes to {bytes} bytes, exceeding the Agent Registry's \
             {MAX_SPEC_CONTENT_BYTES}-byte spec-content limit; trim the file \
             (descriptions count toward the limit) and retry"
        ));
    }
    Ok(content)
}

/// Resolves the project/location scope from flags with environment fallback.
fn resolve_config(scope: &RegistryScopeArgs) -> Result<AgentRegistryConfig> {
    let project = resolve_scope_value(scope.project.clone(), "--project", "GOOGLE_CLOUD_PROJECT")?;
    let location =
        resolve_scope_value(scope.location.clone(), "--location", "GOOGLE_CLOUD_LOCATION")?;
    let mut config = AgentRegistryConfig::new(project, location);
    if let Some(endpoint) = &scope.endpoint {
        config = config.with_endpoint(endpoint);
    }
    Ok(config)
}

fn resolve_scope_value(arg: Option<String>, flag: &str, env_key: &str) -> Result<String> {
    if let Some(value) = arg.map(|value| value.trim().to_string()).filter(|v| !v.is_empty()) {
        return Ok(value);
    }
    std::env::var(env_key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("pass {flag} or set {env_key}"))
}

async fn upsert_and_print(scope: &RegistryScopeArgs, upsert: ServiceUpsert) -> Result<()> {
    let config = resolve_config(scope)?;
    let client = AgentRegistryClient::new_with_adc(config)?;
    let service = client.register_or_update_service(upsert).await?;
    print_service(&service);
    Ok(())
}

fn print_service(service: &Service) {
    println!("Service ready: {}", service.name);
    if let Some(resource) = &service.registry_resource {
        println!("Registry resource: {resource}");
    }
    println!(
        "Note: manual registrations are not lifecycle-synced; re-run this command after changes."
    );
}

async fn search(args: RegistrySearchArgs) -> Result<()> {
    let config = resolve_config(&args.scope)?;
    let client = AgentRegistryClient::new_with_adc(config)?;
    let component = match args.component_type {
        RegistrySearchType::Agent => SearchComponent::Agents,
        RegistrySearchType::McpServer => SearchComponent::McpServers,
    };
    let response = client.search(component, SearchRequest::new(&args.query)).await?;
    let mut matched = 0usize;
    match args.component_type {
        RegistrySearchType::Agent => {
            for agent in &response.agents {
                matched += 1;
                print_entry(
                    agent.agent_id.as_deref().unwrap_or(&agent.name),
                    agent.display_name.as_deref(),
                    agent.first_interface_url(),
                    agent.description.as_deref(),
                );
            }
        }
        RegistrySearchType::McpServer => {
            for server in &response.mcp_servers {
                matched += 1;
                print_entry(
                    server.mcp_server_id.as_deref().unwrap_or(&server.name),
                    server.display_name.as_deref(),
                    server.first_interface_url(),
                    server.description.as_deref(),
                );
            }
        }
    }
    if matched == 0 {
        println!("No matches for '{}'.", args.query);
    }
    if let Some(token) = &response.next_page_token {
        println!("More results available (next page token: {token}).");
    }
    Ok(())
}

fn print_entry(
    urn: &str,
    display_name: Option<&str>,
    endpoint: Option<&str>,
    description: Option<&str>,
) {
    println!("{urn}  {}  {}", display_name.unwrap_or("-"), endpoint.unwrap_or("-"));
    if let Some(description) = description {
        println!("    {description}");
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    use crate::cli::{Cli, Commands};
    use adk_tool::vertex::agent_registry::{AgentSpecType, McpServerSpecType, ServiceSpec};
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use clap::Parser;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Parses a full command line into a registry subcommand.
    fn parse(argv: &[&str]) -> RegistryCommands {
        let cli = Cli::try_parse_from(argv).expect("argv parses");
        match cli.command {
            Some(Commands::Registry { command }) => command,
            _ => panic!("argv did not parse to a registry command"),
        }
    }

    /// Writes a JSON fixture file into the temp dir and returns its path.
    fn fixture_file(name: &str, content: &Value) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("adk-cli-registry-{}-{name}", std::process::id()));
        fs::write(&path, content.to_string()).expect("write fixture file");
        path
    }

    #[test]
    fn register_agent_arg_parse_covers_the_full_surface() {
        let command = parse(&[
            "adk-rust",
            "registry",
            "register-agent",
            "--service-id",
            "invoicer-svc",
            "--url",
            "https://invoicer.example.com/a2a",
            "--protocol",
            "jsonrpc",
            "--display-name",
            "Invoicer",
            "--description",
            "Creates invoices.",
            "--project",
            "p",
            "--location",
            "global",
        ]);
        let RegistryCommands::RegisterAgent(args) = command else {
            panic!("expected register-agent");
        };
        assert_eq!(args.service_id, "invoicer-svc");
        assert_eq!(args.url.as_deref(), Some("https://invoicer.example.com/a2a"));
        assert_eq!(args.protocol, Some(RegistryProtocol::Jsonrpc));
        assert_eq!(args.display_name.as_deref(), Some("Invoicer"));
        assert_eq!(args.description.as_deref(), Some("Creates invoices."));
        assert_eq!(args.scope.project.as_deref(), Some("p"));
        assert_eq!(args.scope.location.as_deref(), Some("global"));
        assert_eq!(args.card, None);
        assert_eq!(args.scope.endpoint, None);
    }

    #[test]
    fn register_agent_requires_card_or_url_and_rejects_both() {
        let missing = Cli::try_parse_from([
            "adk-rust",
            "registry",
            "register-agent",
            "--service-id",
            "invoicer-svc",
        ]);
        assert!(missing.is_err(), "either --card or --url is required");

        let both = Cli::try_parse_from([
            "adk-rust",
            "registry",
            "register-agent",
            "--service-id",
            "invoicer-svc",
            "--card",
            "card.json",
            "--url",
            "https://a.example.com",
        ]);
        assert!(both.is_err(), "--card conflicts with --url");
    }

    #[test]
    fn register_mcp_and_endpoint_and_search_parse() {
        let command = parse(&[
            "adk-rust",
            "registry",
            "register-mcp",
            "--service-id",
            "ledger-mcp",
            "--tool-spec",
            "tools.json",
            "--url",
            "https://ledger.example.com/mcp",
        ]);
        let RegistryCommands::RegisterMcp(args) = command else {
            panic!("expected register-mcp");
        };
        assert_eq!(args.tool_spec, "tools.json");
        assert_eq!(args.url, "https://ledger.example.com/mcp");

        let command = parse(&[
            "adk-rust",
            "registry",
            "register-endpoint",
            "--service-id",
            "billing-api",
            "--url",
            "https://billing.example.com/v2",
            "--protocol",
            "http-json",
        ]);
        let RegistryCommands::RegisterEndpoint(args) = command else {
            panic!("expected register-endpoint");
        };
        assert_eq!(args.protocol, Some(RegistryProtocol::HttpJson));

        let command = parse(&["adk-rust", "registry", "search", "billing", "--type", "mcp-server"]);
        let RegistryCommands::Search(args) = command else { panic!("expected search") };
        assert_eq!(args.query, "billing");
        assert_eq!(args.component_type, RegistrySearchType::McpServer);

        // --type defaults to agent.
        let command = parse(&["adk-rust", "registry", "search", "billing"]);
        let RegistryCommands::Search(args) = command else { panic!("expected search") };
        assert_eq!(args.component_type, RegistrySearchType::Agent);
    }

    #[test]
    fn agent_upserts_map_card_and_url_forms() {
        let card = json!({ "name": "Invoicer" });
        let card_path = fixture_file("card.json", &card);
        let RegistryCommands::RegisterAgent(args) = parse(&[
            "adk-rust",
            "registry",
            "register-agent",
            "--service-id",
            "invoicer-svc",
            "--card",
            card_path.to_str().unwrap(),
            "--description",
            "Creates invoices.",
        ]) else {
            panic!("expected register-agent");
        };
        let upsert = build_agent_upsert(&args).unwrap();
        assert_eq!(
            upsert,
            ServiceUpsert::agent("invoicer-svc", "invoicer-svc", AgentSpec::a2a_agent_card(card))
                .with_description("Creates invoices."),
        );
        let ServiceSpec::Agent(spec) = &upsert.spec else { panic!("expected an agent spec") };
        assert_eq!(spec.spec_type, AgentSpecType::A2aAgentCard);
        let _ = fs::remove_file(card_path);

        let RegistryCommands::RegisterAgent(args) = parse(&[
            "adk-rust",
            "registry",
            "register-agent",
            "--service-id",
            "invoicer-svc",
            "--url",
            "https://invoicer.example.com/a2a",
            "--protocol",
            "jsonrpc",
            "--display-name",
            "Invoicer",
        ]) else {
            panic!("expected register-agent");
        };
        assert_eq!(
            build_agent_upsert(&args).unwrap(),
            ServiceUpsert::agent("invoicer-svc", "Invoicer", AgentSpec::no_spec()).with_interfaces(
                vec![
                    Interface::new("https://invoicer.example.com/a2a")
                        .with_protocol_binding(ProtocolBinding::Jsonrpc),
                ]
            ),
        );
    }

    #[test]
    fn oversized_spec_content_is_rejected_with_the_limit_in_the_message() {
        let card = json!({ "pad": "x".repeat(11 * 1024) });
        let card_path = fixture_file("oversized.json", &card);
        let error = load_spec_content(card_path.to_str().unwrap()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("10240"), "limit missing from: {message}");
        assert!(message.contains("trim the file"), "guidance missing from: {message}");
        let _ = fs::remove_file(card_path);
    }

    #[test]
    fn scope_resolution_prefers_flags_and_reports_missing_values() {
        let scope = RegistryScopeArgs {
            project: Some("p".into()),
            location: Some("global".into()),
            endpoint: None,
        };
        let config = resolve_config(&scope).unwrap();
        assert_eq!((config.project_id.as_str(), config.location.as_str()), ("p", "global"));

        // With no flag and no env var the error names both sources.
        let error = resolve_scope_value(None, "--project", "ADK_TEST_UNSET_ENV_VAR").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("--project") && message.contains("ADK_TEST_UNSET_ENV_VAR"));
    }

    /// The CLI path issues a well-formed lookup + create + poll sequence
    /// against a mock server.
    #[tokio::test]
    async fn register_mcp_issues_a_well_formed_upsert_against_a_mock_server() {
        const PROJECT: &str = "test-project";
        const LOCATION: &str = "global";
        let parent = format!("projects/{PROJECT}/locations/{LOCATION}");
        let operation_name = format!("{parent}/operations/1");
        let service_name = format!("{parent}/services/ledger-mcp");

        let bodies: Arc<Mutex<Vec<Value>>> = Arc::default();
        let app = Router::new()
            .route(
                &format!("/v1/{parent}/services/ledger-mcp"),
                get(|| async { (axum::http::StatusCode::NOT_FOUND, Json(json!({}))) }),
            )
            .route(
                &format!("/v1/{parent}/services"),
                post({
                    let operation_name = operation_name.clone();
                    move |State(bodies): State<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| {
                        let operation_name = operation_name.clone();
                        async move {
                            bodies.lock().await.push(body);
                            Json(json!({ "name": operation_name, "done": false }))
                        }
                    }
                }),
            )
            .route(
                &format!("/v1/{parent}/operations/{{op}}"),
                get({
                    let operation_name = operation_name.clone();
                    let service_name = service_name.clone();
                    move || async move {
                        Json(json!({
                            "name": operation_name,
                            "done": true,
                            "response": { "name": service_name },
                        }))
                    }
                }),
            )
            .with_state(bodies.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let tools = json!({ "tools": [{ "name": "post_entry" }] });
        let tools_path = fixture_file("tools.json", &tools);

        // upsert_and_print builds its client with ADC, which is unavailable
        // in CI — so the test drives the same upsert the CLI builds through
        // an explicit-credential client against the mock. The arg→upsert
        // mapping under test is byte-identical.
        let RegistryCommands::RegisterMcp(args) = parse(&[
            "adk-rust",
            "registry",
            "register-mcp",
            "--service-id",
            "ledger-mcp",
            "--tool-spec",
            tools_path.to_str().unwrap(),
            "--url",
            "https://ledger.example.com/mcp",
            "--protocol",
            "http-json",
            "--description",
            "Bookkeeping tools.",
            "--project",
            PROJECT,
            "--location",
            LOCATION,
            "--endpoint",
            &endpoint,
        ]) else {
            panic!("expected register-mcp");
        };
        let upsert = build_mcp_upsert(&args).unwrap();
        let ServiceSpec::McpServer(spec) = &upsert.spec else { panic!("expected an MCP spec") };
        assert_eq!(spec.spec_type, McpServerSpecType::ToolSpec);

        let config = resolve_config(&args.scope).unwrap();
        let credentials =
            google_cloud_auth::credentials::api_key_credentials::Builder::new("test-key").build();
        let client = AgentRegistryClient::with_credentials(config, credentials).unwrap();
        let service = client.register_or_update_service(upsert).await.unwrap();
        assert_eq!(service.name, service_name);

        let captured = bodies.lock().await;
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0],
            json!({
                "displayName": "ledger-mcp",
                "description": "Bookkeeping tools.",
                "interfaces": [
                    { "url": "https://ledger.example.com/mcp", "protocolBinding": "HTTP_JSON" },
                ],
                "mcpServerSpec": {
                    "type": "TOOL_SPEC",
                    "content": { "tools": [{ "name": "post_entry" }] },
                },
            }),
        );
        drop(captured);
        let _ = fs::remove_file(tools_path);
    }

    /// Re-running the same registration against an identical stored service
    /// issues no write at all — the CLI's idempotence guarantee.
    #[tokio::test]
    async fn rerunning_an_identical_registration_issues_no_write() {
        const PARENT: &str = "projects/test-project/locations/global";
        let writes: Arc<Mutex<Vec<Value>>> = Arc::default();
        let app = Router::new()
            .route(
                &format!("/v1/{PARENT}/services/billing-api"),
                get(|| async {
                    Json(json!({
                        "name": "projects/test-project/locations/global/services/billing-api",
                        "displayName": "billing-api",
                        "interfaces": [{ "url": "https://billing.example.com/v2" }],
                        "endpointSpec": { "type": "NO_SPEC" },
                    }))
                })
                .patch(
                    |State(writes): State<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| async move {
                        writes.lock().await.push(body);
                        Json(json!({ "name": format!("{PARENT}/operations/9"), "done": false }))
                    },
                ),
            )
            .with_state(writes.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let RegistryCommands::RegisterEndpoint(args) = parse(&[
            "adk-rust",
            "registry",
            "register-endpoint",
            "--service-id",
            "billing-api",
            "--url",
            "https://billing.example.com/v2",
            "--project",
            "test-project",
            "--location",
            "global",
            "--endpoint",
            &endpoint,
        ]) else {
            panic!("expected register-endpoint");
        };
        let upsert = build_endpoint_upsert(&args);
        let config = resolve_config(&args.scope).unwrap();
        let credentials =
            google_cloud_auth::credentials::api_key_credentials::Builder::new("test-key").build();
        let client = AgentRegistryClient::with_credentials(config, credentials).unwrap();
        let service = client.register_or_update_service(upsert).await.unwrap();

        assert_eq!(service.display_name.as_deref(), Some("billing-api"));
        assert!(writes.lock().await.is_empty(), "an identical re-registration must not write");
    }
}
