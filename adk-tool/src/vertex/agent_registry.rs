//! REST client and discovery tool for the Google Agent Registry (v1, GA).
//!
//! The [Agent Registry](https://cloud.google.com/agent-registry) is a hosted
//! Google Cloud catalog of agents, MCP servers, and plain endpoints. It is
//! served from the single global origin `https://agentregistry.googleapis.com`
//! — the endpoint is **not** location-prefixed and is **not** part of
//! `aiplatform.googleapis.com`.
//!
//! The API splits writes from reads:
//!
//! - **Write side** — the only writable resource is the *Service* at
//!   `projects/*/locations/*/services/*`. [`AgentRegistryClient::register_agent`]
//!   creates one with an [`AgentSpec`] and waits on the returned
//!   long-running operation.
//! - **Read side** — *Agent*, *McpServer*, and *Endpoint* are read-only
//!   projections derived from services. Agents and MCP servers support
//!   `:search` with a mini-language query; endpoints support only `list`
//!   with an AIP-160 filter.
//!
//! There is no content-level deduplication: registering the same agent twice
//! under different service IDs creates two entries. For idempotent
//! re-registration, search or get first, then patch the existing service with
//! an `updateMask` (patching arbitrary entries is deliberately not exposed
//! here).
//!
//! [`AgentSearchTool`] packages discovery as an [`adk_core::Tool`] so an LLM
//! agent can look up other agents, MCP servers, and endpoints at runtime.
//!
//! # Example
//!
//! ```no_run
//! use adk_tool::vertex::agent_registry::{
//!     AgentRegistryClient, AgentRegistryConfig, SearchComponent, SearchRequest,
//! };
//!
//! # async fn demo() -> adk_core::Result<()> {
//! let config = AgentRegistryConfig::new("my-project", "global");
//! let client = AgentRegistryClient::new_with_adc(config)?;
//!
//! let results = client
//!     .search(SearchComponent::Agents, SearchRequest::new("billing"))
//!     .await?;
//! for agent in results.agents {
//!     println!("{}: {:?}", agent.name, agent.first_interface_url());
//! }
//! # Ok(())
//! # }
//! ```

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient, LroPoller, truncate_for_error};
use async_trait::async_trait;
use google_cloud_auth::credentials::Credentials;
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const AGENT_REGISTRY_API_VERSION: &str = "v1";
const AGENT_REGISTRY_ENDPOINT: &str = "https://agentregistry.googleapis.com";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const AUTH_HEADERS_TIMEOUT: Duration = Duration::from_secs(30);
const ENV_GOOGLE_CLOUD_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
const ENV_GOOGLE_CLOUD_LOCATION: &str = "GOOGLE_CLOUD_LOCATION";
const MIN_SERVICE_ID_CHARS: usize = 4;
const MAX_SERVICE_ID_CHARS: usize = 63;
const MAX_DISPLAY_NAME_CHARS: usize = 63;
const MAX_DESCRIPTION_CHARS: usize = 2048;
const MAX_SPEC_CONTENT_BYTES: usize = 10 * 1024;

/// Configuration for the Agent Registry client.
#[derive(Debug, Clone)]
pub struct AgentRegistryConfig {
    /// Google Cloud project ID.
    pub project_id: String,
    /// Registry location segment (e.g. `global` or a region). The API origin
    /// is global either way; the location only scopes resource names.
    pub location: String,
    /// Optional custom API origin.
    ///
    /// The origin receives Google authorization headers plus registry data.
    /// It must not contain userinfo, a path, a query, or a fragment.
    pub endpoint: Option<String>,
    /// Project identifier expected in operation resource names.
    ///
    /// [`AgentRegistryClient::register_agent`] pins operation polling to
    /// `projects/{operation_project}/locations/{location}/`. The service may
    /// mint operation names carrying the project **number** rather than the
    /// configured project ID; when it does, set this to the project number so
    /// scope validation passes. Defaults to [`project_id`](Self::project_id).
    pub operation_project: Option<String>,
}

impl AgentRegistryConfig {
    /// Creates a new config with the given project ID and location.
    pub fn new(project_id: impl Into<String>, location: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            location: location.into(),
            endpoint: None,
            operation_project: None,
        }
    }

    /// Builds a config from environment variables.
    ///
    /// Reads `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`. Values are
    /// trimmed; blank values count as missing.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_tool::vertex::agent_registry::{AgentRegistryClient, AgentRegistryConfig};
    ///
    /// # fn main() -> adk_core::Result<()> {
    /// let config = AgentRegistryConfig::from_env()?;
    /// let client = AgentRegistryClient::new_with_adc(config)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error naming every missing or blank variable.
    pub fn from_env() -> Result<Self> {
        let read = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let project_id = read(ENV_GOOGLE_CLOUD_PROJECT);
        let location = read(ENV_GOOGLE_CLOUD_LOCATION);

        match (project_id, location) {
            (Some(project_id), Some(location)) => Ok(Self::new(project_id, location)),
            (project_id, location) => {
                let missing = [
                    (ENV_GOOGLE_CLOUD_PROJECT, project_id.is_none()),
                    (ENV_GOOGLE_CLOUD_LOCATION, location.is_none()),
                ]
                .into_iter()
                .filter_map(|(key, is_missing)| is_missing.then_some(key))
                .collect::<Vec<_>>()
                .join(", ");
                Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::InvalidInput,
                    "tool.agent_registry.missing_env",
                    format!(
                        "missing or blank environment variable(s): {missing}. Set them, or construct the config with AgentRegistryConfig::new",
                    ),
                )
                .with_provider("google_cloud"))
            }
        }
    }

    /// Sets a custom API origin.
    ///
    /// Use only a trusted HTTPS origin, or loopback HTTP for local tests.
    /// Userinfo, paths, queries, and fragments are rejected before transport.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Sets the project identifier expected in operation resource names.
    ///
    /// See [`operation_project`](Self::operation_project) for when the
    /// project **number** is required instead of the project ID.
    #[must_use]
    pub fn with_operation_project(mut self, operation_project: impl Into<String>) -> Self {
        self.operation_project = Some(operation_project.into());
        self
    }

    fn endpoint(&self) -> String {
        let endpoint = self.endpoint.clone().unwrap_or_else(|| AGENT_REGISTRY_ENDPOINT.to_string());
        if endpoint.contains("://") { endpoint } else { format!("https://{endpoint}") }
    }
}

// ===== Wire types (v1, camelCase JSON) =====

/// A network interface a registry entry is reachable on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interface {
    /// The interface URL.
    #[serde(default)]
    pub url: String,
    /// The protocol binding served at [`url`](Self::url).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_binding: Option<ProtocolBinding>,
}

impl Interface {
    /// Creates an interface for the given URL.
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into(), protocol_binding: None }
    }

    /// Sets the protocol binding.
    #[must_use]
    pub fn with_protocol_binding(mut self, binding: ProtocolBinding) -> Self {
        self.protocol_binding = Some(binding);
        self
    }
}

/// The protocol binding of an [`Interface`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProtocolBinding {
    /// JSON-RPC over HTTP.
    Jsonrpc,
    /// gRPC.
    Grpc,
    /// REST-style JSON over HTTP.
    HttpJson,
    /// Deserialization fallback for bindings this crate does not know yet.
    /// Never serialize this variant.
    #[serde(other)]
    ProtocolBindingUnspecified,
}

/// The declared kind of an [`AgentSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AgentSpecType {
    /// No machine-readable spec; interfaces are declared on the service.
    NoSpec,
    /// An embedded A2A agent card; interfaces come from the card.
    A2aAgentCard,
    /// Deserialization fallback for types this crate does not know yet.
    /// Never serialize this variant.
    #[serde(other)]
    TypeUnspecified,
}

/// The agent spec carried by a writable service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSpec {
    /// The spec kind.
    #[serde(rename = "type")]
    pub spec_type: AgentSpecType,
    /// The spec payload — for [`AgentSpecType::A2aAgentCard`], the raw A2A
    /// agent card JSON (at most 10 KB serialized).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

impl AgentSpec {
    /// Creates an `A2A_AGENT_CARD` spec from raw A2A agent card JSON.
    ///
    /// With a card spec the service's interfaces must be empty — the
    /// registry derives them from the card.
    pub fn a2a_agent_card(content: Value) -> Self {
        Self { spec_type: AgentSpecType::A2aAgentCard, content: Some(content) }
    }

    /// Creates a `NO_SPEC` spec; interfaces are declared on the service.
    pub fn no_spec() -> Self {
        Self { spec_type: AgentSpecType::NoSpec, content: None }
    }
}

/// The declared kind of an MCP server spec on a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum McpServerSpecType {
    /// No machine-readable spec.
    NoSpec,
    /// An embedded MCP tool spec.
    ToolSpec,
    /// Deserialization fallback for types this crate does not know yet.
    /// Never serialize this variant.
    #[serde(other)]
    TypeUnspecified,
}

/// The MCP server spec carried by a writable service (read back only; this
/// client registers agents, not MCP servers).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSpec {
    /// The spec kind.
    #[serde(rename = "type")]
    pub spec_type: McpServerSpecType,
    /// The spec payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

/// The declared kind of an endpoint spec on a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EndpointSpecType {
    /// No machine-readable spec.
    NoSpec,
    /// Deserialization fallback for types this crate does not know yet.
    /// Never serialize this variant.
    #[serde(other)]
    TypeUnspecified,
}

/// The endpoint spec carried by a writable service (read back only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointSpec {
    /// The spec kind.
    #[serde(rename = "type")]
    pub spec_type: EndpointSpecType,
}

/// The writable service resource at `projects/*/locations/*/services/*`,
/// as returned by the registry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    /// Full resource name `projects/*/locations/*/services/*`.
    #[serde(default)]
    pub name: String,
    /// Human-readable display name (at most 63 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Human-readable description (at most 2048 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Interfaces the service is reachable on. Must be empty when the agent
    /// spec is an `A2A_AGENT_CARD` (they come from the card).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<Interface>,
    /// The agent spec, when this service registers an agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_spec: Option<AgentSpec>,
    /// The MCP server spec, when this service registers an MCP server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_server_spec: Option<McpServerSpec>,
    /// The endpoint spec, when this service registers a plain endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_spec: Option<EndpointSpec>,
    /// Output only: the read-only projection resource derived from this
    /// service (an `agents/*`, `mcpServers/*`, or `endpoints/*` name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_resource: Option<String>,
    /// Creation timestamp (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time: Option<String>,
    /// Last-update timestamp (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
}

/// A prepared agent registration for [`AgentRegistryClient::register_agent`].
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceRegistration {
    /// The service ID to create (4–63 characters; the last resource-name
    /// segment).
    pub service_id: String,
    /// Human-readable display name (at most 63 characters).
    pub display_name: String,
    /// Human-readable description (at most 2048 characters).
    pub description: Option<String>,
    /// Interfaces the agent is reachable on. Must be empty when
    /// [`agent_spec`](Self::agent_spec) is an `A2A_AGENT_CARD`.
    pub interfaces: Vec<Interface>,
    /// The agent spec to register.
    pub agent_spec: AgentSpec,
}

impl ServiceRegistration {
    /// Creates a registration with the given service ID, display name, and
    /// agent spec.
    pub fn new(
        service_id: impl Into<String>,
        display_name: impl Into<String>,
        agent_spec: AgentSpec,
    ) -> Self {
        Self {
            service_id: service_id.into(),
            display_name: display_name.into(),
            description: None,
            interfaces: Vec::new(),
            agent_spec,
        }
    }

    /// Sets the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the interfaces. Only valid with a [`AgentSpec::no_spec`] spec;
    /// an `A2A_AGENT_CARD` spec derives interfaces from the card.
    #[must_use]
    pub fn with_interfaces(mut self, interfaces: Vec<Interface>) -> Self {
        self.interfaces = interfaces;
        self
    }
}

/// The service fields sent on `services.create`; `serviceId` and `requestId`
/// travel as query parameters, and output-only fields are never sent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceWriteBody {
    display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    interfaces: Vec<Interface>,
    agent_spec: AgentSpec,
}

/// A skill advertised by an [`Agent`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    /// Stable skill identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Human-readable skill name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What the skill does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Free-form tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Example invocations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
}

/// A protocol an [`Agent`] speaks, with the interfaces serving it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProtocol {
    /// The protocol kind — `"A2A_AGENT"` or `"CUSTOM"`. Kept as a string for
    /// forward compatibility.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub protocol_type: Option<String>,
    /// The protocol version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    /// Interfaces serving this protocol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<Interface>,
}

/// The embedded card of an [`Agent`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    /// The card kind — `"A2A_AGENT_CARD"`. Kept as a string for forward
    /// compatibility.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub card_type: Option<String>,
    /// The raw A2A agent card JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

/// A read-only agent projection at `projects/*/locations/*/agents/*`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    /// Full resource name `projects/*/locations/*/agents/*`.
    #[serde(default)]
    pub name: String,
    /// Stable agent URN, `urn:agent:{publisher}:{namespace}:{name}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// System-assigned unique identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    /// Human-readable display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// What the agent does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Agent version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Skills the agent advertises.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<AgentSkill>,
    /// Protocols the agent speaks, each with its interfaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocols: Vec<AgentProtocol>,
    /// The embedded agent card, when registered from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<AgentCard>,
    /// Free-form attribute map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Value>,
    /// Creation timestamp (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time: Option<String>,
    /// Last-update timestamp (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
}

impl Agent {
    /// The first interface URL across the agent's protocols, if any.
    pub fn first_interface_url(&self) -> Option<&str> {
        self.protocols
            .iter()
            .flat_map(|protocol| protocol.interfaces.iter())
            .map(|interface| interface.url.as_str())
            .next()
    }
}

/// Behavioral hints on an MCP server tool. There is no `inputSchema` in the
/// registry projection — fetch it from the server itself over MCP.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    /// Human-readable tool title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Whether the tool performs no side effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    /// Whether the tool may perform destructive updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// Whether repeated calls with the same arguments have no further effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// Whether the tool interacts with an open world of entities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// A tool advertised by an [`McpServer`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerTool {
    /// Tool name.
    #[serde(default)]
    pub name: String,
    /// What the tool does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Behavioral hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpToolAnnotations>,
}

/// A read-only MCP server projection at `projects/*/locations/*/mcpServers/*`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    /// Full resource name `projects/*/locations/*/mcpServers/*`.
    #[serde(default)]
    pub name: String,
    /// Stable MCP server identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_server_id: Option<String>,
    /// Human-readable display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// What the server provides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Interfaces the server is reachable on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<Interface>,
    /// Tools the server advertises.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<McpServerTool>,
    /// Free-form attribute map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Value>,
    /// Creation timestamp (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time: Option<String>,
    /// Last-update timestamp (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
}

impl McpServer {
    /// The first interface URL, if any.
    pub fn first_interface_url(&self) -> Option<&str> {
        self.interfaces.first().map(|interface| interface.url.as_str())
    }
}

/// A read-only endpoint projection at `projects/*/locations/*/endpoints/*`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    /// Full resource name `projects/*/locations/*/endpoints/*`.
    #[serde(default)]
    pub name: String,
    /// Stable endpoint identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_id: Option<String>,
    /// Human-readable display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// What the endpoint serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Interfaces the endpoint is reachable on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<Interface>,
    /// Free-form attribute map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Value>,
    /// Creation timestamp (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time: Option<String>,
    /// Last-update timestamp (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
}

impl Endpoint {
    /// The first interface URL, if any.
    pub fn first_interface_url(&self) -> Option<&str> {
        self.interfaces.first().map(|interface| interface.url.as_str())
    }
}

/// Which searchable registry collection a search targets. Endpoints have no
/// search — list them with [`AgentRegistryClient::list_endpoints`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchComponent {
    /// Search `{parent}/agents:search`.
    Agents,
    /// Search `{parent}/mcpServers:search`.
    McpServers,
}

impl SearchComponent {
    fn collection(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::McpServers => "mcpServers",
        }
    }
}

/// A registry search request.
///
/// `search_string` is a mini-language: bare words match word-contains,
/// `field="value"` matches exactly, `NOT`/`AND`/`OR` and parentheses combine
/// terms, and a `*` suffix matches prefixes. Responses carry no relevance
/// scores.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    /// The search expression.
    pub search_string: String,
    /// Page size (server default 20, capped at 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i32>,
    /// Continuation token from a previous response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
}

impl SearchRequest {
    /// Creates a search request for the given expression.
    pub fn new(search_string: impl Into<String>) -> Self {
        Self { search_string: search_string.into(), page_size: None, page_token: None }
    }

    /// Sets the page size (server default 20, capped at 100).
    #[must_use]
    pub fn with_page_size(mut self, page_size: i32) -> Self {
        self.page_size = Some(page_size);
        self
    }

    /// Sets the continuation token.
    #[must_use]
    pub fn with_page_token(mut self, page_token: impl Into<String>) -> Self {
        self.page_token = Some(page_token.into());
        self
    }
}

/// A registry search response. [`agents`](Self::agents) is populated for
/// [`SearchComponent::Agents`] and [`mcp_servers`](Self::mcp_servers) for
/// [`SearchComponent::McpServers`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    /// Matching agents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<Agent>,
    /// Matching MCP servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<McpServer>,
    /// Continuation token for the next page, when more results exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// Query parameters for [`AgentRegistryClient::list_agents`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListAgentsRequest {
    /// AIP-160 filter expression.
    pub filter: Option<String>,
    /// Order-by expression.
    pub order_by: Option<String>,
    /// Page size.
    pub page_size: Option<i32>,
    /// Continuation token from a previous response.
    pub page_token: Option<String>,
}

impl ListAgentsRequest {
    /// Creates an empty list request (first page, no filter).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the AIP-160 filter expression.
    #[must_use]
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    /// Sets the order-by expression.
    #[must_use]
    pub fn with_order_by(mut self, order_by: impl Into<String>) -> Self {
        self.order_by = Some(order_by.into());
        self
    }

    /// Sets the page size.
    #[must_use]
    pub fn with_page_size(mut self, page_size: i32) -> Self {
        self.page_size = Some(page_size);
        self
    }

    /// Sets the continuation token.
    #[must_use]
    pub fn with_page_token(mut self, page_token: impl Into<String>) -> Self {
        self.page_token = Some(page_token.into());
        self
    }

    fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(filter) = &self.filter {
            pairs.push(("filter", filter.clone()));
        }
        if let Some(order_by) = &self.order_by {
            pairs.push(("orderBy", order_by.clone()));
        }
        if let Some(page_size) = self.page_size {
            pairs.push(("pageSize", page_size.to_string()));
        }
        if let Some(page_token) = &self.page_token {
            pairs.push(("pageToken", page_token.clone()));
        }
        pairs
    }
}

/// Response for [`AgentRegistryClient::list_agents`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgentsResponse {
    /// The agents on this page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<Agent>,
    /// Continuation token for the next page, when more results exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// Query parameters for [`AgentRegistryClient::list_endpoints`]. Endpoints
/// have no `:search`; an AIP-160 filter is the only query mechanism.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListEndpointsRequest {
    /// AIP-160 filter expression.
    pub filter: Option<String>,
    /// Page size.
    pub page_size: Option<i32>,
    /// Continuation token from a previous response.
    pub page_token: Option<String>,
}

impl ListEndpointsRequest {
    /// Creates an empty list request (first page, no filter).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the AIP-160 filter expression.
    #[must_use]
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    /// Sets the page size.
    #[must_use]
    pub fn with_page_size(mut self, page_size: i32) -> Self {
        self.page_size = Some(page_size);
        self
    }

    /// Sets the continuation token.
    #[must_use]
    pub fn with_page_token(mut self, page_token: impl Into<String>) -> Self {
        self.page_token = Some(page_token.into());
        self
    }

    fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(filter) = &self.filter {
            pairs.push(("filter", filter.clone()));
        }
        if let Some(page_size) = self.page_size {
            pairs.push(("pageSize", page_size.to_string()));
        }
        if let Some(page_token) = &self.page_token {
            pairs.push(("pageToken", page_token.clone()));
        }
        pairs
    }
}

/// Response for [`AgentRegistryClient::list_endpoints`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListEndpointsResponse {
    /// The endpoints on this page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<Endpoint>,
    /// Continuation token for the next page, when more results exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

// ===== Client =====

const GCP_ERROR_CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "tool.agent_registry.invalid_input",
    unauthorized: "tool.agent_registry.unauthorized",
    forbidden: "tool.agent_registry.forbidden",
    not_found: "tool.agent_registry.not_found",
    rate_limited: "tool.agent_registry.rate_limited",
    timeout: "tool.agent_registry.timeout",
    unavailable: "tool.agent_registry.unavailable",
    credentials_unavailable: "tool.agent_registry.credentials_unavailable",
    invalid_response: "tool.agent_registry.invalid_response",
    invalid_request: "tool.agent_registry.invalid_request",
    upstream_error: "tool.agent_registry.upstream_error",
    operation_failed: "tool.agent_registry.operation_failed",
};

/// ADC-authenticated REST client for the Google Agent Registry (v1).
///
/// Registers agents as writable services and reads the derived agent,
/// MCP-server, and endpoint projections. General update and delete of
/// arbitrary registry entries are deliberately not exposed; for idempotent
/// re-registration, search or get first and patch the existing service with
/// an `updateMask` through other tooling.
pub struct AgentRegistryClient {
    client: GcpHttpClient,
    poller: LroPoller,
    project_id: String,
    location: String,
    operation_project: String,
}

impl std::fmt::Debug for AgentRegistryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The transport carries credentials; expose only the scope.
        f.debug_struct("AgentRegistryClient")
            .field("project_id", &self.project_id)
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

impl AgentRegistryClient {
    /// Creates a new client using Application Default Credentials (ADC).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_tool::vertex::agent_registry::{AgentRegistryClient, AgentRegistryConfig};
    ///
    /// # fn main() -> adk_core::Result<()> {
    /// let config = AgentRegistryConfig::new("my-project", "global");
    /// let client = AgentRegistryClient::new_with_adc(config)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed, the endpoint is not a
    /// valid secure origin, or the redirect-disabled HTTP client cannot be
    /// constructed.
    pub fn new_with_adc(config: AgentRegistryConfig) -> Result<Self> {
        Self::build(config, None)
    }

    /// Creates a new client with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid secure origin or the
    /// redirect-disabled HTTP client cannot be constructed.
    pub fn with_credentials(config: AgentRegistryConfig, credentials: Credentials) -> Result<Self> {
        Self::build(config, Some(credentials))
    }

    fn build(config: AgentRegistryConfig, credentials: Option<Credentials>) -> Result<Self> {
        let errors = GcpErrorContext::new(ErrorComponent::Tool, GCP_ERROR_CODES, "agent registry")
            .with_provider("google_cloud");
        let mut builder = GcpHttpClient::builder(errors, config.endpoint())
            .api_version(AGENT_REGISTRY_API_VERSION)
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .request_timeout(HTTP_REQUEST_TIMEOUT)
            .auth_timeout(AUTH_HEADERS_TIMEOUT);
        if let Some(credentials) = credentials {
            builder = builder.credentials(credentials);
        }
        let operation_project =
            config.operation_project.clone().unwrap_or_else(|| config.project_id.clone());
        Ok(Self {
            client: builder.build()?,
            poller: LroPoller::new(),
            project_id: config.project_id,
            location: config.location,
            operation_project,
        })
    }

    /// Replaces the long-running-operation poller (deadline, backoff).
    #[must_use]
    pub fn with_lro_poller(mut self, poller: LroPoller) -> Self {
        self.poller = poller;
        self
    }

    /// The `projects/{project}/locations/{location}` parent this client
    /// operates under.
    pub fn parent(&self) -> String {
        format!("projects/{}/locations/{}", self.project_id, self.location)
    }

    /// Registers an agent by creating a service with an agent spec.
    ///
    /// `POST {parent}/services?serviceId={id}&requestId={uuid}` returns a
    /// `google.longrunning.Operation`, which is polled to completion. A fresh
    /// `requestId` UUID is generated per call; the server deduplicates
    /// retries carrying the same ID for at least 60 minutes. There is no
    /// content-level deduplication — re-registering the same agent under a
    /// different service ID creates a second entry.
    ///
    /// > **Important:** operation polling validates operation names against
    /// > `projects/{project}/locations/{location}/`. When the service mints
    /// > operation names with the project **number**, set
    /// > [`AgentRegistryConfig::operation_project`] to the project number.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_tool::vertex::agent_registry::{
    ///     AgentRegistryClient, AgentRegistryConfig, AgentSpec, ServiceRegistration,
    /// };
    /// use serde_json::json;
    ///
    /// # async fn demo() -> adk_core::Result<()> {
    /// let client = AgentRegistryClient::new_with_adc(
    ///     AgentRegistryConfig::new("my-project", "global"),
    /// )?;
    /// let service = client
    ///     .register_agent(
    ///         ServiceRegistration::new(
    ///             "invoicer-svc",
    ///             "Invoicer",
    ///             AgentSpec::a2a_agent_card(json!({ "name": "Invoicer" })),
    ///         )
    ///         .with_description("Creates and sends invoices."),
    ///     )
    ///     .await?;
    /// println!("registered {}", service.name);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when the registration violates the
    /// documented constraints (service ID 4–63 characters, display name at
    /// most 63, description at most 2048, spec content at most 10 KB,
    /// interfaces empty with an `A2A_AGENT_CARD` spec), and an error on
    /// transport failure, a non-success HTTP status, a failed or timed-out
    /// operation, or an unparseable response.
    pub async fn register_agent(&self, registration: ServiceRegistration) -> Result<Service> {
        self.validate_registration(&registration)?;
        let request_id = uuid::Uuid::new_v4().to_string();
        tracing::info!(
            agent_registry.service_id = %registration.service_id,
            agent_registry.request_id = %request_id,
            "registering agent service"
        );
        let ServiceRegistration { service_id, display_name, description, interfaces, agent_spec } =
            registration;
        let body = ServiceWriteBody { display_name, description, interfaces, agent_spec };
        let request = self
            .client
            .request(Method::POST, &format!("{}/services", self.parent()))
            .await?
            .query(&[("serviceId", service_id.as_str()), ("requestId", request_id.as_str())])
            .json(&body);
        let operation = self.client.send_value(request).await?;
        let response = self
            .poller
            .wait_for_operation(
                &self.client,
                operation,
                "service create",
                true,
                &self.operation_project,
                &self.location,
            )
            .await?;
        let value = response.ok_or_else(|| {
            self.client.errors().invalid_response(
                "agent registry service create operation completed without a service payload",
            )
        })?;
        self.parse(value, "created service")
    }

    /// Fetches an agent by full resource name or by URN.
    ///
    /// A `projects/*/locations/*/agents/*` name is fetched directly with
    /// `GET {v1}/{name}`. A `urn:agent:{publisher}:{namespace}:{name}` URN is
    /// resolved by searching on `agentId` and fetching the match.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when `name_or_urn` is neither shape, a
    /// not-found error when a URN matches no agent, and an error on transport
    /// failure, a non-success HTTP status, or an unparseable response.
    pub async fn get_agent(&self, name_or_urn: &str) -> Result<Agent> {
        let name = if name_or_urn.starts_with("urn:") {
            self.agent_name_for_urn(name_or_urn).await?
        } else {
            self.validated_name(name_or_urn, "/agents/")?
        };
        self.get_json(&name, "agent").await
    }

    /// Lists agents under the configured parent.
    ///
    /// `GET {parent}/agents` with optional `filter`, `orderBy`, `pageSize`,
    /// and `pageToken` query parameters.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, a non-success HTTP status, or
    /// an unparseable response.
    pub async fn list_agents(&self, request: ListAgentsRequest) -> Result<ListAgentsResponse> {
        let http = self
            .client
            .request(Method::GET, &format!("{}/agents", self.parent()))
            .await?
            .query(&request.query_pairs());
        let value = self.client.send_value(http).await?;
        self.parse(value, "agent list")
    }

    /// Searches agents or MCP servers.
    ///
    /// `POST {parent}/agents:search` or `{parent}/mcpServers:search` with a
    /// `searchString` mini-language expression. Responses carry no relevance
    /// scores. Endpoints have no search — use
    /// [`list_endpoints`](Self::list_endpoints) with a filter instead.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, a non-success HTTP status, or
    /// an unparseable response.
    pub async fn search(
        &self,
        component: SearchComponent,
        request: SearchRequest,
    ) -> Result<SearchResponse> {
        tracing::debug!(
            agent_registry.collection = component.collection(),
            "searching agent registry"
        );
        let http = self
            .client
            .request(Method::POST, &format!("{}/{}:search", self.parent(), component.collection()))
            .await?
            .json(&request);
        let value = self.client.send_value(http).await?;
        self.parse(value, "search response")
    }

    /// Lists endpoints under the configured parent.
    ///
    /// `GET {parent}/endpoints` with optional `filter`, `pageSize`, and
    /// `pageToken` query parameters. This is the only query mechanism for
    /// endpoints — they have no `:search`.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, a non-success HTTP status, or
    /// an unparseable response.
    pub async fn list_endpoints(
        &self,
        request: ListEndpointsRequest,
    ) -> Result<ListEndpointsResponse> {
        let http = self
            .client
            .request(Method::GET, &format!("{}/endpoints", self.parent()))
            .await?
            .query(&request.query_pairs());
        let value = self.client.send_value(http).await?;
        self.parse(value, "endpoint list")
    }

    /// Resolves a registry entry to its first interface URL.
    ///
    /// Accepts an agent URN, or a full `agents/*`, `mcpServers/*`, or
    /// `endpoints/*` resource name. For agents the interfaces come from the
    /// agent's protocols; for MCP servers and endpoints from the top-level
    /// interface list.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when `name_or_urn` is neither shape, a
    /// not-found error when the entry does not exist or declares no
    /// interfaces, and an error on transport failure, a non-success HTTP
    /// status, or an unparseable response.
    pub async fn resolve_endpoint(&self, name_or_urn: &str) -> Result<String> {
        let url = if name_or_urn.starts_with("urn:") || name_or_urn.contains("/agents/") {
            self.get_agent(name_or_urn).await?.first_interface_url().map(str::to_string)
        } else if name_or_urn.contains("/mcpServers/") {
            let name = self.validated_name(name_or_urn, "/mcpServers/")?;
            let server: McpServer = self.get_json(&name, "MCP server").await?;
            server.first_interface_url().map(str::to_string)
        } else if name_or_urn.contains("/endpoints/") {
            let name = self.validated_name(name_or_urn, "/endpoints/")?;
            let endpoint: Endpoint = self.get_json(&name, "endpoint").await?;
            endpoint.first_interface_url().map(str::to_string)
        } else {
            return Err(self.client.errors().invalid_input(format!(
                "'{}' is neither an agent URN nor a full agents/mcpServers/endpoints resource name",
                truncate_for_error(name_or_urn),
            )));
        };
        url.ok_or_else(|| {
            self.not_found(format!(
                "agent registry entry '{}' declares no interface URLs",
                truncate_for_error(name_or_urn),
            ))
        })
    }

    async fn agent_name_for_urn(&self, urn: &str) -> Result<String> {
        if urn.contains('"') || urn.chars().any(char::is_whitespace) {
            return Err(self.client.errors().invalid_input(format!(
                "agent URN '{}' must not contain quotes or whitespace",
                truncate_for_error(urn),
            )));
        }
        let request = SearchRequest::new(format!("agentId=\"{urn}\"")).with_page_size(1);
        let response = self.search(SearchComponent::Agents, request).await?;
        let Some(agent) = response.agents.into_iter().next() else {
            return Err(self.not_found(format!(
                "no agent with URN '{}' found under {}",
                truncate_for_error(urn),
                self.parent(),
            )));
        };
        self.validated_name(&agent.name, "/agents/")
    }

    fn validated_name(&self, name: &str, segment: &str) -> Result<String> {
        let collection = segment.trim_matches('/');
        if !name.starts_with("projects/")
            || !name.contains(segment)
            || name.contains("://")
            || name.contains("..")
        {
            return Err(self.client.errors().invalid_input(format!(
                "'{}' is not a full agent registry resource name; expected projects/*/locations/*/{collection}/*",
                truncate_for_error(name),
            )));
        }
        Ok(name.to_string())
    }

    fn validate_registration(&self, registration: &ServiceRegistration) -> Result<()> {
        let errors = self.client.errors();
        let id_chars = registration.service_id.chars().count();
        if !(MIN_SERVICE_ID_CHARS..=MAX_SERVICE_ID_CHARS).contains(&id_chars) {
            return Err(errors.invalid_input(format!(
                "service ID must be {MIN_SERVICE_ID_CHARS}-{MAX_SERVICE_ID_CHARS} characters, got {id_chars}",
            )));
        }
        let display_name_chars = registration.display_name.chars().count();
        if display_name_chars > MAX_DISPLAY_NAME_CHARS {
            return Err(errors.invalid_input(format!(
                "display name must be at most {MAX_DISPLAY_NAME_CHARS} characters, got {display_name_chars}",
            )));
        }
        if let Some(description) = &registration.description {
            let description_chars = description.chars().count();
            if description_chars > MAX_DESCRIPTION_CHARS {
                return Err(errors.invalid_input(format!(
                    "description must be at most {MAX_DESCRIPTION_CHARS} characters, got {description_chars}",
                )));
            }
        }
        match registration.agent_spec.spec_type {
            AgentSpecType::A2aAgentCard => {
                if !registration.interfaces.is_empty() {
                    return Err(errors.invalid_input(
                        "interfaces must be empty when the agent spec is A2A_AGENT_CARD; the registry derives them from the agent card",
                    ));
                }
                let Some(content) = &registration.agent_spec.content else {
                    return Err(
                        errors.invalid_input("an A2A_AGENT_CARD agent spec requires card content")
                    );
                };
                let content_bytes = content.to_string().len();
                if content_bytes > MAX_SPEC_CONTENT_BYTES {
                    return Err(errors.invalid_input(format!(
                        "agent spec content must serialize to at most {MAX_SPEC_CONTENT_BYTES} bytes, got {content_bytes}",
                    )));
                }
            }
            AgentSpecType::NoSpec => {
                if registration.agent_spec.content.is_some() {
                    return Err(errors.invalid_input(
                        "agent spec content is only valid with an A2A_AGENT_CARD spec",
                    ));
                }
            }
            AgentSpecType::TypeUnspecified => {
                return Err(errors.invalid_input(
                    "agent spec type must be NO_SPEC or A2A_AGENT_CARD; construct it with AgentSpec::no_spec or AgentSpec::a2a_agent_card",
                ));
            }
        }
        Ok(())
    }

    async fn get_json<R: DeserializeOwned>(&self, path: &str, what: &str) -> Result<R> {
        let request = self.client.request(Method::GET, path).await?;
        let value = self.client.send_value(request).await?;
        self.parse(value, what)
    }

    fn parse<R: DeserializeOwned>(&self, value: Value, what: &str) -> Result<R> {
        serde_json::from_value(value).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            self.client
                .errors()
                .invalid_response(format!("failed to parse agent registry {what} JSON: {error}"))
        })
    }

    fn not_found(&self, message: String) -> AdkError {
        let errors = self.client.errors();
        errors.error(ErrorCategory::NotFound, errors.codes().not_found, message)
    }
}

// ===== Discovery tool =====

/// An [`adk_core::Tool`] that searches the Agent Registry for agents, MCP
/// servers, or endpoints.
///
/// Input arguments:
///
/// - `query` (string, required) — for agents and MCP servers, a registry
///   search expression; for endpoints, an AIP-160 list filter (endpoints
///   have no search), or empty to list all.
/// - `component_type` (string, optional) — `"agent"` (default),
///   `"mcp_server"`, or `"endpoint"`.
///
/// The output is a JSON array of
/// `{urn, displayName, description, skills, endpoint}` entries, where
/// `endpoint` is the entry's first interface URL and `skills` carries the
/// agent's skills, the MCP server's tools, or an empty array for endpoints.
///
/// The tool is read-only and concurrency-safe, so
/// [`ToolExecutionStrategy::Auto`](adk_core::ToolExecutionStrategy) may
/// dispatch it in parallel with other calls.
///
/// # Example
///
/// ```no_run
/// use adk_tool::vertex::agent_registry::{
///     AgentRegistryClient, AgentRegistryConfig, AgentSearchTool,
/// };
/// use std::sync::Arc;
///
/// # fn main() -> adk_core::Result<()> {
/// let client = AgentRegistryClient::new_with_adc(
///     AgentRegistryConfig::new("my-project", "global"),
/// )?;
/// let tool = AgentSearchTool::new(Arc::new(client));
/// # let _ = tool;
/// # Ok(())
/// # }
/// ```
pub struct AgentSearchTool {
    client: Arc<AgentRegistryClient>,
}

impl AgentSearchTool {
    /// Creates the tool over an existing registry client.
    pub fn new(client: Arc<AgentRegistryClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for AgentSearchTool {
    fn name(&self) -> &str {
        "search_agent_registry"
    }

    fn description(&self) -> &str {
        "Searches the Google Agent Registry for agents, MCP servers, or endpoints. \
         Returns a JSON array of {urn, displayName, description, skills, endpoint} \
         entries, where endpoint is the entry's callable URL."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "For agents and MCP servers: a search expression \
                        (bare words match word-contains, field=\"value\" matches exactly, \
                        NOT/AND/OR and parentheses combine terms, a trailing * matches \
                        prefixes). For endpoints: an AIP-160 list filter, or empty to \
                        list all endpoints.",
                },
                "component_type": {
                    "type": "string",
                    "enum": ["agent", "mcp_server", "endpoint"],
                    "description": "Which registry component to search. Defaults to 'agent'.",
                },
            },
            "required": ["query"],
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let errors = self.client.client.errors();
        let query = args.get("query").and_then(Value::as_str).ok_or_else(|| {
            errors.invalid_input("the 'query' argument is required and must be a string")
        })?;
        let component = args.get("component_type").and_then(Value::as_str).unwrap_or("agent");
        tracing::debug!(
            agent_registry.component = component,
            agent_registry.query = query,
            "executing agent registry discovery"
        );
        let entries = match component {
            "agent" => {
                let response =
                    self.client.search(SearchComponent::Agents, SearchRequest::new(query)).await?;
                response.agents.iter().map(agent_entry).collect()
            }
            "mcp_server" => {
                let response = self
                    .client
                    .search(SearchComponent::McpServers, SearchRequest::new(query))
                    .await?;
                response.mcp_servers.iter().map(mcp_server_entry).collect()
            }
            "endpoint" => {
                // Endpoints have no :search; the query rides as an AIP-160
                // list filter, and an empty query lists everything.
                let mut request = ListEndpointsRequest::new();
                if !query.trim().is_empty() {
                    request = request.with_filter(query);
                }
                let response = self.client.list_endpoints(request).await?;
                response.endpoints.iter().map(endpoint_entry).collect()
            }
            other => {
                return Err(errors.invalid_input(format!(
                    "unknown component_type '{other}'; expected 'agent', 'mcp_server', or 'endpoint'",
                )));
            }
        };
        Ok(Value::Array(entries))
    }
}

fn agent_entry(agent: &Agent) -> Value {
    json!({
        "urn": agent.agent_id.as_deref().unwrap_or(&agent.name),
        "displayName": &agent.display_name,
        "description": &agent.description,
        "skills": &agent.skills,
        "endpoint": agent.first_interface_url(),
    })
}

fn mcp_server_entry(server: &McpServer) -> Value {
    json!({
        "urn": server.mcp_server_id.as_deref().unwrap_or(&server.name),
        "displayName": &server.display_name,
        "description": &server.description,
        "skills": &server.tools,
        "endpoint": server.first_interface_url(),
    })
}

fn endpoint_entry(endpoint: &Endpoint) -> Value {
    json!({
        "urn": endpoint.endpoint_id.as_deref().unwrap_or(&endpoint.name),
        "displayName": &endpoint.display_name,
        "description": &endpoint.description,
        "skills": [],
        "endpoint": endpoint.first_interface_url(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spec_and_binding_enums_use_documented_wire_strings() {
        assert_eq!(
            serde_json::to_value(AgentSpec::a2a_agent_card(json!({"name": "a"}))).unwrap(),
            json!({ "type": "A2A_AGENT_CARD", "content": { "name": "a" } }),
        );
        assert_eq!(
            serde_json::to_value(AgentSpec::no_spec()).unwrap(),
            json!({ "type": "NO_SPEC" }),
        );
        let bindings = [
            (ProtocolBinding::Jsonrpc, "JSONRPC"),
            (ProtocolBinding::Grpc, "GRPC"),
            (ProtocolBinding::HttpJson, "HTTP_JSON"),
        ];
        for (binding, wire) in bindings {
            assert_eq!(serde_json::to_value(binding).unwrap(), json!(wire));
        }
        // Unknown wire values must deserialize to the fallback, not fail.
        let unknown: ProtocolBinding = serde_json::from_value(json!("FUTURE_BINDING")).unwrap();
        assert_eq!(unknown, ProtocolBinding::ProtocolBindingUnspecified);
    }

    #[test]
    fn test_config_defaults_to_the_single_global_endpoint() {
        let config = AgentRegistryConfig::new("p", "global");
        assert_eq!(config.endpoint(), "https://agentregistry.googleapis.com");
        assert_eq!(
            config.with_endpoint("registry.example.com").endpoint(),
            "https://registry.example.com",
        );
    }

    // async: the credentials builder requires an ambient tokio runtime.
    #[tokio::test]
    async fn test_registration_constraints_are_rejected_before_transport() {
        let credentials =
            google_cloud_auth::credentials::api_key_credentials::Builder::new("k").build();
        let client = AgentRegistryClient::with_credentials(
            AgentRegistryConfig::new("p", "global"),
            credentials,
        )
        .unwrap();

        let card = AgentSpec::a2a_agent_card(json!({ "name": "a" }));
        let cases = [
            (ServiceRegistration::new("abc", "Agent", card.clone()), "4-63 characters"),
            (ServiceRegistration::new("abcd", "d".repeat(64), card.clone()), "display name"),
            (
                ServiceRegistration::new("abcd", "Agent", card.clone())
                    .with_description("d".repeat(2049)),
                "description",
            ),
            (
                ServiceRegistration::new("abcd", "Agent", card.clone())
                    .with_interfaces(vec![Interface::new("https://a.example.com")]),
                "interfaces must be empty",
            ),
            (
                ServiceRegistration::new(
                    "abcd",
                    "Agent",
                    AgentSpec::a2a_agent_card(json!({ "pad": "x".repeat(11 * 1024) })),
                ),
                "10240 bytes",
            ),
            (
                ServiceRegistration::new(
                    "abcd",
                    "Agent",
                    AgentSpec { spec_type: AgentSpecType::NoSpec, content: Some(json!({})) },
                ),
                "only valid with an A2A_AGENT_CARD",
            ),
        ];
        for (registration, expected) in cases {
            let error = client.validate_registration(&registration).unwrap_err();
            assert!(
                error.message.contains(expected),
                "expected '{expected}' in: {}",
                error.message,
            );
        }
    }

    #[test]
    fn test_first_interface_url_walks_agent_protocols() {
        let agent = Agent {
            protocols: vec![
                AgentProtocol::default(),
                AgentProtocol {
                    interfaces: vec![Interface::new("https://a.example.com/a2a")],
                    ..AgentProtocol::default()
                },
            ],
            ..Agent::default()
        };
        assert_eq!(agent.first_interface_url(), Some("https://a.example.com/a2a"));
        assert_eq!(Agent::default().first_interface_url(), None);
    }
}
