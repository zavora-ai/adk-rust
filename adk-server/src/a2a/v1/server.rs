//! Serving an agent over A2A v1 in one call.
//!
//! [`A2aV1Server`] assembles everything an A2A v1 endpoint needs: the task store, the
//! executor, the [`RequestHandler`], and an agent card that lists the interfaces it
//! mounts. It serves:
//!
//! - the agent card at [`AGENT_CARD_PATH`], with `ETag` and `Last-Modified`;
//! - the JSON-RPC binding (A2A §9) at [`DEFAULT_JSONRPC_PATH`];
//! - the HTTP+JSON binding (A2A §11) under [`DEFAULT_REST_PATH`];
//!
//! with `A2A-Version` negotiation in front of both bindings.
//!
//! [`ServerBuilder::with_a2a`](crate::ServerBuilder::with_a2a) mounts this server. Use the
//! builder directly to serve an agent on its own router.

use std::sync::Arc;

use a2a_protocol_types::extensions::AgentExtension;
use a2a_protocol_types::{AgentCapabilities, AgentInterface, AgentSkill};
use axum::Router;
use axum::routing::{get, post};
use tokio::sync::RwLock;

use super::card::{CachedAgentCard, build_v1_agent_card, serve_agent_card, v1_agent_skills};
use super::convert::ADK_EXTENSION_URI;
use super::executor::V1Executor;
use super::jsonrpc_handler::jsonrpc_handler;
use super::push::{NoOpPushNotificationSender, PushNotificationSender};
use super::request_handler::RequestHandler;
use super::rest_handler::rest_router;
use super::task_store::{InMemoryTaskStore, TaskStore};
use super::version::version_negotiation;

/// Where the agent card is served (A2A §8.2).
pub const AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

/// Where [`A2aV1Server`] mounts the JSON-RPC binding unless told otherwise.
pub const DEFAULT_JSONRPC_PATH: &str = "/a2a/v1";

/// Where [`A2aV1Server`] mounts the HTTP+JSON binding unless told otherwise.
pub const DEFAULT_REST_PATH: &str = "/a2a/v1/rest";

/// An agent served over A2A v1.
///
/// # Example
///
/// ```rust,ignore
/// use adk_server::a2a::v1::server::A2aV1Server;
///
/// let runner_config = Arc::new(
///     Runner::builder()
///         .app_name("weather")
///         .agent(agent)
///         .session_service(Arc::new(InMemorySessionService::new()))
///         .build_config(),
/// );
/// let server = A2aV1Server::builder(runner_config, "http://localhost:8080").build();
/// let app = server.router();
/// ```
pub struct A2aV1Server {
    handler: Arc<RequestHandler>,
    card: Arc<RwLock<CachedAgentCard>>,
    jsonrpc_path: String,
    rest_path: String,
}

impl A2aV1Server {
    /// Starts a server for the agent `runner_config` runs, reachable at `base_url`
    /// (`http://host:port`, no trailing path): the card names its interfaces under it.
    pub fn builder(
        runner_config: Arc<adk_runner::RunnerConfig>,
        base_url: impl Into<String>,
    ) -> A2aV1ServerBuilder {
        A2aV1ServerBuilder {
            runner_config,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            jsonrpc_path: DEFAULT_JSONRPC_PATH.to_string(),
            rest_path: DEFAULT_REST_PATH.to_string(),
            task_store: None,
            push_sender: None,
            name: None,
            description: None,
            version: "1.0.0".to_string(),
            streaming: true,
            extra_skills: Vec::new(),
            legacy_jsonrpc_url: None,
        }
    }

    /// The request handler behind both bindings.
    pub fn handler(&self) -> &Arc<RequestHandler> {
        &self.handler
    }

    /// The served agent card. Updating it changes what the card route serves.
    pub fn card(&self) -> &Arc<RwLock<CachedAgentCard>> {
        &self.card
    }

    /// The agent card route. Discovery needs no credentials, so this router takes no
    /// authentication.
    pub fn card_router(&self) -> Router {
        Router::new().route(AGENT_CARD_PATH, get(serve_agent_card)).with_state(self.card.clone())
    }

    /// The JSON-RPC and HTTP+JSON bindings, behind `A2A-Version` negotiation. Put
    /// authentication on this router: the handlers act for the caller it identifies.
    pub fn rpc_router(&self) -> Router {
        Router::new()
            .route(&self.jsonrpc_path, post(jsonrpc_handler))
            .with_state(self.handler.clone())
            .nest(&self.rest_path, rest_router(self.handler.clone()))
            .layer(axum::middleware::from_fn(version_negotiation))
    }

    /// The card route and both bindings, without authentication.
    pub fn router(&self) -> Router {
        self.card_router().merge(self.rpc_router())
    }
}

/// Configures an [`A2aV1Server`]; created by [`A2aV1Server::builder`].
pub struct A2aV1ServerBuilder {
    runner_config: Arc<adk_runner::RunnerConfig>,
    base_url: String,
    jsonrpc_path: String,
    rest_path: String,
    task_store: Option<Arc<dyn TaskStore>>,
    push_sender: Option<Arc<dyn PushNotificationSender>>,
    name: Option<String>,
    description: Option<String>,
    version: String,
    streaming: bool,
    extra_skills: Vec<AgentSkill>,
    legacy_jsonrpc_url: Option<String>,
}

impl A2aV1ServerBuilder {
    /// Mounts the JSON-RPC binding at `path` instead of [`DEFAULT_JSONRPC_PATH`].
    pub fn jsonrpc_path(mut self, path: impl Into<String>) -> Self {
        self.jsonrpc_path = path.into();
        self
    }

    /// Mounts the HTTP+JSON binding under `path` instead of [`DEFAULT_REST_PATH`].
    pub fn rest_path(mut self, path: impl Into<String>) -> Self {
        self.rest_path = path.into();
        self
    }

    /// Keeps tasks in `store` instead of a new [`InMemoryTaskStore`].
    pub fn task_store(mut self, store: Arc<dyn TaskStore>) -> Self {
        self.task_store = Some(store);
        self
    }

    /// Sends push notifications with `sender`. Without one, no notification is sent.
    pub fn push_sender(mut self, sender: Arc<dyn PushNotificationSender>) -> Self {
        self.push_sender = Some(sender);
        self
    }

    /// The card's name; the agent's name by default.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// The card's description; the agent's description by default.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// The card's agent version; `1.0.0` by default.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Whether the card declares streaming; `true` by default.
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Adds skills to those derived from the agent.
    pub fn skills(mut self, skills: impl IntoIterator<Item = AgentSkill>) -> Self {
        self.extra_skills.extend(skills);
        self
    }

    /// Also lists an A2A 0.3 JSON-RPC endpoint at `url` as the card's last interface, with
    /// `protocolVersion` `0.3`, so a client that reads the v1 card finds both versions. A v1
    /// card has no top-level `url`, so a 0.3 client still needs a 0.3 card of its own,
    /// which [`ServerBuilder::with_a2a`](crate::ServerBuilder::with_a2a) serves at
    /// `/.well-known/agent.json`.
    pub fn legacy_jsonrpc_url(mut self, url: impl Into<String>) -> Self {
        self.legacy_jsonrpc_url = Some(url.into());
        self
    }

    /// Builds the server.
    pub fn build(self) -> A2aV1Server {
        let agent = self.runner_config.agent.clone();
        let jsonrpc_url = format!("{}{}", self.base_url, self.jsonrpc_path);
        let rest_url = format!("{}{}", self.base_url, self.rest_path);

        let mut skills = v1_agent_skills(agent.as_ref());
        skills.extend(self.extra_skills);
        // Every capability is stated, so the handlers refuse what the card does not offer.
        let mut capabilities = AgentCapabilities::none()
            .with_streaming(self.streaming)
            .with_push_notifications(self.push_sender.is_some())
            .with_extended_agent_card(false);
        capabilities.extensions = Some(vec![AgentExtension {
            uri: ADK_EXTENSION_URI.to_string(),
            description: Some(
                "Tasks carry ADK metadata: adk_app_name, adk_user_id, adk_session_id.".to_string(),
            ),
            required: Some(false),
            params: None,
        }]);
        let mut card = build_v1_agent_card(
            self.name.as_deref().unwrap_or_else(|| agent.name()),
            self.description.as_deref().unwrap_or_else(|| agent.description()),
            &jsonrpc_url,
            &self.version,
            skills,
            capabilities,
        );
        card.supported_interfaces.push(AgentInterface {
            url: rest_url,
            protocol_binding: "HTTP+JSON".to_string(),
            protocol_version: "1.0".to_string(),
            tenant: None,
        });
        if let Some(legacy_url) = self.legacy_jsonrpc_url {
            card.supported_interfaces.push(AgentInterface {
                url: legacy_url,
                protocol_binding: "JSONRPC".to_string(),
                protocol_version: "0.3".to_string(),
                tenant: None,
            });
        }

        let card = Arc::new(RwLock::new(CachedAgentCard::new(card)));
        let task_store: Arc<dyn TaskStore> =
            self.task_store.unwrap_or_else(|| Arc::new(InMemoryTaskStore::new()));
        let push_sender: Arc<dyn PushNotificationSender> =
            self.push_sender.unwrap_or_else(|| Arc::new(NoOpPushNotificationSender));
        let handler = Arc::new(RequestHandler::with_runner(
            Arc::new(V1Executor::new(task_store.clone())),
            task_store,
            push_sender,
            card.clone(),
            self.runner_config,
        ));
        A2aV1Server { handler, card, jsonrpc_path: self.jsonrpc_path, rest_path: self.rest_path }
    }
}
