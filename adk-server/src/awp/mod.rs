//! The AWP 2.0 agent edge (spec 3.5 §7.3, §7.4, §7.9, §7.10; feature `awp`).
//!
//! [`AwpEdge`] serves a site's capabilities to people and agents and hosts its public
//! partition. Mount it with [`ServerBuilder::awp`](crate::ServerBuilder::awp), which merges it
//! through `add_root_routes`, so the server's host guard, CORS, timeouts and security headers
//! apply, and attaches the server's kill switch.
//!
//! | Route | Purpose | Module |
//! |---|---|---|
//! | `GET /.well-known/agent-card.json` | The signed A2A card, one skill per capability the caller sees | [`a2a`] |
//! | `POST /awp/v2/a2a`, `/awp/v2/a2a/v1/…` | A2A v1.0 JSON-RPC and HTTP+JSON: capabilities and conversation | [`a2a`] |
//! | `/mcp`, `/mcp/server-card`, `/.well-known/mcp/server-card.json` | The capabilities as MCP tools | [`mcp`] |
//! | `GET /awp/v2/manifest`, `GET /awp/v2/status`, `GET /awp/v2/trust` | Contract surfaces | `adk_awp::v2::routes` |
//! | `POST /awp/v2/capabilities/{name}`, `GET /awp/v2/calls/{id}` | Invocation and held-call status | `adk_awp::v2::routes`, [`executor`] |
//! | `GET /awp/v2/chat` (WebSocket) | The on-site assistant | [`chat`] |
//!
//! Every request first passes the edge's guard: the edge's mode, the caller's tier from its
//! verified credentials ([`tiers`]), and its rate limit ([`limits`]). All three capability
//! bindings enter one governed path ([`executor`]) under a policy compiled from the manifest
//! ([`policy`]). The edge exposes no owner route: the owner's control plane is the kernel's.
//!
//! A verified caller's fetch of the Agent Card, the capability manifest or the MCP Server
//! Card is recorded as `awp.request.verified` with its `awp.surface`, so verified agent
//! discovery is counted (spec 3.5 §7.12, §9.3).
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_awp::signing::jws::Ed25519DocumentSigner;
//! use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry};
//! use adk_core::Url;
//! use adk_server::awp::{AwpEdge, EdgeKeys};
//! use awp_types::{CapabilityEffect, CapabilityV2};
//! use serde_json::{Value, json};
//!
//! struct Hours;
//!
//! #[async_trait::async_trait]
//! impl CapabilityHandler for Hours {
//!     async fn handle(&self, _call: CapabilityCall) -> Result<Value, CapabilityError> {
//!         Ok(json!({ "open": "08:00", "close": "17:00" }))
//!     }
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut registry = CapabilityRegistry::new("example.com");
//! registry.declare(CapabilityV2::new("opening_hours", "Opening hours", CapabilityEffect::ReadOnly), Arc::new(Hours))?;
//! let keys = EdgeKeys {
//!     card: Arc::new(Ed25519DocumentSigner::generate("edge-card-2026-10")?),
//!     receipt: Arc::new(Ed25519DocumentSigner::generate("edge-receipt-2026-10")?),
//! };
//! let edge = AwpEdge::builder(registry, keys, Url::parse("https://agent.example.com")?).build()?;
//! let app: axum::Router = edge.router();
//! # let _ = app;
//! # Ok(())
//! # }
//! ```

pub mod a2a;
pub mod beacon;
pub mod chat;
pub mod conformance;
pub mod consent;
pub mod control;
pub mod events;
pub mod executor;
pub mod limits;
pub mod mcp;
pub mod policy;
pub mod public;
#[cfg(any(feature = "awp-sqlite", feature = "awp-postgres", feature = "awp-redis"))]
pub mod stores;
pub mod tiers;

use std::sync::{Arc, Weak};
use std::time::Duration;

use adk_awp::signing::jws::DocumentSigner;
use adk_awp::v2::capability::CapabilityRegistry;
use adk_awp::v2::response::envelope_response;
use adk_awp::v2::routes::{EdgeState, awp_v2_routes};
use adk_awp::v2::store::{
    ConsentReceiptStore, HeldCallStore, IdempotencyStore, LeadStore, MemoryEdgeStores, StatusStore,
    StoreError, SubscriptionStore,
};
use adk_awp::v2::tier::TierAssigner;
use adk_core::audit_log::AuditTrail;
use adk_core::{
    ActionLedger, AdkError, Agent, AgentId, AgentPrincipal, ApprovalService, ApprovalStore,
    ErrorCategory, ErrorComponent, GovernanceControl, InMemoryActionLedger, InMemoryApprovalStore,
    Url,
};
use adk_session::{InMemorySessionService, SessionService};
use adk_tool::mcp::server::McpSessions;
use awp_types::{
    CapabilityEffect, ErrorBody, ErrorCode, ErrorEnvelope, HeldCall, SiteStatus, TrustRules,
    TrustTier,
};
use axum::Router;
use axum::extract::{FromRequest as _, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tracing::Instrument as _;

pub use a2a::AgentCardInfo;
pub use beacon::{BeaconCounts, BeaconStore, InMemoryBeaconStore};
pub use consent::{ConsentConfig, ConsentPolicy, LeadsData, SubjectData};
pub use control::{EdgeMode, StatusError};
pub use events::{
    EdgeEvents, EventLog, EventsConfig, InMemoryEventLog, PendingDelivery, RetainedEvent,
};
pub use executor::{GovernedExecutor, KeySources};
pub use limits::{EdgeLimits, InMemoryRateLimitStore, RateLimitStore, RateWindow};
pub use policy::{CapabilityPolicy, PublicPartitionPolicy};
pub use public::{
    ConversationScreen, InjectionScreen, META_BINDING, META_LIMIT_KEY, META_THREAD, PublicTools,
    RecordingSignals, RecordingThreadSink, ReplyPolicy, SignalEmitter, SiteHit, SiteSearch,
    StaticSiteIndex, ThreadSink,
};
pub use tiers::{EdgeCaller, EdgeTiers, WebBotAuthTrustAssigner};

use control::EdgeControl;
use executor::CURRENT_CALLER;
use public::{LiveThreads, PublicPartition, PublicParts, TurnWatch, Watch};

/// The edge's delegated signing keys (spec 3.5 §7.2, D-11; AWP 2.0 §5.3, C3).
///
/// The root key never reaches the edge; these are short-lived keys the root delegated.
#[derive(Clone)]
pub struct EdgeKeys {
    /// Signs the A2A Agent Card (scope `card`).
    pub card: Arc<dyn DocumentSigner>,
    /// Signs the site status, consent receipts and stream entries (scope `receipt`).
    pub receipt: Arc<dyn DocumentSigner>,
}

impl std::fmt::Debug for EdgeKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeKeys")
            .field("card", &self.card.kid())
            .field("receipt", &self.receipt.kid())
            .finish()
    }
}

/// The edge's durable stores (spec 3.5 §7.9).
#[derive(Clone)]
#[non_exhaustive]
pub struct EdgeStores {
    /// The current signed status.
    pub status: Arc<dyn StatusStore>,
    /// Calls held for the owner.
    pub held_calls: Arc<dyn HeldCallStore>,
    /// Outcomes of keyed calls, replayed in full.
    pub idempotency: Arc<dyn IdempotencyStore>,
    /// Consent receipts.
    pub consent: Arc<dyn ConsentReceiptStore>,
    /// Event subscriptions.
    pub subscriptions: Arc<dyn SubscriptionStore>,
    /// Leads captured at the edge.
    pub leads: Arc<dyn LeadStore>,
    /// Events retained for replay, catalogue pages, consent references and pending
    /// deliveries; the builder's [`event_log`](AwpEdgeBuilder::event_log), else in memory.
    pub event_log: Option<Arc<dyn EventLog>>,
    /// The beacon's daily counts; the builder's
    /// [`beacon_store`](AwpEdgeBuilder::beacon_store), else in memory.
    pub beacon: Option<Arc<dyn BeaconStore>>,
}

impl EdgeStores {
    /// Stores over the given backends.
    pub fn new(
        status: Arc<dyn StatusStore>,
        held_calls: Arc<dyn HeldCallStore>,
        idempotency: Arc<dyn IdempotencyStore>,
        consent: Arc<dyn ConsentReceiptStore>,
    ) -> Self {
        let memory = MemoryEdgeStores::default();
        Self {
            status,
            held_calls,
            idempotency,
            consent,
            subscriptions: Arc::new(memory.subscriptions),
            leads: Arc::new(memory.leads),
            event_log: None,
            beacon: None,
        }
    }

    /// Keeps event subscriptions in `subscriptions`; in memory by default.
    #[must_use]
    pub fn with_subscriptions(mut self, subscriptions: Arc<dyn SubscriptionStore>) -> Self {
        self.subscriptions = subscriptions;
        self
    }

    /// Keeps leads in `leads`; in memory by default.
    #[must_use]
    pub fn with_leads(mut self, leads: Arc<dyn LeadStore>) -> Self {
        self.leads = leads;
        self
    }

    /// Retains events, catalogue pages, consent references and pending deliveries in `log`,
    /// so they survive a restart (spec 3.5 §7.5).
    #[must_use]
    pub fn with_event_log(mut self, log: Arc<dyn EventLog>) -> Self {
        self.event_log = Some(log);
        self
    }

    /// Keeps the beacon's daily counts in `beacon` (spec 3.5 §7.12).
    #[must_use]
    pub fn with_beacon(mut self, beacon: Arc<dyn BeaconStore>) -> Self {
        self.beacon = Some(beacon);
        self
    }

    /// The in-memory `stores`, shared with the caller's handle.
    pub fn in_memory(stores: &MemoryEdgeStores) -> Self {
        Self::new(
            Arc::new(stores.status.clone()),
            Arc::new(stores.held_calls.clone()),
            Arc::new(stores.idempotency.clone()),
            Arc::new(stores.consent.clone()),
        )
        .with_subscriptions(Arc::new(stores.subscriptions.clone()))
        .with_leads(Arc::new(stores.leads.clone()))
    }
}

impl std::fmt::Debug for EdgeStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeStores").finish_non_exhaustive()
    }
}

/// Answers an AWP error envelope with the `AWP-Version` field.
pub(crate) fn envelope(error: ErrorBody, status: Option<SiteStatus>) -> Response {
    let mut response =
        envelope_response(&ErrorEnvelope { awp: awp_types::VERSION_2_0, error, status });
    response.headers_mut().insert("awp-version", HeaderValue::from_static("2.0"));
    response
}

type AgentFactory = Box<dyn FnOnce(&PublicTools) -> Arc<dyn Agent> + Send>;

/// Builds an [`AwpEdge`].
pub struct AwpEdgeBuilder {
    registry: CapabilityRegistry,
    keys: EdgeKeys,
    public_url: Url,
    tiers: EdgeTiers,
    limits: EdgeLimits,
    stores: Option<EdgeStores>,
    approval_service: Option<Arc<dyn ApprovalService>>,
    approval_store: Option<Arc<dyn ApprovalStore>>,
    ledger: Option<Arc<dyn ActionLedger>>,
    audit: Option<AuditTrail>,
    principal: Option<AgentPrincipal>,
    governance: Option<GovernanceControl>,
    hold_expiry: Option<chrono::Duration>,
    card: Option<AgentCardInfo>,
    agent: Option<AgentFactory>,
    search: Option<Arc<dyn SiteSearch>>,
    threads: Option<Arc<dyn ThreadSink>>,
    signals: Option<Arc<dyn SignalEmitter>>,
    screen: Option<Arc<dyn ConversationScreen>>,
    reply_policy: ReplyPolicy,
    sessions: Option<Arc<dyn SessionService>>,
    mcp_sessions: McpSessions,
    events: EventsConfig,
    event_log: Option<Arc<dyn EventLog>>,
    consent: ConsentConfig,
    subject_data: Option<Arc<dyn SubjectData>>,
    beacon: Option<Arc<dyn BeaconStore>>,
}

impl AwpEdgeBuilder {
    /// Sets how callers earn tiers; the default makes every caller `public` (A-I6).
    #[must_use]
    pub fn tiers(mut self, tiers: EdgeTiers) -> Self {
        self.tiers = tiers;
        self
    }

    /// Sets the rate limits; rate classes the tiers define are added to them.
    #[must_use]
    pub fn limits(mut self, limits: EdgeLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets the edge stores; the default keeps them in memory.
    #[must_use]
    pub fn stores(mut self, stores: EdgeStores) -> Self {
        self.stores = Some(stores);
        self
    }

    /// Sends held calls to the owner's approval service (spec 3.3); its store is where the
    /// owner's decisions are read from.
    #[must_use]
    pub fn approval_service(mut self, service: Arc<dyn ApprovalService>) -> Self {
        self.approval_service = Some(service);
        self
    }

    /// Sets the approval store held calls wait in, when no approval service provides one.
    #[must_use]
    pub fn approval_store(mut self, store: Arc<dyn ApprovalStore>) -> Self {
        self.approval_store = Some(store);
        self
    }

    /// Sets the action ledger `non_idempotent` calls are recorded in; in memory by default.
    #[must_use]
    pub fn action_ledger(mut self, ledger: Arc<dyn ActionLedger>) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// Records governed decisions, `awp.capability.*` records and verified discovery
    /// (`awp.request.verified`) on `audit`.
    #[must_use]
    pub fn audit(mut self, audit: AuditTrail) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Sets the site's own principal, the acting agent of every call; by default
    /// `urn:adk:agent:{domain}/awp-edge`.
    #[must_use]
    pub fn principal(mut self, principal: AgentPrincipal) -> Self {
        self.principal = Some(principal);
        self
    }

    /// Attaches the owner's kill switch; [`ServerBuilder::awp`](crate::ServerBuilder::awp)
    /// attaches the server's when none is set.
    #[must_use]
    pub fn governance(mut self, control: GovernanceControl) -> Self {
        self.governance = Some(control);
        self
    }

    /// Gives every held call an `expiresAt` this long after it is held; without it a call
    /// waits as long as the owner takes.
    #[must_use]
    pub fn hold_expiry(mut self, expiry: chrono::Duration) -> Self {
        self.hold_expiry = Some(expiry);
        self
    }

    /// Sets the Agent Card's name, description and version.
    #[must_use]
    pub fn agent_card(mut self, card: AgentCardInfo) -> Self {
        self.card = Some(card);
        self
    }

    /// Hosts the site's public agent, built from the tools the edge gives it (spec 3.5 §7.10).
    ///
    /// The agent answers A2A messages that name no capability and `GET /awp/v2/chat`, under
    /// [`PublicPartitionPolicy`]: whatever tools it holds, only [`PublicTools`] run.
    #[must_use]
    pub fn public_agent(
        mut self,
        factory: impl FnOnce(&PublicTools) -> Arc<dyn Agent> + Send + 'static,
    ) -> Self {
        self.agent = Some(Box::new(factory));
        self
    }

    /// Sets the search behind `search_site`; an empty index by default.
    #[must_use]
    pub fn site_search(mut self, search: Arc<dyn SiteSearch>) -> Self {
        self.search = Some(search);
        self
    }

    /// Sets where `reply_in_thread` delivers replies outside the edge's own chat sockets.
    #[must_use]
    pub fn threads(mut self, threads: Arc<dyn ThreadSink>) -> Self {
        self.threads = Some(threads);
        self
    }

    /// Sets where `emit_signal` files signals; the kernel's `SignalSink` in `adk-website`.
    #[must_use]
    pub fn signals(mut self, signals: Arc<dyn SignalEmitter>) -> Self {
        self.signals = Some(signals);
        self
    }

    /// Sets the screen conversation input passes before the public agent reads it; a flagged
    /// turn the agent files no signal for gets one from the edge (spec 3.5 §7.10, S7).
    /// [`InjectionScreen`] by default.
    #[must_use]
    pub fn conversation_screen(mut self, screen: Arc<dyn ConversationScreen>) -> Self {
        self.screen = Some(screen);
        self
    }

    /// Sets the template and pace of `reply_in_thread`.
    #[must_use]
    pub fn reply_policy(mut self, policy: ReplyPolicy) -> Self {
        self.reply_policy = policy;
        self
    }

    /// Sets the session service public conversations are kept in; in memory by default.
    #[must_use]
    pub fn sessions(mut self, sessions: Arc<dyn SessionService>) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// Sets how the MCP endpoint keeps sessions; stateful by default.
    #[must_use]
    pub fn mcp_sessions(mut self, sessions: McpSessions) -> Self {
        self.mcp_sessions = sessions;
        self
    }

    /// Sets the event catalogue, retries, retention and egress policy (spec 3.5 §7.5).
    #[must_use]
    pub fn events(mut self, events: EventsConfig) -> Self {
        self.events = events;
        self
    }

    /// Sets where events are retained for replay; the stores' event log, else in memory.
    #[must_use]
    pub fn event_log(mut self, log: Arc<dyn EventLog>) -> Self {
        self.event_log = Some(log);
        self
    }

    /// Sets the consent scopes, policy versions and the subject session key (spec 3.5 §7.5).
    #[must_use]
    pub fn consent(mut self, consent: ConsentConfig) -> Self {
        self.consent = consent;
        self
    }

    /// Sets the data `GET /awp/v2/consent/export` and `POST /awp/v2/consent/erase` cover;
    /// by default the leads referencing the subject's receipts ([`LeadsData`]).
    #[must_use]
    pub fn subject_data(mut self, data: Arc<dyn SubjectData>) -> Self {
        self.subject_data = Some(data);
        self
    }

    /// Sets where the beacon counts; the stores' beacon store, else in memory.
    #[must_use]
    pub fn beacon_store(mut self, store: Arc<dyn BeaconStore>) -> Self {
        self.beacon = Some(store);
        self
    }

    /// Builds the edge.
    ///
    /// # Errors
    ///
    /// An `AdkError` when the public URL is not an `http` or `https` origin, the principal's id
    /// is invalid, or the MCP server or the public agent's runner cannot be built.
    pub fn build(self) -> Result<AwpEdge, AdkError> {
        let invalid = |code: &'static str, message: String| {
            AdkError::new(ErrorComponent::Server, ErrorCategory::InvalidInput, code, message)
        };
        if !matches!(self.public_url.scheme(), "http" | "https") {
            return Err(invalid(
                "awp.edge.url",
                "the edge's public URL must be http or https".to_owned(),
            ));
        }
        let domain = self.registry.domain().to_owned();
        let registry = Arc::new(self.registry);
        let stores =
            self.stores.unwrap_or_else(|| EdgeStores::in_memory(&MemoryEdgeStores::default()));
        let control =
            Arc::new(EdgeControl::new(Arc::clone(&stores.status), Arc::clone(&self.keys.receipt)));
        if let Some(governance) = self.governance {
            control.attach_governance(governance);
        }
        let events = EdgeEvents::new(
            Arc::clone(&stores.subscriptions),
            self.event_log
                .or_else(|| stores.event_log.clone())
                .unwrap_or_else(|| Arc::new(InMemoryEventLog::new())),
            self.events,
            self.public_url.as_str(),
        );
        control.attach_events(events.clone());
        let references = events.references();
        let principal = match self.principal {
            Some(principal) => principal,
            None => AgentPrincipal::new(
                AgentId::new(format!("urn:adk:agent:{domain}/awp-edge"))
                    .map_err(|error| invalid("awp.edge.principal", error.to_string()))?,
            ),
        };
        let approval_store = self
            .approval_service
            .as_ref()
            .and_then(|service| service.approval_store())
            .or(self.approval_store)
            .unwrap_or_else(|| Arc::new(InMemoryApprovalStore::new()));
        let audit = self.audit;
        let tiers = Arc::new(self.tiers);
        let limits = Arc::new(self.limits.classes(tiers.rules().rate_classes.clone()));
        let policy =
            Arc::new(CapabilityPolicy::new(Arc::clone(&registry), Arc::clone(&stores.consent)));
        let executor = GovernedExecutor::from_inner(executor::Inner {
            registry: Arc::clone(&registry),
            held_calls: Arc::clone(&stores.held_calls),
            idempotency: Arc::clone(&stores.idempotency),
            consent: Arc::clone(&stores.consent),
            approval_service: self.approval_service,
            approval_store,
            ledger: self.ledger.unwrap_or_else(|| Arc::new(InMemoryActionLedger::new())),
            audit: audit.clone(),
            principal: principal.clone(),
            policy,
            control: Arc::clone(&control),
            limits: Arc::clone(&limits),
            hold_expiry: self.hold_expiry,
            references: references.clone(),
        });
        let card = self.card.unwrap_or_else(|| AgentCardInfo {
            name: domain.clone(),
            description: format!("The agent edge of {domain}"),
            version: "2.0".to_owned(),
        });

        let live = LiveThreads::default();
        let public = match self.agent {
            None => None,
            Some(factory) => {
                let signals: Arc<dyn SignalEmitter> =
                    self.signals.unwrap_or_else(|| Arc::new(RecordingSignals::new()));
                let turns = TurnWatch::default();
                let tools = public::public_tools(
                    &executor,
                    PublicParts {
                        search: self.search.unwrap_or_else(|| Arc::new(StaticSiteIndex::new())),
                        threads: self
                            .threads
                            .unwrap_or_else(|| Arc::new(RecordingThreadSink::new())),
                        signals: Arc::clone(&signals),
                        reply_policy: self.reply_policy,
                        emitted_by: format!("{}#public", principal.id.as_str()),
                        watch: turns.clone(),
                    },
                    &live,
                );
                let agent = factory(&tools);
                let sessions =
                    self.sessions.unwrap_or_else(|| Arc::new(InMemorySessionService::new()));
                let watch = Watch {
                    turns,
                    screen: self.screen.unwrap_or_else(|| Arc::new(InjectionScreen)),
                    signals,
                    edge: principal.id.as_str().to_owned(),
                };
                Some(Arc::new(PublicPartition::new(
                    agent,
                    &tools,
                    sessions,
                    "awp-public",
                    audit.as_ref(),
                    watch,
                )?))
            }
        };

        let v2 = awp_v2_routes(
            EdgeState::builder((*registry).clone())
                .executor(Arc::new(executor.clone()))
                .tiers(Arc::clone(&tiers) as Arc<dyn TierAssigner>)
                .stores(Arc::clone(&stores.status), Arc::clone(&stores.held_calls))
                .trust_rules(tiers.published_rules().clone())
                .build(),
        );
        let a2a = Arc::new(a2a::A2aBinding::new(
            executor.clone(),
            public.clone(),
            live.clone(),
            Arc::clone(&control),
            card.clone(),
            self.public_url.as_str(),
            Arc::clone(&self.keys.card),
        ));
        // SEP-2127 names a server in reverse DNS with one '/': `com.example/awp`.
        let mcp_name = format!("{}/awp", domain.split('.').rev().collect::<Vec<_>>().join("."));
        let mcp = mcp::router(
            &executor,
            Arc::clone(&control),
            mcp::McpInfo {
                name: &mcp_name,
                description: &card.description,
                version: &card.version,
            },
            &self.public_url,
            self.mcp_sessions,
        )?;
        let consent = consent::ConsentState {
            tokens: consent::SubjectTokens::for_config(&self.consent),
            config: Arc::new(self.consent),
            store: Arc::clone(&stores.consent),
            held_calls: Arc::clone(&stores.held_calls),
            references,
            events: events.clone(),
            signer: Arc::clone(&self.keys.receipt),
            data: self
                .subject_data
                .unwrap_or_else(|| Arc::new(LeadsData::new(Arc::clone(&stores.leads)))),
            audit: audit.clone(),
        };
        let beacon_store = self
            .beacon
            .or_else(|| stores.beacon.clone())
            .unwrap_or_else(|| Arc::new(InMemoryBeaconStore::new()));
        let beacon = beacon::BeaconState {
            store: Arc::clone(&beacon_store),
            consent: Arc::clone(&stores.consent),
        };
        let mut router = Router::new()
            .merge(v2)
            .merge(a2a.router())
            .merge(mcp)
            .merge(events.router())
            .merge(consent.router())
            .merge(beacon.router());
        if let Some(public) = &public {
            router = router.route(
                chat::CHAT_PATH,
                get(chat::chat).with_state(chat::ChatState {
                    public: Arc::clone(public),
                    live: live.clone(),
                    control: Arc::clone(&control),
                }),
            );
        }
        let guard = Arc::new(Guard {
            tiers: Arc::clone(&tiers),
            limits,
            control: Arc::clone(&control),
            executor: executor.clone(),
        });
        let router = router.layer(from_fn_with_state(guard, edge_guard));
        Ok(AwpEdge {
            inner: Arc::new(EdgeInner {
                executor,
                control,
                tiers,
                router,
                events,
                beacon: beacon_store,
            }),
        })
    }
}

struct EdgeInner {
    executor: GovernedExecutor,
    control: Arc<EdgeControl>,
    tiers: Arc<EdgeTiers>,
    router: Router,
    events: EdgeEvents,
    beacon: Arc<dyn BeaconStore>,
}

/// The AWP 2.0 agent edge of one site (spec 3.5 §7.9).
///
/// Cheap to clone; clones share the edge.
#[derive(Clone)]
pub struct AwpEdge {
    inner: Arc<EdgeInner>,
}

impl std::fmt::Debug for AwpEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwpEdge")
            .field("domain", &self.inner.executor.registry().domain())
            .field("mode", &self.inner.control.mode())
            .finish_non_exhaustive()
    }
}

impl AwpEdge {
    /// Starts a builder for the site whose capabilities `registry` holds, signing with `keys`,
    /// served at `public_url`, the edge origin.
    pub fn builder(
        registry: CapabilityRegistry,
        keys: EdgeKeys,
        public_url: Url,
    ) -> AwpEdgeBuilder {
        AwpEdgeBuilder {
            registry,
            keys,
            public_url,
            tiers: EdgeTiers::builder().build(),
            limits: EdgeLimits::default(),
            stores: None,
            approval_service: None,
            approval_store: None,
            ledger: None,
            audit: None,
            principal: None,
            governance: None,
            hold_expiry: None,
            card: None,
            agent: None,
            search: None,
            threads: None,
            signals: None,
            screen: None,
            reply_policy: ReplyPolicy::default(),
            sessions: None,
            mcp_sessions: McpSessions::default(),
            events: EventsConfig::default(),
            event_log: None,
            consent: ConsentConfig::default(),
            subject_data: None,
            beacon: None,
        }
    }

    /// The edge's routes, guarded; [`ServerBuilder::awp`](crate::ServerBuilder::awp) mounts them.
    pub fn router(&self) -> Router {
        self.inner.router.clone()
    }

    /// The governed executor every binding shares.
    pub fn executor(&self) -> &GovernedExecutor {
        &self.inner.executor
    }

    /// The trust rules published at `GET /awp/v2/trust`.
    pub fn published_trust_rules(&self) -> &TrustRules {
        self.inner.tiers.published_rules()
    }

    /// The edge's mode.
    pub fn mode(&self) -> EdgeMode {
        self.inner.control.mode()
    }

    /// Switches the mode and publishes the signed status it implies (spec 3.5 §7.9, A-I11).
    ///
    /// # Errors
    ///
    /// [`StatusError`] when the status cannot be signed or stored; the mode still changes.
    pub async fn set_mode(&self, mode: EdgeMode) -> Result<Option<SiteStatus>, StatusError> {
        self.inner.control.set_mode(mode).await
    }

    /// Records the release being served, named in the status the edge signs.
    pub fn set_release_seq(&self, seq: u64) {
        self.inner.control.set_release_seq(seq);
    }

    /// Attaches the owner's kill switch unless one is attached; returns whether it was.
    pub fn attach_governance(&self, control: GovernanceControl) -> bool {
        self.inner.control.attach_governance(control)
    }

    /// The event publisher: releases, catalogue changes and order updates (spec 3.5 §7.5).
    pub fn events(&self) -> &EdgeEvents {
        &self.inner.events
    }

    /// The beacon's daily counts, for the KPI tracker's `beacon` source (spec 3.5 §7.12).
    pub fn beacon(&self) -> BeaconCounts {
        BeaconCounts::new(Arc::clone(&self.inner.beacon))
    }

    /// Resumes, every `interval` until the edge is dropped, the event deliveries a stopped
    /// process left in a durable [`EventLog`], so retries survive a restart (spec 3.5 §7.5);
    /// see [`EdgeEvents::resume_deliveries`]. With a log that keeps no deliveries it finds
    /// nothing.
    pub fn spawn_delivery_resumer(&self, interval: Duration) -> tokio::task::JoinHandle<()> {
        let weak: Weak<EdgeInner> = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let Some(inner) = weak.upgrade() else { break };
                if let Err(error) = inner.events.resume_deliveries().await {
                    tracing::warn!(error = %error, "awp delivery resumer failed");
                }
            }
        })
    }

    /// A handle that switches the edge to read-only and back, for the back office's incident
    /// path (spec 3.5 A-I11).
    pub fn status_control(&self) -> EdgeStatusControl {
        EdgeStatusControl { control: Arc::clone(&self.inner.control) }
    }

    /// Resolves held call `id` if the owner decided it; see [`GovernedExecutor::resolve`].
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the held-call store fails.
    pub async fn resolve_held(&self, id: &str) -> Result<Option<HeldCall>, StoreError> {
        self.inner.executor.resolve(id).await
    }

    /// Resolves pending held calls every `interval` until the edge is dropped, so decisions
    /// take effect without a requester polling (spec 3.5 §7.4).
    pub fn spawn_resolver(&self, interval: Duration) -> tokio::task::JoinHandle<()> {
        let weak: Weak<EdgeInner> = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let Some(inner) = weak.upgrade() else { break };
                if let Err(error) = inner.executor.resolve_pending().await {
                    tracing::warn!(error = %error, "awp held-call resolver failed");
                }
            }
        })
    }
}

/// Switches an edge to read-only and back (spec 3.5 §7.9, A-I11).
///
/// Switching the edge to read-only is one of the two incident acts an owner may
/// pre-authorise; a back-office tool holds this handle, never the edge itself. Each switch
/// publishes the signed status it implies.
///
/// # Example
///
/// ```rust,ignore
/// let control = edge.status_control();
/// control.read_only("drift_detected").await?;
/// // ... rollback to the last signed release ...
/// control.resume().await?;
/// ```
#[derive(Clone)]
pub struct EdgeStatusControl {
    control: Arc<EdgeControl>,
}

impl std::fmt::Debug for EdgeStatusControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeStatusControl").field("mode", &self.control.mode()).finish()
    }
}

impl EdgeStatusControl {
    /// Puts the edge in read-only mode for `reason`, a snake_case code such as
    /// `drift_detected`, and publishes a `read_only` status.
    ///
    /// # Errors
    ///
    /// [`StatusError`] when the status cannot be signed or stored; the mode still changes.
    pub async fn read_only(&self, reason: &str) -> Result<Option<SiteStatus>, StatusError> {
        self.control.set_mode(EdgeMode::read_only(reason)).await
    }

    /// Returns the edge to serving and publishes an `operational` status.
    ///
    /// # Errors
    ///
    /// [`StatusError`] when the status cannot be signed or stored; the mode still changes.
    pub async fn resume(&self) -> Result<Option<SiteStatus>, StatusError> {
        self.control.set_mode(EdgeMode::Serving).await
    }

    /// The edge's mode; the kill switch is reported by [`is_read_only`](Self::is_read_only).
    pub fn mode(&self) -> EdgeMode {
        self.control.mode()
    }

    /// Whether non-read-only capabilities are refused: read-only mode or the kill switch.
    pub fn is_read_only(&self) -> bool {
        self.control.is_read_only()
    }

    /// The current signed status, if one was published.
    pub async fn status(&self) -> Option<SiteStatus> {
        self.control.current_status().await
    }
}

struct Guard {
    tiers: Arc<EdgeTiers>,
    limits: Arc<EdgeLimits>,
    control: Arc<EdgeControl>,
    executor: GovernedExecutor,
}

/// The edge's guard: mode, caller, rate limit, and the HTTP binding's read-only answer.
async fn edge_guard(State(guard): State<Arc<Guard>>, request: Request, next: Next) -> Response {
    guard.control.sync_kill_switch().await;
    if guard.control.is_off() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (mut parts, mut body) = request.into_parts();
    let caller = match guard.tiers.assess(&parts).await {
        Ok(caller) => caller,
        Err(refusal) => {
            tracing::info!(awp.identity = ?refusal.method, "awp.request.refused");
            return envelope(
                ErrorBody::new(
                    ErrorCode::Unauthorized,
                    "the request's credential failed verification",
                ),
                None,
            );
        }
    };
    let standing = match guard.limits.admit_request(&caller).await {
        Ok(standing) => standing,
        Err(limited) => {
            let mut body = ErrorBody::new(
                ErrorCode::RateLimitExceeded,
                format!("the caller passed its limit of {} requests a minute", limited.limit),
            );
            body.retry_after = Some(limited.retry_after);
            let mut response = envelope(body, None);
            rate_headers(&mut response, &limited.class, limited.limit, 0, limited.retry_after);
            return response;
        }
    };
    let path = parts.uri.path().to_owned();
    if parts.method == Method::POST
        && let Some(name) = path.strip_prefix("/awp/v2/capabilities/")
        && guard.control.is_read_only()
        && guard
            .executor
            .registry()
            .visible(name, caller.tier)
            .is_some_and(|tool| tool.declaration().effect != CapabilityEffect::ReadOnly)
    {
        let mut body = ErrorBody::new(
            ErrorCode::ReadOnly,
            "the site is read-only; only read-only capabilities answer",
        );
        body.retryable = true;
        return envelope(body, guard.control.current_status().await);
    }
    // A frozen static site's forms post form-encoded; the HTTP binding reads JSON.
    if parts.method == Method::POST
        && let Some(name) = path.strip_prefix("/awp/v2/capabilities/")
        && is_form(&parts.headers)
    {
        let schema = guard
            .executor
            .registry()
            .visible(name, caller.tier)
            .and_then(|tool| tool.declaration().input_schema.clone());
        let Ok(bytes) = axum::body::to_bytes(body, adk_awp::v2::routes::MAX_CALL_BODY).await else {
            return envelope(
                ErrorBody::new(ErrorCode::InvalidRequest, "the body is too large"),
                None,
            );
        };
        let form = Request::builder()
            .method(Method::POST)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(bytes))
            .unwrap_or_default();
        let Ok(axum::Form(fields)) =
            axum::Form::<Vec<(String, String)>>::from_request(form, &()).await
        else {
            return envelope(
                ErrorBody::new(ErrorCode::InvalidRequest, "the form body could not be read"),
                None,
            );
        };
        let json = serde_json::to_vec(&form_input(fields, schema.as_ref())).unwrap_or_default();
        parts.headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        parts.headers.remove(header::CONTENT_LENGTH);
        body = axum::body::Body::from(json);
    }
    if parts.method == Method::GET
        && let Some(id) = path.strip_prefix("/awp/v2/calls/")
        && let Err(error) = guard.executor.resolve(id).await
    {
        tracing::warn!(error = %error, "awp held call could not be resolved");
    }
    let span = tracing::info_span!(
        "awp.request",
        awp.requester = caller.requester.as_ref().map_or("", |requester| requester.as_str()),
        awp.tier = %caller.tier,
        awp.claimed_agent = caller.claimed_agent.as_deref().unwrap_or(""),
        http.path = %path,
    );
    parts.extensions.insert(caller.clone());
    let discovery = (parts.method == Method::GET).then(|| discovery_surface(&path)).flatten();
    let verified = caller.tier > TrustTier::Public;
    let mut response = CURRENT_CALLER
        .scope(caller.clone(), next.run(Request::from_parts(parts, body)))
        .instrument(span)
        .await;
    if let Some(surface) = discovery
        && verified
        && response.status().is_success()
    {
        guard.executor.record_discovery(&caller, surface).await;
    }
    rate_headers(
        &mut response,
        &standing.class,
        standing.limit,
        standing.remaining,
        standing.reset,
    );
    response
}

/// The discovery surface a path serves, for `awp.request.verified` (spec 3.5 §7.12).
fn discovery_surface(path: &str) -> Option<&'static str> {
    match path {
        a2a::AGENT_CARD_PATH => Some("agent_card"),
        "/awp/v2/manifest" => Some("manifest"),
        "/mcp/server-card" | "/.well-known/mcp/server-card.json" => Some("mcp_server_card"),
        _ => None,
    }
}

fn is_form(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/x-www-form-urlencoded"))
}

/// The JSON input of a form-encoded call: each field a string, or the type its input schema
/// declares (`integer`, `number`, `boolean`, `array`); a repeated field is an array (spec 3.5
/// §7.8 freeze, §7.9).
pub(crate) fn form_input(
    fields: Vec<(String, String)>,
    schema: Option<&serde_json::Value>,
) -> serde_json::Value {
    use serde_json::Value;
    let declared = |name: &str| -> (Option<String>, Option<String>) {
        let property = schema.and_then(|schema| {
            schema.pointer(&format!("/properties/{}", name.replace('~', "~0").replace('/', "~1")))
        });
        let kind = property
            .and_then(|property| property.get("type"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let items = property
            .and_then(|property| property.pointer("/items/type"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        (kind, items)
    };
    let coerce = |raw: &str, kind: Option<&str>| -> Value {
        match kind {
            Some("integer") => raw
                .trim()
                .parse::<i64>()
                .map_or_else(|_| Value::String(raw.to_owned()), Value::from),
            Some("number") => raw
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map_or_else(|| Value::String(raw.to_owned()), Value::Number),
            Some("boolean") => match raw.trim().to_ascii_lowercase().as_str() {
                "true" | "on" | "1" | "yes" => Value::Bool(true),
                "false" | "off" | "0" | "no" => Value::Bool(false),
                _ => Value::String(raw.to_owned()),
            },
            _ => Value::String(raw.to_owned()),
        }
    };
    let mut object = serde_json::Map::new();
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for (name, value) in fields {
        match grouped.iter_mut().find(|(existing, _)| *existing == name) {
            Some((_, values)) => values.push(value),
            None => grouped.push((name, vec![value])),
        }
    }
    for (name, values) in grouped {
        let (kind, items) = declared(&name);
        let value = if kind.as_deref() == Some("array") || values.len() > 1 {
            let item_kind = if kind.as_deref() == Some("array") { items } else { kind };
            Value::Array(values.iter().map(|value| coerce(value, item_kind.as_deref())).collect())
        } else {
            coerce(&values[0], kind.as_deref())
        };
        object.insert(name, value);
    }
    Value::Object(object)
}

/// The rate-limit fields of draft-ietf-httpapi-ratelimit-headers (AWP 2.0 §6.6).
fn rate_headers(response: &mut Response, class: &str, limit: u32, remaining: u32, reset: u64) {
    let headers = response.headers_mut();
    if let Ok(policy) = HeaderValue::from_str(&format!("\"{class}\";q={limit};w=60")) {
        headers.insert("ratelimit-policy", policy);
    }
    if let Ok(state) = HeaderValue::from_str(&format!("\"{class}\";r={remaining};t={reset}")) {
        headers.insert("ratelimit", state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn form_fields_take_their_schema_types() {
        let schema = json!({ "type": "object", "properties": {
            "panels": { "type": "integer" },
            "kw": { "type": "number" },
            "roof": { "type": "boolean" },
            "days": { "type": "array", "items": { "type": "integer" } },
        } });
        let fields = [
            ("panels", "4"),
            ("kw", "5.5"),
            ("roof", "on"),
            ("days", "1"),
            ("days", "3"),
            ("name", "Ana"),
            ("tag", "a"),
            ("tag", "b"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
        assert_eq!(
            form_input(fields, Some(&schema)),
            json!({ "panels": 4, "kw": 5.5, "roof": true, "days": [1, 3], "name": "Ana", "tag": ["a", "b"] })
        );
        let unparsable = vec![("panels".to_owned(), "four".to_owned())];
        assert_eq!(form_input(unparsable, Some(&schema)), json!({ "panels": "four" }));
    }
}
