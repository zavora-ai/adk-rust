//! The PostgreSQL spend ledger enforces limits across pooled connections.
//!
//! Run against a scratch database at `DATABASE_URL` (default
//! `postgres://postgres@localhost:5432/postgres`). Each test uses a fresh organization:
//!
//! ```bash
//! DATABASE_URL=postgres://user@localhost/scratch \
//!   cargo nextest run -p adk-session --features postgres --run-ignored only spend_ledger_postgres
//! ```

#![cfg(feature = "postgres")]

use std::sync::Arc;

use adk_core::{SPEND_LIMIT_EXCEEDED_CODE, SpendKey, SpendLedger, SpendLimits, SpendPeriod};
use adk_session::PostgresSpendLedger;
use sqlx::postgres::PgPoolOptions;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

async fn ledger(limits: SpendLimits) -> PostgresSpendLedger {
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(&database_url())
        .await
        .expect("connect to postgres");
    let ledger = PostgresSpendLedger::from_pool(pool).with_limits(limits);
    ledger.migrate().await.expect("migrate ledger tables");
    ledger
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn concurrent_reservations_never_exceed_a_daily_cap() {
    let org = format!("org-{}", uuid::Uuid::new_v4());
    let ledger = Arc::new(
        ledger(SpendLimits::new().limit(SpendKey::org(&org).per(SpendPeriod::Day), 1_000)).await,
    );

    let tasks: Vec<_> = (0..40)
        .map(|i| {
            let ledger = ledger.clone();
            let key = SpendKey::org(&org).with_agent(format!("agent-{}", i % 4));
            tokio::spawn(async move { ledger.reserve(&key, 100).await })
        })
        .collect();
    let mut granted = 0;
    for task in tasks {
        match task.await.expect("task completes") {
            Ok(_) => granted += 1,
            Err(error) => assert_eq!(error.code, SPEND_LIMIT_EXCEEDED_CODE, "{error}"),
        }
    }
    assert_eq!(granted, 10);
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn committed_spend_is_summed_per_vendor() {
    let org = format!("org-{}", uuid::Uuid::new_v4());
    let ledger = ledger(SpendLimits::new()).await;
    let gemini = SpendKey::org(&org).with_vendor("gemini");
    let held = ledger.reserve(&gemini, 10).await.unwrap();
    ledger.commit(held, 25).await.unwrap();
    let released = ledger.reserve(&SpendKey::org(&org).with_vendor("openai"), 10).await.unwrap();
    ledger.release(released).await.unwrap();

    assert_eq!(ledger.spent(&gemini, SpendPeriod::Day).await.unwrap(), 25);
    assert_eq!(
        ledger.spent(&SpendKey::org(&org).with_vendor("openai"), SpendPeriod::Day).await.unwrap(),
        0
    );
}
