//! The SQLite edge stores (spec 3.5 §7.9): the edge store conformance suite, the event log and
//! beacon store conformance suite, and an edge that restarts over the same database file and
//! still answers its held call and its idempotency record, serves its catalogue pages, reads
//! its beacon counts and resumes the delivery a stopped process left.
//!
//! Every handle is a new pool over the file, as a restarted or second process would open:
//!
//! ```bash
//! cargo nextest run -p adk-server --features awp-sqlite --test awp_store_sqlite
//! ```

#![cfg(feature = "awp-sqlite")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_awp::signing::jws::Ed25519DocumentSigner;
use adk_awp::v2::capability::{
    CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry,
};
use adk_awp::v2::store::conformance::EdgeStoreHarness;
use adk_awp::v2::store::{
    ConsentReceiptStore, HeldCallStore, IdempotencyStore, LeadStore, StatusStore, SubscriptionStore,
};
use adk_core::{InMemoryApprovalStore, Url};
use adk_server::awp::conformance::EdgeLogHarness;
use adk_server::awp::stores::SqliteEdgeStores;
use adk_server::awp::tiers::ApiKeyCredentials;
use adk_server::awp::{AwpEdge, EdgeKeys, EdgeTiers};
use adk_server::awp::{BeaconStore, EventLog, EventsConfig};
use async_trait::async_trait;
use awp_types::{ApprovalHint, CapabilityEffect, CapabilityV2, TrustTier};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tower::ServiceExt;

/// A database file in a directory removed when the harness drops.
struct File {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl File {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("edge.db");
        Self { _dir: dir, path }
    }

    async fn pool(&self) -> SqlitePool {
        let options = SqliteConnectOptions::new()
            .filename(&self.path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(10));
        SqlitePoolOptions::new().max_connections(4).connect_with(options).await.expect("open")
    }

    async fn stores(&self) -> SqliteEdgeStores {
        let stores = SqliteEdgeStores::new(self.pool().await).with_site("example.com");
        stores.migrate().await.expect("migrate");
        stores
    }
}

#[async_trait]
impl EdgeStoreHarness for File {
    async fn subscriptions(&self) -> Arc<dyn SubscriptionStore> {
        Arc::new(self.stores().await)
    }
    async fn consent(&self) -> Arc<dyn ConsentReceiptStore> {
        Arc::new(self.stores().await)
    }
    async fn status(&self) -> Arc<dyn StatusStore> {
        Arc::new(self.stores().await)
    }
    async fn held_calls(&self) -> Arc<dyn HeldCallStore> {
        Arc::new(self.stores().await)
    }
    async fn leads(&self) -> Arc<dyn LeadStore> {
        Arc::new(self.stores().await)
    }
    async fn idempotency(&self) -> Arc<dyn IdempotencyStore> {
        Arc::new(self.stores().await)
    }
}

adk_awp::edge_store_conformance_tests! {
    harness = File::new(),
}

#[async_trait]
impl EdgeLogHarness for File {
    async fn events(&self) -> Arc<dyn EventLog> {
        Arc::new(self.stores().await)
    }
    async fn beacon(&self) -> Arc<dyn BeaconStore> {
        Arc::new(self.stores().await)
    }
}

mod log {
    use super::*;

    adk_server::edge_log_conformance_tests! {
        harness = File::new(),
    }
}

#[async_trait]
impl adk_server::awp::conformance::RateLimitHarness for File {
    async fn rate_limits(&self) -> Arc<dyn adk_server::awp::RateLimitStore> {
        Arc::new(self.stores().await)
    }
}

mod rate {
    use super::*;

    adk_server::rate_limit_conformance_tests! {
        harness = File::new(),
    }
}

struct Book(Arc<AtomicUsize>);

#[async_trait]
impl CapabilityHandler for Book {
    async fn handle(&self, _call: CapabilityCall) -> Result<Value, CapabilityError> {
        Ok(json!({ "booking": self.0.fetch_add(1, Ordering::SeqCst) + 1 }))
    }
}

async fn edge(
    file: &File,
    runs: &Arc<AtomicUsize>,
    approvals: &Arc<InMemoryApprovalStore>,
) -> AwpEdge {
    let mut registry = CapabilityRegistry::new("example.com");
    registry
        .declare(
            CapabilityV2::new("book_visit", "Book", CapabilityEffect::NonIdempotent)
                .with_min_trust(TrustTier::Known)
                .with_approval(ApprovalHint::AboveAmount(50_000)),
            Arc::new(Book(Arc::clone(runs))),
        )
        .unwrap();
    let keys = EdgeKeys {
        card: Arc::new(Ed25519DocumentSigner::from_seed("card", [1; 32]).unwrap()),
        receipt: Arc::new(Ed25519DocumentSigner::from_seed("receipt", [2; 32]).unwrap()),
    };
    AwpEdge::builder(registry, keys, Url::parse("https://agent.example.com").unwrap())
        .tiers(
            EdgeTiers::builder()
                .verifier(ApiKeyCredentials::new().key_secret("buyer", "s"))
                .build(),
        )
        .stores(file.stores().await.edge_stores())
        .approval_store(Arc::clone(approvals) as Arc<dyn adk_core::ApprovalStore>)
        .build()
        .unwrap()
}

async fn book(edge: &AwpEdge, key: &str, amount: u64) -> (StatusCode, Value) {
    let request = Request::post("https://agent.example.com/awp/v2/capabilities/book_visit")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "ApiKey buyer:s")
        .header("idempotency-key", key)
        .body(Body::from(json!({ "amount": amount }).to_string()))
        .unwrap();
    let response = edge.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// A restarted edge over the same file answers a repeat from its record and resolves the
/// held call the first one made, once the owner decides (spec 3.5 §7.4, A-I7).
#[tokio::test]
async fn an_edge_restarted_over_the_file_keeps_its_records() {
    let file = File::new();
    let runs = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let first = edge(&file, &runs, &approvals).await;
    let (status, booked) = book(&first, "k-1", 1_000).await;
    assert_eq!((status, &booked), (StatusCode::OK, &json!({ "booking": 1 })));
    let (status, held) = book(&first, "k-2", 90_000).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    drop(first);

    let second = edge(&file, &runs, &approvals).await;
    assert_eq!(book(&second, "k-1", 1_000).await, (StatusCode::OK, json!({ "booking": 1 })));
    let id = held["id"].as_str().unwrap();
    let scope = adk_core::ApprovalScope::new("awp:example.com", "key:buyer", id);
    let pending = adk_core::ApprovalStore::pending(approvals.as_ref(), &scope).await.unwrap();
    adk_core::ApprovalStore::decide(
        approvals.as_ref(),
        &scope,
        &pending[0].fingerprint,
        adk_core::ToolApproval::approve(),
    )
    .await
    .unwrap();
    let call = second.resolve_held(id).await.unwrap().unwrap();
    assert_eq!(call.state, awp_types::HeldCallState::Completed);
    assert_eq!(call.result, Some(json!({ "booking": 2 })));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

/// Stops a spawned server on every exit path.
struct Serving(tokio::task::JoinHandle<()>);

impl Drop for Serving {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A webhook receiver on loopback answering `status` and counting deliveries by id.
struct Receiver {
    url: String,
    hits: Arc<std::sync::Mutex<Vec<String>>>,
    status: Arc<std::sync::atomic::AtomicU16>,
    _serving: Serving,
}

impl Receiver {
    async fn start(status: u16) -> Self {
        let hits: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let answer = Arc::new(std::sync::atomic::AtomicU16::new(status));
        let (record, code) = (Arc::clone(&hits), Arc::clone(&answer));
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(move |headers: axum::http::HeaderMap| {
                let (record, code) = (Arc::clone(&record), Arc::clone(&code));
                async move {
                    let id = headers["webhook-id"].to_str().unwrap_or_default().to_owned();
                    record.lock().unwrap().push(id);
                    StatusCode::from_u16(code.load(Ordering::SeqCst)).unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let serving =
            Serving(tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }));
        Self { url, hits, status: answer, _serving: serving }
    }

    async fn wait_for(&self, count: usize) -> Vec<String> {
        for _ in 0..500 {
            if self.hits.lock().unwrap().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.hits.lock().unwrap().clone()
    }
}

/// An edge over `file`, whose `edge_stores()` carry the event log and the beacon store,
/// retrying a failed delivery after an hour.
async fn durable_edge(file: &File) -> AwpEdge {
    let stores = file.stores().await;
    let keys = EdgeKeys {
        card: Arc::new(Ed25519DocumentSigner::from_seed("card", [1; 32]).unwrap()),
        receipt: Arc::new(Ed25519DocumentSigner::from_seed("receipt", [2; 32]).unwrap()),
    };
    let mut events = EventsConfig::default()
        .egress(adk_core::webhook_delivery::EgressPolicy::allow_private_networks())
        .retry_schedule(vec![Duration::from_secs(3_600)]);
    events.delivery_lease = Duration::from_secs(5);
    AwpEdge::builder(
        CapabilityRegistry::new("example.com"),
        keys,
        Url::parse("https://agent.example.com").unwrap(),
    )
    .tiers(EdgeTiers::builder().verifier(ApiKeyCredentials::new().key_secret("buyer", "s")).build())
    .stores(stores.edge_stores())
    .events(events)
    .build()
    .unwrap()
}

async fn send(
    edge: &AwpEdge,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(format!("https://agent.example.com{path}"))
        .header(header::AUTHORIZATION, "ApiKey buyer:s");
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = edge.router().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A restarted edge over the same file keeps what event delivery and measurement need: the
/// release's further catalogue pages, the beacon's counts, and the retry a stopped process
/// was waiting on, which it delivers under the event's original id (spec 3.5 §7.5, §7.12).
#[tokio::test]
async fn a_restarted_edge_resumes_its_deliveries_and_keeps_its_counts() {
    let file = File::new();
    let receiver = Receiver::start(503).await;
    let first = durable_edge(&file).await;
    let subscription = adk_awp::v2::store::SubscriptionRecord {
        owner: awp_types::RequesterId::web_bot_auth("https://buyer.example", "k1"),
        subscription: awp_types::EventSubscription {
            id: "sub_restart".to_owned(),
            types: vec![awp_types::EventType::CatalogChanged],
            url: receiver.url.clone(),
            state: awp_types::SubscriptionState::Active,
            created_at: chrono::Utc::now(),
            failures: 0,
            secret: Some("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw".to_owned()),
        },
    };
    SubscriptionStore::create(&file.stores().await, subscription).await.unwrap();
    let items = (0..1_500).map(|n| awp_types::ContentId(format!("sku-{n}"))).collect();
    let event_id = first.events().catalog_changed(4, items).await.unwrap();
    let beacon = json!({ "kind": "pageview", "path": "/pricing" });
    assert_eq!(
        send(&first, "POST", "/awp/v2/beacon", Some(beacon)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(receiver.wait_for(1).await, vec![event_id.clone()], "the first attempt failed");
    // The edge keeps the failure after the receiver answers; read the file, without claiming
    // the delivery, until it has.
    let pool = file.pool().await;
    let mut kept = None;
    for _ in 0..250 {
        kept = sqlx::query_scalar::<_, i64>(
            "SELECT failures FROM adk_awp_deliveries WHERE event_id = ?",
        )
        .bind(&event_id)
        .fetch_optional(&pool)
        .await
        .unwrap();
        if kept == Some(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(kept, Some(1), "the failed attempt is kept in the file");

    // The failed delivery waits an hour in the file, leased to the process that tried it.
    let log = file.stores().await;
    let soon = chrono::Utc::now() + chrono::Duration::minutes(30);
    assert!(log.claim_deliveries(soon, chrono::Duration::seconds(5)).await.unwrap().is_empty());
    let later = chrono::Utc::now() + chrono::Duration::hours(2);
    let waiting = log.claim_deliveries(later, chrono::Duration::seconds(5)).await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!((waiting[0].event_id.as_str(), waiting[0].failures), (event_id.as_str(), 1));

    // The process stops; an hour passes; a new one starts over the file.
    drop(first);
    let mut overdue = waiting[0].clone();
    overdue.due = chrono::Utc::now() - chrono::Duration::seconds(1);
    log.save_delivery(&overdue).await.unwrap();
    receiver.status.store(200, Ordering::SeqCst);
    let second = durable_edge(&file).await;
    assert_eq!(second.events().resume_deliveries().await.unwrap(), 1);
    assert_eq!(receiver.wait_for(2).await, vec![event_id.clone(), event_id.clone()]);
    for _ in 0..100 {
        if log.claim_deliveries(later, chrono::Duration::seconds(5)).await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        log.claim_deliveries(later, chrono::Duration::seconds(5)).await.unwrap().is_empty(),
        "a delivered event is no longer pending"
    );

    let page = log.catalog_page(4, 2).await.unwrap().expect("the release's second page");
    assert_eq!(page.items[0].0, "sku-1000");
    let today = chrono::Utc::now().date_naive();
    let midnight = today.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let views = second
        .beacon()
        .read("pageviews:/pricing", midnight, midnight + chrono::Duration::days(1))
        .await
        .unwrap();
    assert_eq!(views, Some(1.0));
}

/// Two edges over one database file count a caller against one window, as replicas and a
/// restarted edge do (spec 3.5 §7.9).
#[tokio::test]
async fn two_edges_over_one_file_share_a_callers_window() {
    use adk_server::awp::EdgeLimits;
    use adk_server::awp::tiers::EdgeCaller;
    use awp_types::RateClass;

    let file = File::new();
    let class = RateClass { requests_per_minute: 2, calls_per_day: 1 };
    let limits = |stores: SqliteEdgeStores| {
        EdgeLimits::default()
            .tier_default(TrustTier::Public, class)
            .store(Arc::new(stores) as Arc<dyn adk_server::awp::RateLimitStore>)
    };
    let (first, second) = (limits(file.stores().await), limits(file.stores().await));
    let caller = EdgeCaller::public("ip:203.0.113.9");
    assert_eq!(first.admit_request(&caller).await.unwrap().remaining, 1);
    assert_eq!(second.admit_request(&caller).await.unwrap().remaining, 0);
    assert!(first.admit_request(&caller).await.is_err(), "the window is shared");
    assert!(second.admit_call(&caller, None).await.is_ok());
    assert!(first.admit_call(&caller, None).await.is_err(), "so is the daily call window");
    let restarted = limits(file.stores().await);
    assert!(restarted.admit_request(&caller).await.is_err(), "and it survives a restart");
    assert!(
        restarted.admit_request(&EdgeCaller::public("ip:198.51.100.7")).await.is_ok(),
        "another caller has its own"
    );
}
