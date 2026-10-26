//! A fixture site's agent edge (spec 3.5 §7.9): the kind's capabilities with the packs'
//! handlers, in-memory edge stores, consent for the kind's scopes, trust tiers that know one
//! buyer agent and one partner's OAuth issuer, the owner's approval inbox, and the release on
//! the host behind it.

use std::sync::Arc;

use adk_auth::sso::{TokenClaims, TokenError, TokenValidator};
use adk_auth::web_bot_auth::{
    Ed25519SigningKey, HttpSignatureSigner, HttpSignatureVerifier, InMemoryKeyDirectorySource,
    KeyDirectory,
};
use adk_awp::signing::jws::DocumentSigner;
use adk_awp::v2::store::MemoryEdgeStores;
use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalService, ApproverSelector, InMemoryApprovalStore,
    InboxQuery, StoreApprovalService, Url,
};
use adk_server::awp::tiers::OAuthClientCredentials;
use adk_server::awp::{
    ConsentPolicy, EdgeKeys, EdgeLimits, EdgeStores, EdgeTiers, WebBotAuthTrustAssigner,
};
use adk_website::contract::SiteKeys;
use adk_website::edge::adapters::{KernelSignals, ReleaseSearch};
use adk_website::edge::handlers::{Calendar, InMemoryContactBook, PaymentDesk};
use adk_website::edge::{PublicAgentFactory, SiteEdge, SiteEdgeParts, consent_config, site_router};
use anyhow::{Result, anyhow};
use awp_types::{Digest, RequesterId, TrustTier};
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use base64::Engine as _;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{OWNER, RELEASE_SEED, Site};

/// The buyer agent's key directory.
pub const BUYER: &str = "https://buyer.example";
/// The partner's OAuth issuer.
pub const PARTNER_ISSUER: &str = "https://auth.partner.example";

/// The fixture's root and edge seeds; the release seed is the one the site log signs with.
pub const ROOT_SEED: [u8; 32] = [0x11; 32];
/// The edge key's seed.
pub const EDGE_SEED: [u8; 32] = [0x33; 32];

/// What a fixture edge is built with beyond the defaults.
#[derive(Default)]
pub struct EdgeOptions {
    /// The owner's payment rail.
    pub payments: Option<Arc<dyn PaymentDesk>>,
    /// The bookable slots.
    pub calendar: Option<Arc<dyn Calendar>>,
    /// Rate limits.
    pub limits: Option<EdgeLimits>,
    /// Where the beacon counts.
    pub beacon: Option<Arc<dyn adk_server::awp::BeaconStore>>,
    /// The public agent the edge hosts on A2A and chat, such as `edge::concierge`.
    pub public_agent: Option<PublicAgentFactory>,
    /// The owner's amount thresholds: orders, bookings and payments above them wait.
    pub thresholds: adk_website::packs::ApprovalThresholds,
}

/// A site's edge and what a scenario drives it with.
pub struct EdgeSite {
    /// The edge and its handles.
    pub edge: SiteEdge,
    /// The public router: the edge's routes with the release behind them.
    pub router: Router,
    /// The edge's stores.
    pub stores: MemoryEdgeStores,
    /// The buyer agent's Web Bot Auth signer.
    pub buyer: HttpSignatureSigner,
    /// The buyer's verified identity.
    pub buyer_id: RequesterId,
    /// The owner's approval inbox.
    pub approvals: Arc<dyn ApprovalService>,
    /// The site's origin.
    pub origin: String,
    /// The edge's own audit records.
    pub audit: InMemoryAuditRecorder,
}

/// The site's keys from the fixture's seeds.
///
/// # Errors
///
/// When a seed is refused.
pub fn site_keys(domain: &str) -> Result<SiteKeys> {
    Ok(SiteKeys::from_seeds(domain, ROOT_SEED, RELEASE_SEED, EDGE_SEED)?)
}

/// An OAuth or OIDC issuer that validates the one token it issued, granting `scope`.
pub struct StaticIssuer {
    /// The issuer.
    pub issuer: &'static str,
    /// The token's subject and client.
    pub subject: &'static str,
    /// The scope it grants.
    pub scope: &'static str,
}

/// The partner's OAuth issuer: `awp:partner`.
pub const PARTNER: StaticIssuer =
    StaticIssuer { issuer: PARTNER_ISSUER, subject: "partner-co", scope: "awp:partner" };

/// A national identity provider (an OIDC fixture) whose verified residents apply for
/// services: `awp:verified`.
pub const APPLICANT: StaticIssuer = StaticIssuer {
    issuer: "https://id.ecitizen.example",
    subject: "resident-2741",
    scope: "awp:verified",
};

impl StaticIssuer {
    /// The bearer token it issued.
    pub fn token(&self) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({ "iss": self.issuer, "sub": self.subject }).to_string());
        format!("eyJhbGciOiJSUzI1NiJ9.{payload}.c2ln")
    }
}

#[async_trait::async_trait]
impl TokenValidator for StaticIssuer {
    async fn validate(&self, token: &str) -> std::result::Result<TokenClaims, TokenError> {
        if token != self.token() {
            return Err(TokenError::ValidationError("unknown token".into()));
        }
        serde_json::from_value(json!({
            "sub": self.subject,
            "iss": self.issuer,
            "client_id": self.subject,
            "scope": self.scope,
        }))
        .map_err(|e| TokenError::ValidationError(e.to_string()))
    }

    fn issuer(&self) -> &str {
        self.issuer
    }
}

/// The edge of `site`, served at the site's own origin.
///
/// # Errors
///
/// When a key or the edge cannot be built.
pub async fn edge(site: &Site, domain: &str) -> Result<EdgeSite> {
    edge_with(site, domain, EdgeOptions::default()).await
}

/// The edge of `site` with `options`.
///
/// # Errors
///
/// When a key or the edge cannot be built.
pub async fn edge_with(site: &Site, domain: &str, options: EdgeOptions) -> Result<EdgeSite> {
    let keys = site_keys(domain)?;
    let buyer_key = Arc::new(Ed25519SigningKey::generate()?);
    let buyer_id = RequesterId::web_bot_auth(BUYER, buyer_key.thumbprint());
    let directories = Arc::new(InMemoryKeyDirectorySource::new());
    directories.insert(BUYER, KeyDirectory::from_keys([buyer_key.as_ref()]), None).await;
    let buyer = HttpSignatureSigner::builder(buyer_key, BUYER).build()?;
    let tiers = EdgeTiers::builder()
        .verifier(WebBotAuthTrustAssigner::new(HttpSignatureVerifier::builder(directories).build()))
        .verifier(
            OAuthClientCredentials::new()
                .validator(Arc::new(PARTNER))
                .validator(Arc::new(APPLICANT))
                .scope_tier("awp:partner", TrustTier::Partner)
                .scope_tier("awp:verified", TrustTier::Verified),
        )
        .build();
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_router(ApprovalPolicy::new([ApproverSelector::human(OWNER)])),
    );
    let stores = MemoryEdgeStores::default();
    let contacts = Arc::new(InMemoryContactBook::default());
    let signer: Arc<dyn DocumentSigner> = keys.edge.clone();
    let policy = ConsentPolicy { digest: Digest::from_sha256_bytes([1; 32]), ttl: None };
    let origin = site.site.origin.clone();
    let audit = InMemoryAuditRecorder::new();
    let parts = SiteEdgeParts {
        template: site.template.clone(),
        runtime: site.runtime.clone(),
        keys: EdgeKeys { card: signer.clone(), receipt: signer },
        edge_url: Url::parse(&origin)?,
        stores: EdgeStores::in_memory(&stores),
        signals: Arc::new(KernelSignals::new(
            site.partitions.signal_sink(site.signals.clone(), site.agenda.clone()),
        )),
        search: Arc::new(ReleaseSearch::new(site.search.clone())),
        contacts: contacts.clone(),
        calendar: options.calendar,
        payments: options.payments,
        thresholds: options.thresholds,
        approvals: Some(approvals.clone()),
        tiers: Some(tiers),
        consent: Some(consent_config(&site.template, policy, vec![9u8; 32])),
        threads: None,
        quote_validity: chrono::Duration::days(7),
        action_ledger: None,
        audit: Some(AuditTrail::new(Arc::new(audit.clone()))),
        beacon: options.beacon,
        limits: options.limits,
        public_agent: options.public_agent,
    };
    let edge = SiteEdge::build(parts).map_err(|e| anyhow!("{e}"))?;
    let router = site_router(&edge.edge, site.host.clone(), &site.base.config.name);
    site.announcer.set(edge.announcer());
    Ok(EdgeSite { edge, router, stores, buyer, buyer_id, approvals, origin, audit })
}

impl EdgeSite {
    /// `GET path` at the site's origin.
    pub fn get(&self, path: &str) -> Request<Body> {
        Request::get(format!("{}{path}", self.origin)).body(Body::empty()).expect("a request")
    }

    /// `POST path` with a JSON body.
    pub fn post(&self, path: &str, body: &Value) -> Request<Body> {
        Request::post(format!("{}{path}", self.origin))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("a request")
    }

    /// A capability call with an idempotency key and, when given, a consent receipt.
    pub fn call(
        &self,
        capability: &str,
        key: &str,
        receipt: Option<&str>,
        body: &Value,
    ) -> Request<Body> {
        let mut request = self.post(&format!("/awp/v2/capabilities/{capability}"), body);
        request.headers_mut().insert("idempotency-key", key.parse().expect("a header"));
        if let Some(receipt) = receipt {
            request
                .headers_mut()
                .insert("awp-consent", format!("\"{receipt}\"").parse().expect("a header"));
        }
        request
    }

    /// `request` signed by the buyer agent: the `known` tier.
    pub fn signed(&self, mut request: Request<Body>) -> Request<Body> {
        self.buyer.sign_request(&mut request).expect("the buyer signs");
        request
    }

    /// `request` with the partner's bearer token: the `partner` tier.
    pub fn partner(&self, request: Request<Body>) -> Request<Body> {
        Self::bearer(request, &PARTNER)
    }

    /// `request` with a resident's token from the identity provider: the `verified` tier.
    pub fn applicant(&self, request: Request<Body>) -> Request<Body> {
        Self::bearer(request, &APPLICANT)
    }

    fn bearer(mut request: Request<Body>, issuer: &StaticIssuer) -> Request<Body> {
        let bearer = format!("Bearer {}", issuer.token());
        request.headers_mut().insert(header::AUTHORIZATION, bearer.parse().expect("a header"));
        request
    }

    /// Sends `request` through the public router.
    pub async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        let response = self.router.clone().oneshot(request).await.expect("the router answers");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap_or_default();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, headers, body)
    }

    /// Grants consent to `scope` under the packs' policy version; returns the receipt's id.
    ///
    /// # Errors
    ///
    /// When the edge does not issue a receipt.
    pub async fn consent(&self, scope: &str) -> Result<String> {
        let grant = json!({ "scope": scope, "policyVersion": "1", "jurisdiction": "KE" });
        let (status, _, receipt) = self.send(self.post("/awp/v2/consent", &grant)).await;
        anyhow::ensure!(status == StatusCode::CREATED, "consent refused: {status} {receipt}");
        receipt["id"].as_str().map(str::to_string).ok_or_else(|| anyhow!("no receipt id"))
    }

    /// The owner approves every call waiting in the inbox; returns how many.
    ///
    /// # Errors
    ///
    /// When the approval service refuses.
    pub async fn approve_pending(&self) -> Result<usize> {
        let page = self
            .approvals
            .inbox(InboxQuery::for_approver(SubjectRef::human(OWNER), Vec::<String>::new()))
            .await?;
        let count = page.items.len();
        for request in page.items {
            self.approvals
                .decide(
                    &request.id,
                    request.version,
                    ApprovalDecision::approve(SubjectRef::human(OWNER)),
                )
                .await?;
        }
        Ok(count)
    }

    /// The capability names a manifest lists.
    pub fn names(manifest: &Value) -> Vec<String> {
        manifest["capabilities"]
            .as_array()
            .map(|list| {
                list.iter().filter_map(|c| c["name"].as_str().map(str::to_string)).collect()
            })
            .unwrap_or_default()
    }
}
