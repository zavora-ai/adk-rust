//! The site on the agent edge (spec 3.5 §7.9, §7.10, §7.16 `packs`, §7.20, A-I11).
//!
//! `adk-server`'s `AwpEdge` serves the site's capabilities over A2A, MCP and HTTP from one
//! declaration under one governed path. [`SiteEdge::build`] gives it the kind's capability
//! declarations with the packs' [`handlers`], the [`adapters`] to the back office, and returns
//! the handles the rest of the product uses:
//!
//! | Handle | Use |
//! |---|---|
//! | [`SiteEdge::status`] | The incident path's [`EdgeStatus`]: drift puts the edge in read-only mode, a person's close resumes it (A-I11) |
//! | [`SiteEdge::read_only_tool`] | The Guardian's `edge_read_only` tool, the second reversible incident act |
//! | [`SiteEdge::announcer`] | The pipeline's announcer: releases and catalog changes on the edge's events |
//! | [`stream_router`] | The stream lane's feed a live page polls |
//! | [`site_router`] | The edge's routes with the release the host serves behind them |
//! | [`consent_config`] | The consent scopes the kind's capabilities ask for |
//!
//! Payments go through the owner's [`handlers::PaymentDesk`]: [`ap2::Ap2Desk`] for AP2
//! mandates, [`token::TokenDesk`] for token rails, [`token::PaymentRails`] for both.
//! Bookings go through a [`handlers::Calendar`]: [`caldav::CalDavCalendar`] over the owner's
//! CalDAV calendar, or [`commerce::StoredCalendar`] (in memory, [`handlers::InMemoryCalendar`])
//! from `calendar.json`. Quotes, orders and the bookings of `calendar.json`'s slots live in a
//! [`commerce::CommerceStore`]: [`SiteEdge::build_with_commerce`] takes a durable one,
//! [`SiteEdge::build`] keeps them in memory.
//!
//! The declarations and their handlers come from the site's capability packs (spec 3.5
//! §7.23): [`SiteEdge::build_with_packs`] serves the kind's capabilities and every capability
//! of the packs a site installed ([`PackSet`]), each with the built-in handler its pack's
//! configuration names; the other constructors serve the built-in packs.

pub mod adapters;
pub mod ap2;
pub mod caldav;
pub mod commerce;
pub mod handlers;
pub mod token;

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_awp::v2::capability::{CapabilityHandler, CapabilityRegistry};
use adk_core::audit_log::AuditTrail;
use adk_core::{
    ActionLedger, AdkError, Agent, ApprovalService, Llm, Result, Tool, ToolContext, ToolEffect, Url,
};
use adk_harness::website::SitePartitions;
use adk_harness::website::SiteRuntime;
use adk_kernel::Partitions;
use adk_server::awp::{
    AwpEdge, BeaconStore, ConsentConfig, ConsentPolicy, EdgeKeys, EdgeLimits, EdgeStatusControl,
    EdgeStores, EdgeTiers, PublicTools, SignalEmitter, SiteSearch, ThreadSink,
};
use adk_site::host::SiteHost;
use async_trait::async_trait;
use awp_types::{CapabilityV2, ConsentScope, SiteState};
use chrono::Duration;
use serde_json::{Value, json};

use self::adapters::{EdgeAnnouncer, EdgeLiveStore, SiteSubjectData};
use self::handlers::{
    BookingHandler, Calendar, ContactBook, FormHandler, InMemoryCalendar, ListingHandler,
    OrderBook, OrderHandler, PaymentDesk, PaymentHandler, PublishedContent, QuoteBook,
    QuoteHandler, SearchHandler, StatusHandler,
};
use crate::incident::{InMemoryStatusBoard, StatusRecord, StatusSink};
use crate::kinds::KindTemplate;
use crate::packs::bundle::HandlerConfig;
use crate::packs::installed::{Dispatched, PackSet};
use crate::team::{AgentRole, SiteTeam};

/// What a site's edge is built from (spec 3.5 §7.9).
pub struct SiteEdgeParts {
    /// The site's kind, whose capabilities the edge declares.
    pub template: KindTemplate,
    /// The site runtime: published content and the release.
    pub runtime: Arc<SiteRuntime>,
    /// The edge's delegated `card` and `receipt` keys.
    pub keys: EdgeKeys,
    /// The edge's public URL, such as `https://agent.example.com`.
    pub edge_url: Url,
    /// The edge's stores.
    pub stores: EdgeStores,
    /// Signals into the back office ([`adapters::KernelSignals`]).
    pub signals: Arc<dyn SignalEmitter>,
    /// The release's index ([`adapters::ReleaseSearch`]).
    pub search: Arc<dyn SiteSearch>,
    /// The site's contact book.
    pub contacts: Arc<dyn ContactBook>,
    /// The bookable slots; none open without one.
    pub calendar: Option<Arc<dyn Calendar>>,
    /// The owner's payment rail, such as an [`ap2::Ap2Desk`]; payments are refused without
    /// one.
    pub payments: Option<Arc<dyn PaymentDesk>>,
    /// The owner's amount thresholds: orders, bookings and payments above them wait for the
    /// owner's decision (spec 3.5 §7.4, S9).
    pub thresholds: crate::packs::ApprovalThresholds,
    /// The kernel's approval service, where held calls wait for the owner.
    pub approvals: Option<Arc<dyn ApprovalService>>,
    /// Trust tiers; every caller is `public` without them.
    pub tiers: Option<EdgeTiers>,
    /// Consent scopes and policies.
    pub consent: Option<ConsentConfig>,
    /// In-thread replies ([`adapters::InboxThreads`]).
    pub threads: Option<Arc<dyn ThreadSink>>,
    /// How long a quote keeps its price (A-I25).
    pub quote_validity: Duration,
    /// The action ledger non-idempotent calls run exactly once through (A-I7), such as
    /// `measurement::MeteredActionLedger`.
    pub action_ledger: Option<Arc<dyn ActionLedger>>,
    /// Where the edge's audit records go, such as through `measurement::EdgeMeter`.
    pub audit: Option<AuditTrail>,
    /// Where the beacon counts; in memory without one.
    pub beacon: Option<Arc<dyn BeaconStore>>,
    /// The edge's rate limits; the edge's defaults without them.
    pub limits: Option<EdgeLimits>,
    /// The public agent the chat and A2A conversation run, such as [`concierge`]; none
    /// without one.
    pub public_agent: Option<PublicAgentFactory>,
}

/// Builds the public agent over the edge's own tools.
pub type PublicAgentFactory = Box<dyn FnOnce(&PublicTools) -> Arc<dyn Agent> + Send>;

/// The Concierge over the edge's public tools (spec 3.5 §7.10, §7.19, A-I2, A-I3): the
/// website preset in the site's public partition with the kind's instruction, and exactly the
/// tools the edge's public policy admits — the declared capabilities, `reply_in_thread` and
/// the edge's `emit_signal`, which files signals through the kernel's sink. A declared
/// `search_site` capability is the site's search; the edge's own one is left out so no two
/// tools share the name.
///
/// The agent is built with `adk-harness`' `website_public_served` over the edge's tools, not
/// with the kit's own `search_site` and `emit_signal`, which the edge's public policy would
/// refuse: the manifest decides each tool, the harness's `PublicPolicy` admits only the
/// public tools, the agent is offered nothing else, and the edge's governed path holds the
/// calls its manifest holds.
pub fn concierge(
    team: &SiteTeam,
    partitions: &SitePartitions,
    table: Partitions,
    model: Arc<dyn Llm>,
    workspace: impl Into<std::path::PathBuf>,
) -> PublicAgentFactory {
    let instruction = match team.public_instruction() {
        "" => AgentRole::Concierge.prompt().to_string(),
        kind => format!("{}\n\n{kind}", AgentRole::Concierge.prompt()),
    };
    let partitions = partitions.clone();
    let workspace = workspace.into();
    Box::new(move |tools: &PublicTools| {
        // The manifest decides (spec 3.5 §7.19 "Manifest policy"): the public policy admits
        // only these tools, and the edge's governed path holds what the manifest holds.
        let served = adk_harness::website::ServedPublicTools {
            partitions: partitions.clone(),
            table,
            search_site: tools.search_site.clone(),
            reply_in_thread: Some(tools.reply_in_thread.clone()),
            emit_signal: tools.emit_signal.clone(),
            capabilities: tools.capabilities.iter().map(|tool| (tool.clone(), false)).collect(),
        };
        let builder = adk_harness::HarnessAgent::builder(AgentRole::Concierge.as_str())
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(&workspace))
            .website_public_served(served)
            .instruction(instruction.clone());
        match builder.build() {
            Ok(agent) => Arc::new(agent) as Arc<dyn Agent>,
            Err(error) => {
                tracing::error!(error = %error, "the Concierge did not build; a plain agent answers");
                let declared_search =
                    tools.capabilities.iter().any(|tool| tool.name() == "search_site");
                let mut fallback = adk_agent::LlmAgentBuilder::new(AgentRole::Concierge.as_str())
                    .model(model)
                    .instruction(instruction);
                for tool in tools.capabilities.iter().cloned().chain(
                    [
                        (!declared_search).then(|| tools.search_site.clone()),
                        Some(tools.reply_in_thread.clone()),
                        Some(tools.emit_signal.clone()),
                    ]
                    .into_iter()
                    .flatten(),
                ) {
                    fallback = fallback.tool(tool);
                }
                Arc::new(fallback.build().expect("an LlmAgent with a model builds"))
            }
        }
    })
}

/// A site's edge and its handles (spec 3.5 §7.9).
pub struct SiteEdge {
    /// The edge; mount it with `ServerBuilder::awp`.
    pub edge: AwpEdge,
    /// The quotes it issued.
    pub quotes: Arc<QuoteBook>,
    /// The orders placed.
    pub orders: Arc<OrderBook>,
    /// The live lane's values.
    pub live: Arc<EdgeLiveStore>,
}

impl SiteEdge {
    /// The kind's capability declarations with the packs' handlers (spec 3.5 §7.16 `packs`),
    /// each under the owner's amount threshold (§7.4), from the built-in packs.
    ///
    /// # Errors
    ///
    /// A threshold on a capability whose calls carry no amount, or a declaration the
    /// registry refuses.
    pub fn registry(
        parts: &SiteEdgeParts,
        quotes: &Arc<QuoteBook>,
        orders: &Arc<OrderBook>,
    ) -> Result<CapabilityRegistry> {
        Self::registry_with_packs(parts, quotes, orders, &PackSet::builtin()?)
    }

    /// The capabilities `packs` serve for the kind ([`PackSet::served`]), each with the
    /// built-in handler its pack's configuration names and under the owner's amount threshold
    /// (spec 3.5 §7.4, §7.23).
    ///
    /// # Errors
    ///
    /// A threshold on a capability whose calls carry no amount, or a declaration the
    /// registry refuses.
    pub fn registry_with_packs(
        parts: &SiteEdgeParts,
        quotes: &Arc<QuoteBook>,
        orders: &Arc<OrderBook>,
        packs: &PackSet,
    ) -> Result<CapabilityRegistry> {
        Self::registry_of(parts, quotes, orders, packs.served(&parts.template))
    }

    fn registry_of(
        parts: &SiteEdgeParts,
        quotes: &Arc<QuoteBook>,
        orders: &Arc<OrderBook>,
        served: Vec<(CapabilityV2, HandlerConfig)>,
    ) -> Result<CapabilityRegistry> {
        let site = parts.runtime.site();
        let domain = Url::parse(&site.origin)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_else(|| site.name.clone());
        let content = PublishedContent::new(parts.runtime.clone());
        let forms = Arc::new(FormHandler::new(
            parts.stores.leads.clone(),
            parts.contacts.clone(),
            parts.signals.clone(),
            AgentRole::AgentDesk.principal(site),
        ));
        let calendar = parts
            .calendar
            .clone()
            .unwrap_or_else(|| Arc::new(InMemoryCalendar::default()) as Arc<dyn Calendar>);
        let booking: Arc<dyn CapabilityHandler> =
            Arc::new(BookingHandler::new(calendar, forms.clone()));
        parts.thresholds.validate()?;
        let mut order = OrderHandler::new(quotes.clone(), orders.clone());
        if let Some(desk) = &parts.payments {
            order = order.with_payments(desk.clone());
        }
        let order: Arc<dyn CapabilityHandler> = Arc::new(order);
        let mut payment = PaymentHandler::new(parts.payments.clone()).with_orders(orders.clone());
        if let Some(audit) = &parts.audit {
            payment = payment.with_audit(audit.clone());
        }
        let payment: Arc<dyn CapabilityHandler> = Arc::new(payment);
        let mut registry = CapabilityRegistry::new(domain);
        for (declaration, config) in served {
            let declaration = parts.thresholds.apply(declaration);
            let handler: Arc<dyn CapabilityHandler> = match config {
                HandlerConfig::Form => forms.clone(),
                HandlerConfig::Quote => Arc::new(QuoteHandler::new(
                    content.clone(),
                    quotes.clone(),
                    parts.quote_validity,
                )),
                HandlerConfig::Order { .. } => order.clone(),
                HandlerConfig::Payment => payment.clone(),
                HandlerConfig::Booking { .. } => booking.clone(),
                HandlerConfig::Search => Arc::new(SearchHandler::new(parts.search.clone())),
                HandlerConfig::Listing { content: kind } => {
                    Arc::new(ListingHandler::new(content.clone(), kind))
                }
                HandlerConfig::Status => Arc::new(StatusHandler::new(parts.stores.leads.clone())),
            };
            // A pack's capability reaches a handler that dispatches on a built-in name as
            // that name (spec 3.5 §7.23).
            let handler = match config.dispatches_as() {
                Some(builtin) if builtin != declaration.name => {
                    Arc::new(Dispatched::new(builtin, handler)) as Arc<dyn CapabilityHandler>
                }
                _ => handler,
            };
            registry
                .declare(declaration, handler)
                .map_err(|error| AdkError::config(error.to_string()))?;
        }
        Ok(registry)
    }

    /// Builds the site's edge (spec 3.5 §7.9).
    ///
    /// # Errors
    ///
    /// A refused declaration, or the edge builder's error.
    pub fn build(parts: SiteEdgeParts) -> Result<Self> {
        Self::build_with_commerce(parts, Arc::new(commerce::InMemoryCommerceStore::default()))
    }

    /// Builds the site's edge with its quotes and orders in `commerce`, such as a served
    /// site's [`commerce::SqliteCommerceStore`] in `data/edge.db`, so they survive a restart
    /// (spec 3.5 §7.5, §7.9).
    ///
    /// # Errors
    ///
    /// A refused declaration, or the edge builder's error.
    pub fn build_with_commerce(
        parts: SiteEdgeParts,
        commerce: Arc<dyn commerce::CommerceStore>,
    ) -> Result<Self> {
        Self::build_with_packs(parts, commerce, &PackSet::builtin()?)
    }

    /// Builds the site's edge over `packs`, such as a served site's [`PackDir::load`]
    /// (spec 3.5 §7.9, §7.23): the kind's capabilities and those of the installed packs, each
    /// with the handler its pack configures, quotes and orders in `commerce`. The consent
    /// scopes the served capabilities ask for join `parts.consent`'s.
    ///
    /// [`PackDir::load`]: crate::packs::installed::PackDir::load
    ///
    /// # Errors
    ///
    /// A refused declaration, or the edge builder's error.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use adk_website::edge::commerce::InMemoryCommerceStore;
    /// use adk_website::edge::{SiteEdge, SiteEdgeParts};
    /// use adk_website::packs::installed::PackDir;
    ///
    /// fn edge(parts: SiteEdgeParts, project: &std::path::Path) -> adk_core::Result<SiteEdge> {
    ///     let packs = PackDir::new(project).load()?;
    ///     SiteEdge::build_with_packs(parts, Arc::new(InMemoryCommerceStore::default()), &packs)
    /// }
    /// ```
    pub fn build_with_packs(
        mut parts: SiteEdgeParts,
        commerce: Arc<dyn commerce::CommerceStore>,
        packs: &PackSet,
    ) -> Result<Self> {
        let quotes = Arc::new(QuoteBook::new(commerce.clone()));
        let orders = Arc::new(OrderBook::new(commerce));
        let served = packs.served(&parts.template);
        if let Some(consent) = parts.consent.as_mut() {
            for (declaration, _) in &served {
                if let Some(required) = &declaration.consent
                    && !consent.scopes.contains(&required.scope)
                {
                    consent.scopes.push(required.scope.clone());
                }
            }
        }
        let registry = Self::registry_of(&parts, &quotes, &orders, served)?;
        let mut builder = AwpEdge::builder(registry, parts.keys, parts.edge_url)
            .stores(parts.stores.clone())
            .signals(parts.signals.clone())
            .site_search(parts.search.clone())
            .subject_data(Arc::new(SiteSubjectData::new(
                parts.stores.leads.clone(),
                parts.contacts.clone(),
            )))
            .principal(
                adk_harness::website::SitePartitions::new(parts.runtime.site())
                    .public()
                    .principal(&parts.runtime.site().name, AgentRole::AgentDesk.as_str()),
            );
        if let Some(tiers) = parts.tiers {
            builder = builder.tiers(tiers);
        }
        if let Some(approvals) = parts.approvals {
            builder = builder.approval_service(approvals);
        }
        if let Some(consent) = parts.consent {
            builder = builder.consent(consent);
        }
        if let Some(threads) = parts.threads {
            builder = builder.threads(threads);
        }
        if let Some(ledger) = parts.action_ledger {
            builder = builder.action_ledger(ledger);
        }
        if let Some(audit) = parts.audit {
            builder = builder.audit(audit);
        }
        if let Some(beacon) = parts.beacon {
            builder = builder.beacon_store(beacon);
        }
        if let Some(limits) = parts.limits {
            builder = builder.limits(limits);
        }
        if let Some(agent) = parts.public_agent {
            builder = builder.public_agent(agent);
        }
        Ok(Self {
            edge: builder.build()?,
            quotes,
            orders,
            live: Arc::new(EdgeLiveStore::default()),
        })
    }

    /// The incident path's status: the edge's signed status and read-only mode (A-I11).
    pub fn status(&self) -> EdgeStatus {
        EdgeStatus::new(self.edge.status_control())
    }

    /// The Guardian's `edge_read_only` tool (spec 3.5 A-I11).
    pub fn read_only_tool(&self) -> Arc<dyn Tool> {
        Arc::new(EdgeReadOnlyTool { control: self.edge.status_control() })
    }

    /// The pipeline's announcer over the edge's events (spec 3.5 §7.8.2 step 7).
    pub fn announcer(&self) -> Arc<EdgeAnnouncer> {
        Arc::new(EdgeAnnouncer::new(self.edge.events().clone()))
    }
}

/// The consent the kind asks for (spec 3.5 §7.5, A-I8): every scope a declaration names, and
/// `nurture` when the kind nurtures its leads ([`crate::leads::consent_scopes`]), under the packs' [`POLICY_VERSION`](crate::packs::POLICY_VERSION) with
/// `policy` as its text, and subject sessions keyed by `subject_key` (at least 32 bytes).
///
/// # Example
///
/// ```rust
/// use adk_server::awp::ConsentPolicy;
/// use adk_website::edge::consent_config;
/// use adk_website::kinds::template;
/// use awp_types::{ConsentScope, Digest, SiteKind};
///
/// let policy = ConsentPolicy { digest: Digest::from_sha256_bytes([1; 32]), ttl: None };
/// let consent = consent_config(&template(SiteKind::Business), policy, vec![7u8; 32]);
/// assert!(consent.scopes.contains(&ConsentScope::from("contact")));
/// assert!(consent.scopes.contains(&ConsentScope::from("nurture")), "a business nurtures its leads");
/// ```
pub fn consent_config(
    template: &KindTemplate,
    policy: ConsentPolicy,
    subject_key: impl Into<Arc<[u8]>>,
) -> ConsentConfig {
    let scopes: Vec<ConsentScope> = crate::leads::consent_scopes(template);
    let policies = BTreeMap::from([(crate::packs::POLICY_VERSION.to_string(), policy)]);
    ConsentConfig::new(scopes, policies, subject_key)
}

/// The site's public router (spec 3.5 §7.9, §7.22): the edge's routes, and behind them the
/// files of the release `host` serves, each with its release media type, HSTS, and a CSP on
/// pages; before the launch `/` answers the holding page of the site `name`.
pub fn site_router(edge: &AwpEdge, host: Arc<dyn SiteHost>, name: &str) -> axum::Router {
    use axum::http::{HeaderValue, StatusCode, Uri, header};
    use axum::response::{IntoResponse, Response};

    async fn content(host: &dyn SiteHost, name: &str, path: &str) -> Response {
        let path = if path.ends_with('/') { format!("{path}index.html") } else { path.to_string() };
        let (kind, body) = match host.fetch(&path).await {
            Ok(Some(bytes)) => (adk_site::release::content_type(&path), bytes.to_vec()),
            Ok(None) if path == "/index.html" => {
                ("text/html; charset=utf-8", crate::onboarding::holding_page(name).into_bytes())
            }
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(error) => {
                tracing::error!(error = %error, path, "the host could not serve a file");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        };
        let mut response = (StatusCode::OK, body).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
        if kind.starts_with("text/html") {
            headers.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("default-src 'self'; script-src 'self'"),
            );
        }
        response
    }

    let name = name.to_string();
    edge.router().fallback(move |uri: Uri| {
        let (host, name) = (host.clone(), name.clone());
        async move { content(host.as_ref(), &name, uri.path()).await }
    })
}

/// The site's status on the edge (spec 3.5 §7.5, §7.20, A-I11): an incident or a read-only
/// state puts the edge in read-only mode with the reason, so every non-read capability
/// answers `503 read_only` with the signed status; `operational` resumes it.
pub struct EdgeStatus {
    control: EdgeStatusControl,
    board: InMemoryStatusBoard,
}

impl EdgeStatus {
    /// The status over `control`, the edge's.
    pub fn new(control: EdgeStatusControl) -> Self {
        Self { control, board: InMemoryStatusBoard::new() }
    }
}

#[async_trait]
impl StatusSink for EdgeStatus {
    async fn set(&self, site: uuid::Uuid, status: StatusRecord) -> Result<()> {
        let switched = match status.state {
            SiteState::Operational => self.control.resume().await,
            _ => self.control.read_only(&status.reason).await,
        };
        if let Err(error) = switched {
            tracing::warn!(error = %error, "the edge's status was not signed; its mode changed");
        }
        self.board.set(site, status).await
    }

    async fn get(&self, site: uuid::Uuid) -> Result<Option<StatusRecord>> {
        self.board.get(site).await
    }
}

/// `edge_read_only`: switches the edge to read-only for a reason, or back (spec 3.5 A-I11).
/// It implements `adk-harness`' catalogue entry, under the back office's origin, so the
/// catalogue classes it as a reversible incident act an owner may pre-authorise.
struct EdgeReadOnlyTool {
    control: EdgeStatusControl,
}

#[async_trait]
impl Tool for EdgeReadOnlyTool {
    fn name(&self) -> &str {
        adk_harness::website::EDGE_READ_ONLY
    }

    fn origin(&self) -> &str {
        adk_harness::website::BACK_OFFICE_ORIGIN
    }

    fn description(&self) -> &str {
        "Switches the agent edge to read-only for a reason, such as drift_detected, or back to \
         serving with resume: true. Read-only capabilities and static pages keep working."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "reason": { "type": "string", "pattern": "^[a-z][a-z0-9_]{0,63}$" },
                "resume": { "type": "boolean" }
            },
            "additionalProperties": false
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let resume = args["resume"].as_bool().unwrap_or(false);
        let result = if resume {
            self.control.resume().await
        } else {
            self.control.read_only(args["reason"].as_str().unwrap_or("incident")).await
        };
        let status = result.map_err(|error| AdkError::tool(error.to_string()))?;
        Ok(json!({ "readOnly": self.control.is_read_only(), "status": status }))
    }
}

/// The stream lane's feed (spec 3.5 §7.8.3 "Stream"): `GET /awp/v2/stream?item={id}&after={n}`
/// answers the entries after `n`, each signed by the edge's receipt key.
pub fn stream_router(engine: Arc<adk_site::updates::UpdateEngine>) -> axum::Router {
    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    #[derive(serde::Deserialize)]
    struct Params {
        item: String,
        #[serde(default)]
        after: u64,
    }

    async fn feed(
        State(engine): State<Arc<adk_site::updates::UpdateEngine>>,
        Query(params): Query<Params>,
    ) -> axum::response::Response {
        match engine.stream_since(&awp_types::ContentId(params.item), params.after).await {
            Ok(entries) => axum::Json(json!({ "entries": entries })).into_response(),
            Err(error) => {
                (StatusCode::NOT_FOUND, axum::Json(json!({ "error": error.to_string() })))
                    .into_response()
            }
        }
    }

    axum::Router::new().route("/awp/v2/stream", axum::routing::get(feed)).with_state(engine)
}
