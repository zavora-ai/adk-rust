//! The SQLite wallet store (spec 3.5 §7.14, A-I16): the conformance suite, a restart, a cap
//! and a key shared by two instances on one database file, and its migration beside the
//! spend ledger's.
//!
//! Every `open` is a new pool over the file, as a restarted or second process would create:
//!
//! ```bash
//! cargo nextest run -p adk-payments --features sqlite --test wallet_store_sqlite
//! ```

#![cfg(feature = "sqlite")]

#[path = "support/wallet_stores.rs"]
mod wallet_stores;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{Clock, SpendLimits};
use adk_payments::wallet::{PaymentQuery, SqliteWalletStore, WalletStore};
use adk_session::SqliteSpendLedger;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use wallet_stores::{Backend, Instance};

/// A database file in a directory removed when the backend drops.
struct File {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl File {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("site.db");
        Self { _dir: dir, path }
    }

    async fn pool(&self) -> SqlitePool {
        let options = SqliteConnectOptions::new()
            .filename(&self.path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(30));
        SqlitePoolOptions::new().max_connections(4).connect_with(options).await.expect("open")
    }
}

#[async_trait::async_trait]
impl Backend for File {
    async fn open(&self, limits: SpendLimits, clock: Arc<dyn Clock>) -> Instance {
        let pool = self.pool().await;
        let store = SqliteWalletStore::new(pool.clone());
        store.migrate().await.expect("the wallet tables migrate");
        let ledger = SqliteSpendLedger::from_pool(pool).with_limits(limits).with_clock(clock);
        ledger.migrate().await.expect("the ledger table migrates");
        Instance { store: Arc::new(store), ledger: Arc::new(ledger) }
    }
}

#[tokio::test]
async fn the_sqlite_store_passes_the_conformance_suite() {
    wallet_stores::conformance(&File::new()).await;
}

#[tokio::test]
async fn an_in_memory_database_passes_the_conformance_suite() {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    let store = SqliteWalletStore::new(pool);
    store.migrate().await.unwrap();
    adk_payments::wallet::conformance::wallet_store_conformance(&store).await;
}

#[tokio::test]
async fn a_held_payment_and_a_paused_category_survive_a_restart() {
    wallet_stores::held_payment_and_pause_survive_a_restart(&File::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_instances_never_settle_past_a_cap() {
    wallet_stores::instances_never_settle_past_the_cap(&File::new(), 3, 30).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_pays_once_across_two_instances() {
    wallet_stores::a_key_pays_once_across_instances(&File::new(), 12).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_payment_is_decided_once_across_two_instances() {
    wallet_stores::a_held_payment_is_decided_once_across_instances(&File::new()).await;
}

#[tokio::test]
async fn migrating_twice_keeps_the_journal() {
    let file = File::new();
    let store = SqliteWalletStore::new(file.pool().await);
    store.migrate().await.unwrap();
    let pause = adk_payments::wallet::CategoryPause {
        wallet: "site-1".into(),
        category: adk_payments::wallet::SpendCategory::Models,
        reason: "burst".into(),
        paused_at: chrono::Utc::now(),
    };
    store.pause(pause.clone()).await.unwrap();
    let again = SqliteWalletStore::new(file.pool().await);
    again.migrate().await.expect("a second migration is a no-op");
    assert_eq!(again.pauses("site-1").await.unwrap(), vec![pause]);
    assert!(again.list(&PaymentQuery::new("site-1")).await.unwrap().is_empty());
    let steps: Vec<(i64,)> = sqlx::query_as("SELECT version FROM _adk_wallet_migrations")
        .fetch_all(again.pool())
        .await
        .unwrap();
    assert_eq!(steps, vec![(1,)], "each step is recorded once");
}
