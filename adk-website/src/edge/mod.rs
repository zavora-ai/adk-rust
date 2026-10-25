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

pub mod adapters;
pub mod handlers;

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_awp::v2::capability::{CapabilityHandler, CapabilityRegistry};
use adk_core::{AdkError, ApprovalService, Result, Tool, ToolContext, ToolEffect, Url};
use adk_harness::website::SiteRuntime;
use adk_server::awp::{
    AwpEdge, ConsentConfig, ConsentPolicy, EdgeKeys, EdgeStatusControl, EdgeStores, EdgeTiers,
    SignalEmitter, SiteSearch, ThreadSink,
};
use adk_site::host::SiteHost;
use async_trait::async_trait;
use awp_types::{ConsentScope, ContentKind, SiteState};
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
use crate::team::AgentRole;

/// The capabilities whose handler is a form (spec 3.5 §7.16 `contact`).
const FORMS: &[&str] = &[
    "submit_inquiry",
    "subscribe_newsletter",
    "request_prospectus",
    "licensing_enquiry",
    "volunteer",
    "register",
    "submit_service_form",
];

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
    /// The owner's payment rail; payments are refused without one.
    pub payments: Option<Arc<dyn PaymentDesk>>,
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
    /// The kind's capability declarations with the packs' handlers (spec 3.5 §7.16 `packs`).
    ///
    /// # Errors
    ///
    /// A declaration the registry refuses.
    pub fn registry(
        parts: &SiteEdgeParts,
        quotes: &Arc<QuoteBook>,
        orders: &Arc<OrderBook>,
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
        let order: Arc<dyn CapabilityHandler> =
            Arc::new(OrderHandler::new(quotes.clone(), orders.clone()));
        let payment: Arc<dyn CapabilityHandler> =
            Arc::new(PaymentHandler::new(parts.payments.clone()));
        let mut registry = CapabilityRegistry::new(domain);
        for declaration in parts.template.capabilities() {
            let handler: Arc<dyn CapabilityHandler> = match declaration.name.as_str() {
                name if FORMS.contains(&name) => forms.clone(),
                "get_quote" => Arc::new(QuoteHandler::new(
                    content.clone(),
                    quotes.clone(),
                    parts.quote_validity,
                )),
                "place_order" | "order_status" | "request_return" => order.clone(),
                "pay_order" | "donate" | "subscribe" | "buy_ticket" => payment.clone(),
                "check_availability" | "book" | "book_appointment" => booking.clone(),
                "search_site" => Arc::new(SearchHandler::new(parts.search.clone())),
                "list_products" => {
                    Arc::new(ListingHandler::new(content.clone(), ContentKind::Product))
                }
                "list_notices" => {
                    Arc::new(ListingHandler::new(content.clone(), ContentKind::Notice))
                }
                "list_events" | "get_schedule" => {
                    Arc::new(ListingHandler::new(content.clone(), ContentKind::Event))
                }
                "practice_information" => {
                    Arc::new(ListingHandler::new(content.clone(), ContentKind::Service))
                }
                "lookup_status" => Arc::new(StatusHandler::new(parts.stores.leads.clone())),
                other => {
                    tracing::warn!(
                        capability = other,
                        "no pack handles this capability; it is not served"
                    );
                    continue;
                }
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
        let quotes = Arc::new(QuoteBook::default());
        let orders = Arc::new(OrderBook::default());
        let registry = Self::registry(&parts, &quotes, &orders)?;
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

/// The consent the kind's capabilities ask for (spec 3.5 §7.5, A-I8): every scope a
/// declaration names, under the packs' [`POLICY_VERSION`](crate::packs::POLICY_VERSION) with
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
/// ```
pub fn consent_config(
    template: &KindTemplate,
    policy: ConsentPolicy,
    subject_key: impl Into<Arc<[u8]>>,
) -> ConsentConfig {
    let mut scopes: Vec<ConsentScope> = Vec::new();
    for requirement in template.capabilities().into_iter().filter_map(|c| c.consent) {
        if !scopes.contains(&requirement.scope) {
            scopes.push(requirement.scope);
        }
    }
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
struct EdgeReadOnlyTool {
    control: EdgeStatusControl,
}

#[async_trait]
impl Tool for EdgeReadOnlyTool {
    fn name(&self) -> &str {
        "edge_read_only"
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
