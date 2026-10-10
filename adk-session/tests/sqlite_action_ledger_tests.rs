//! The SQLite action ledger keeps begun and completed records across a restart.
#![cfg(feature = "sqlite")]

use adk_core::{ActionLedger, ActionOutcome, ActionRecord, ToolEffect};
use adk_session::SqliteActionLedger;
use serde_json::json;

async fn memory_ledger() -> SqliteActionLedger {
    let ledger = SqliteActionLedger::new("sqlite::memory:").await.unwrap();
    ledger.migrate().await.unwrap();
    ledger
}

fn record(key: &str) -> ActionRecord {
    ActionRecord::new(key, "pay", &json!({"amount": 50}), ToolEffect::NonIdempotent)
}

#[tokio::test]
async fn a_record_round_trips_through_begin_and_complete() {
    let ledger = memory_ledger().await;
    let begun = record("k1");
    ledger.begin(&begun).await.unwrap();

    let stored = ledger.get("k1").await.unwrap().unwrap();
    assert_eq!(stored.key, begun.key);
    assert_eq!(stored.tool_name, "pay");
    assert_eq!(stored.args_digest, begun.args_digest);
    assert_eq!(stored.effect, ToolEffect::NonIdempotent);
    assert_eq!(stored.started_at.timestamp_micros(), begun.started_at.timestamp_micros());
    assert_eq!(stored.outcome, None);

    let outcome = ActionOutcome::Succeeded { result_digest: "fnv1a128:ab".into() };
    ledger.complete("k1", outcome.clone()).await.unwrap();
    let completed = ledger.get("k1").await.unwrap().unwrap();
    assert_eq!(completed.outcome, Some(outcome));
    assert!(completed.completed_at.is_some());
}

#[tokio::test]
async fn begin_and_complete_are_write_once() {
    let ledger = memory_ledger().await;
    ledger.begin(&record("k1")).await.unwrap();
    let duplicate = ledger.begin(&record("k1")).await.unwrap_err();
    assert_eq!(duplicate.code, "tool.action_ledger.duplicate_key");

    let failed = ActionOutcome::Failed { error: "declined".into() };
    ledger.complete("k1", failed.clone()).await.unwrap();
    let again = ledger.complete("k1", failed).await.unwrap_err();
    assert_eq!(again.code, "tool.action_ledger.already_completed");

    let missing = ledger
        .complete("absent", ActionOutcome::Succeeded { result_digest: String::new() })
        .await
        .unwrap_err();
    assert!(missing.is_not_found());
    assert_eq!(ledger.get("absent").await.unwrap(), None);
}

#[tokio::test]
async fn a_begun_record_survives_a_restart() {
    let path = std::env::temp_dir().join(format!("adk-ledger-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite:{}?mode=rwc", path.display());
    {
        let ledger = SqliteActionLedger::new(&url).await.unwrap();
        ledger.migrate().await.unwrap();
        ledger.begin(&record("crash-key")).await.unwrap();
    }

    let reopened = SqliteActionLedger::new(&url).await.unwrap();
    reopened.migrate().await.unwrap();
    let record = reopened.get("crash-key").await.unwrap().expect("record persisted");
    assert!(!record.is_completed(), "a crash mid-call leaves the record begun");
    let _ = std::fs::remove_file(&path);
}
