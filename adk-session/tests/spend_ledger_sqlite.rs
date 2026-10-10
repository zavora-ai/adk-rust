//! The SQLite spend ledger enforces limits across pooled connections and restarts.

#![cfg(feature = "sqlite")]

use std::sync::Arc;
use std::time::Duration;

use adk_core::{SPEND_LIMIT_EXCEEDED_CODE, SpendKey, SpendLedger, SpendLimits, SpendPeriod};
use adk_session::SqliteSpendLedger;

/// A database file, so pooled connections contend for the write lock as separate
/// processes would.
struct FileDatabase {
    path: std::path::PathBuf,
}

impl FileDatabase {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("adk-spend-{}.db", uuid::Uuid::new_v4()));
        Self { path }
    }

    async fn ledger(&self, limits: SpendLimits) -> SqliteSpendLedger {
        let url = format!("sqlite://{}?mode=rwc", self.path.display());
        let ledger = SqliteSpendLedger::new(&url).await.expect("SQLite opens").with_limits(limits);
        ledger.migrate().await.expect("ledger schema migrates");
        ledger
    }
}

impl Drop for FileDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

fn daily_cap(max: u64) -> SpendLimits {
    SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), max)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reservations_never_exceed_a_daily_cap() {
    let database = FileDatabase::new();
    let ledger = Arc::new(database.ledger(daily_cap(1_000)).await);

    let tasks: Vec<_> = (0..40)
        .map(|i| {
            let ledger = ledger.clone();
            tokio::spawn(async move {
                let key = SpendKey::org("acme").with_agent(format!("agent-{}", i % 4));
                ledger.reserve(&key, 100).await
            })
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
async fn committed_spend_survives_a_restart_and_counts_against_the_cap() {
    let database = FileDatabase::new();
    let key = SpendKey::org("acme").with_agent("researcher").with_vendor("gemini");
    {
        let ledger = database.ledger(daily_cap(100)).await;
        let held = ledger.reserve(&key, 50).await.unwrap();
        ledger.commit(held, 70).await.unwrap();
    }

    let ledger = database.ledger(daily_cap(100)).await;
    assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap(), 70);
    assert_eq!(
        ledger
            .spent(&SpendKey::org("acme").with_vendor("openai"), SpendPeriod::Month)
            .await
            .unwrap(),
        0
    );
    let error = ledger.reserve(&key, 31).await.unwrap_err();
    assert_eq!(error.code, SPEND_LIMIT_EXCEEDED_CODE);
    ledger.reserve(&key, 30).await.expect("30 fits under the cap");
}

#[tokio::test]
async fn an_expired_reservation_stops_holding_budget() {
    let database = FileDatabase::new();
    let ledger =
        database.ledger(daily_cap(100)).await.with_reservation_ttl(Duration::from_millis(20));
    let key = SpendKey::org("acme");

    let abandoned = ledger.reserve(&key, 100).await.unwrap();
    assert!(ledger.reserve(&key, 1).await.is_err());
    tokio::time::sleep(Duration::from_millis(60)).await;
    let next = ledger.reserve(&key, 100).await.expect("the expired hold is released");

    ledger.release(next.clone()).await.unwrap();
    assert_eq!(ledger.release(next).await.unwrap_err().code, "spend.unknown_reservation");
    ledger.commit(abandoned, 40).await.expect("a late commit still records the spend");
    assert_eq!(ledger.spent(&key, SpendPeriod::Lifetime).await.unwrap(), 40);
}
