//! The SQLite edge stores (spec 3.5 §7.9): the edge store conformance suite, and an edge that
//! restarts over the same database file and still answers its held call and its idempotency
//! record.
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
use adk_server::awp::stores::SqliteEdgeStores;
use adk_server::awp::tiers::ApiKeyCredentials;
use adk_server::awp::{AwpEdge, EdgeKeys, EdgeTiers};
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
