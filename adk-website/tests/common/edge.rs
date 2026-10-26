//! A fixture site's agent edge (spec 3.5 §7.9): the kind's capabilities with the packs'
//! handlers, in-memory edge stores, consent for the kind's scopes, Web Bot Auth tiers that
//! know one buyer agent, the owner's approval inbox, and the release on the host behind it.

use std::sync::{Arc, Mutex};

use adk_auth::web_bot_auth::{
    Ed25519SigningKey, HttpSignatureSigner, HttpSignatureVerifier, InMemoryKeyDirectorySource,
    KeyDirectory,
};
use adk_awp::signing::jws::DocumentSigner;
use adk_awp::v2::store::MemoryEdgeStores;
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalService, ApproverSelector, InMemoryApprovalStore,
    InboxQuery, StoreApprovalService, Url,
};
use adk_server::awp::{ConsentPolicy, EdgeKeys, EdgeStores, EdgeTiers, WebBotAuthTrustAssigner};
use adk_site::pipeline::{Announcement, Announcer};
use adk_website::contract::SiteKeys;
use adk_website::edge::adapters::{KernelSignals, ReleaseSearch};
use adk_website::edge::handlers::InMemoryContactBook;
use adk_website::edge::{SiteEdge, SiteEdgeParts, consent_config, site_router};
use awp_types::Digest;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

use super::{OWNER, Site};

/// The buyer agent's key directory.
pub const BUYER: &str = "https://buyer.example";

/// The fixture's root, release and edge seeds; the release seed is the one the fixture's site
/// log signs with.
pub const ROOT_SEED: [u8; 32] = [0x11; 32];
pub const RELEASE_SEED: [u8; 32] = [7; 32];
pub const EDGE_SEED: [u8; 32] = [0x33; 32];

/// A site's edge and what a test drives it with.
pub struct EdgeSite {
    pub edge: SiteEdge,
    pub router: Router,
    pub stores: MemoryEdgeStores,
    pub buyer: HttpSignatureSigner,
    pub approvals: Arc<dyn ApprovalService>,
    pub contacts: Arc<InMemoryContactBook>,
    pub keys: SiteKeys,
    pub origin: String,
    pub announced: Arc<Recorded>,
}

/// The site's keys from the fixture's seeds.
pub fn site_keys(domain: &str) -> SiteKeys {
    SiteKeys::from_seeds(domain, ROOT_SEED, RELEASE_SEED, EDGE_SEED).unwrap()
}

/// The edge of `site`, served at the site's own origin.
pub async fn edge(site: &Site, domain: &str) -> EdgeSite {
    edge_with(site, domain, None).await
}

/// The edge of `site` with `public_agent` answering conversations.
pub async fn edge_with(
    site: &Site,
    domain: &str,
    public_agent: Option<adk_website::edge::PublicAgentFactory>,
) -> EdgeSite {
    let keys = site_keys(domain);
    let buyer_key = Arc::new(Ed25519SigningKey::generate().unwrap());
    let directories = Arc::new(InMemoryKeyDirectorySource::new());
    directories.insert(BUYER, KeyDirectory::from_keys([buyer_key.as_ref()]), None).await;
    let buyer = HttpSignatureSigner::builder(buyer_key, BUYER).build().unwrap();
    let tiers = EdgeTiers::builder()
        .verifier(WebBotAuthTrustAssigner::new(HttpSignatureVerifier::builder(directories).build()))
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
    let edge = SiteEdge::build(SiteEdgeParts {
        template: site.template.clone(),
        runtime: site.runtime.clone(),
        keys: EdgeKeys { card: signer.clone(), receipt: signer },
        edge_url: Url::parse(&origin).unwrap(),
        stores: EdgeStores::in_memory(&stores),
        signals: Arc::new(KernelSignals::new(
            site.partitions.signal_sink(site.signals.clone(), site.agenda.clone()),
        )),
        search: Arc::new(ReleaseSearch::new(site.search.clone())),
        contacts: contacts.clone(),
        calendar: None,
        payments: None,
        approvals: Some(approvals.clone()),
        tiers: Some(tiers),
        consent: Some(consent_config(&site.template, policy, vec![9u8; 32])),
        threads: None,
        quote_validity: chrono::Duration::days(7),
        action_ledger: None,
        audit: None,
        beacon: None,
        limits: None,
        public_agent,
    })
    .unwrap();
    let router = site_router(&edge.edge, site.host.clone(), &site.base.config.name);
    let announced = Arc::new(Recorded { target: edge.announcer(), seen: Mutex::default() });
    site.announcer.set(announced.clone());
    EdgeSite { edge, router, stores, buyer, approvals, contacts, keys, origin, announced }
}

/// The edge's announcer, recording each release and whether the edge took it.
pub struct Recorded {
    target: Arc<dyn Announcer>,
    pub seen: Mutex<Vec<(u64, bool)>>,
}

#[async_trait::async_trait]
impl Announcer for Recorded {
    async fn announce(&self, announcement: &Announcement) -> adk_site::Result<()> {
        let result = self.target.announce(announcement).await;
        self.seen.lock().unwrap().push((announcement.release.seq, result.is_ok()));
        result
    }
}

impl EdgeSite {
    pub fn get(&self, path: &str) -> Request<Body> {
        Request::get(format!("{}{path}", self.origin)).body(Body::empty()).unwrap()
    }

    pub fn post(&self, path: &str, body: &Value) -> Request<Body> {
        Request::post(format!("{}{path}", self.origin))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
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
        request.headers_mut().insert("idempotency-key", key.parse().unwrap());
        if let Some(receipt) = receipt {
            request.headers_mut().insert("awp-consent", format!("\"{receipt}\"").parse().unwrap());
        }
        request
    }

    /// `request` signed by the buyer agent: the `known` tier.
    pub fn signed(&self, mut request: Request<Body>) -> Request<Body> {
        self.buyer.sign_request(&mut request).unwrap();
        request
    }

    pub async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, headers, body)
    }

    /// Grants consent to `scope` under the packs' policy version; returns the receipt's id.
    pub async fn consent(&self, scope: &str) -> String {
        let grant =
            serde_json::json!({ "scope": scope, "policyVersion": "1", "jurisdiction": "KE" });
        let (status, _, receipt) = self.send(self.post("/awp/v2/consent", &grant)).await;
        assert_eq!(status, StatusCode::CREATED, "{receipt}");
        receipt["id"].as_str().unwrap().to_string()
    }

    /// The owner approves every call waiting in the inbox.
    pub async fn approve_pending(&self) -> usize {
        let page = self
            .approvals
            .inbox(InboxQuery::for_approver(SubjectRef::human(OWNER), Vec::<String>::new()))
            .await
            .unwrap();
        let count = page.items.len();
        for request in page.items {
            self.approvals
                .decide(
                    &request.id,
                    request.version,
                    ApprovalDecision::approve(SubjectRef::human(OWNER)),
                )
                .await
                .unwrap();
        }
        count
    }
}
