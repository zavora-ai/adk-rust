//! The PostgreSQL wallet store (spec 3.5 §7.14, A-I16): the conformance suite, a restart, a
//! cap and a key shared by two instances, and instances migrating one database together.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them
//! (`scripts/test-postgres.sh`). Each test works in a scratch schema of its own, which it
//! drops, and every `open` is a new pool over it, as a restarted or second instance would
//! create:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-payments --features sqlite,postgres,redis \
//!   --run-ignored only --test wallet_store_postgres
//! ```

#![cfg(feature = "postgres")]

#[path = "support/wallet_stores.rs"]
mod wallet_stores;

use std::str::FromStr;
use std::sync::Arc;

use adk_core::{Clock, SpendLimits};
use adk_payments::wallet::PostgresWalletStore;
use adk_session::PostgresSpendLedger;
use sqlx::Connection as _;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use wallet_stores::{Backend, Instance};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

/// A scratch schema, created once on first use and dropped by [`Scratch::drop_schema`].
struct Scratch {
    schema: String,
    created: tokio::sync::OnceCell<()>,
}

impl Scratch {
    fn new() -> Self {
        Self {
            schema: format!("adk_wallet_{}", uuid::Uuid::new_v4().simple()),
            created: tokio::sync::OnceCell::new(),
        }
    }

    async fn admin(&self, statement: String) {
        let mut admin = sqlx::PgConnection::connect(&database_url()).await.expect("connect");
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement)).execute(&mut admin).await.expect("admin");
        let _ = admin.close().await;
    }

    async fn pool(&self) -> PgPool {
        self.created
            .get_or_init(|| self.admin(format!("CREATE SCHEMA IF NOT EXISTS {}", self.schema)))
            .await;
        let options = PgConnectOptions::from_str(&database_url())
            .expect("parse DATABASE_URL")
            .options([("search_path", self.schema.as_str())]);
        PgPoolOptions::new().max_connections(4).connect_with(options).await.expect("connect")
    }

    async fn drop_schema(self) {
        self.admin(format!("DROP SCHEMA IF EXISTS {} CASCADE", self.schema)).await;
    }
}

#[async_trait::async_trait]
impl Backend for Scratch {
    async fn open(&self, limits: SpendLimits, clock: Arc<dyn Clock>) -> Instance {
        let pool = self.pool().await;
        let store = PostgresWalletStore::new(pool.clone());
        store.migrate().await.expect("the wallet tables migrate");
        let ledger = PostgresSpendLedger::from_pool(pool).with_limits(limits).with_clock(clock);
        ledger.migrate().await.expect("the ledger tables migrate");
        Instance { store: Arc::new(store), ledger: Arc::new(ledger) }
    }
}

#[tokio::test]
#[ignore = "needs PostgreSQL: DATABASE_URL"]
async fn the_postgres_store_passes_the_conformance_suite() {
    let scratch = Scratch::new();
    wallet_stores::conformance(&scratch).await;
    scratch.drop_schema().await;
}

#[tokio::test]
#[ignore = "needs PostgreSQL: DATABASE_URL"]
async fn a_held_payment_and_a_paused_category_survive_a_restart() {
    let scratch = Scratch::new();
    wallet_stores::held_payment_and_pause_survive_a_restart(&scratch).await;
    scratch.drop_schema().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: DATABASE_URL"]
async fn two_instances_never_settle_past_a_cap() {
    let scratch = Scratch::new();
    wallet_stores::instances_never_settle_past_the_cap(&scratch, 4, 40).await;
    scratch.drop_schema().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: DATABASE_URL"]
async fn a_key_pays_once_across_two_instances() {
    let scratch = Scratch::new();
    wallet_stores::a_key_pays_once_across_instances(&scratch, 16).await;
    scratch.drop_schema().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs PostgreSQL: DATABASE_URL"]
async fn instances_migrate_one_database_together() {
    let scratch = Scratch::new();
    let stores: Vec<_> = futures::future::join_all(
        (0..4).map(|_| async { PostgresWalletStore::new(scratch.pool().await) }),
    )
    .await;
    let migrated = futures::future::join_all(stores.iter().map(PostgresWalletStore::migrate)).await;
    for result in migrated {
        result.expect("every instance migrates");
    }
    let steps: Vec<(i64,)> = sqlx::query_as("SELECT version FROM _adk_wallet_migrations")
        .fetch_all(stores[0].pool())
        .await
        .unwrap();
    assert_eq!(steps, vec![(1,)], "each step is recorded once");
    scratch.drop_schema().await;
}
