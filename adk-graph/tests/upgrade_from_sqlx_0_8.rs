//! A checkpoint database written by adk-graph 2.3.0, the last SQLx 0.8 release, opens
//! with the current crate: the checkpoint reads back with a backfilled version, and the
//! thread continues from it.
//!
//! The fixture and the program that wrote it are described in
//! `tests/fixtures/sqlx-0.8/README.md`. adk-graph 2.3.0 had no PostgreSQL checkpointer.

#![cfg(feature = "sqlite")]

use adk_graph::{Checkpointer, SqliteCheckpointer};
use serde_json::json;
use sqlx::{Connection, SqliteConnection};

#[tokio::test]
async fn a_2_3_sqlite_checkpoint_database_upgrades() {
    let path = std::env::temp_dir().join(format!(
        "adk-graph-upgrade-{}-{}.db",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let mut conn = SqliteConnection::connect(&url).await.expect("create database");
    sqlx::raw_sql(include_str!("fixtures/sqlx-0.8/checkpoints_sqlite.sql"))
        .execute(&mut conn)
        .await
        .expect("load the 2.3.0 fixture");
    conn.close().await.expect("close");

    let checkpointer = SqliteCheckpointer::new(&url).await.expect("open");
    let loaded = checkpointer
        .load("upgrade_thread")
        .await
        .expect("load")
        .expect("the 2.3.0 checkpoint is readable");
    assert_eq!(
        (
            loaded.checkpoint_id.as_str(),
            loaded.step,
            loaded.version,
            loaded.pending_nodes.clone(),
            loaded.state.get("note").cloned(),
            loaded.state.get("count").cloned(),
        ),
        (
            "upgrade_checkpoint",
            3,
            1,
            vec!["review".to_string()],
            Some(json!("O'Brien")),
            Some(json!(2))
        )
    );

    let mut next = loaded.clone();
    next.checkpoint_id = "after_upgrade".to_string();
    next.step = 4;
    next.version = loaded.version + 1;
    next.pending_nodes = vec![];
    next.state.insert("count".to_string(), json!(3));
    checkpointer.save(&next).await.expect("save after the upgrade");
    assert!(checkpointer.save(&loaded).await.is_err(), "a stale version is refused");

    let reopened = SqliteCheckpointer::new(&url).await.expect("reopen");
    let ids: Vec<String> = reopened
        .list("upgrade_thread")
        .await
        .expect("list")
        .into_iter()
        .map(|checkpoint| checkpoint.checkpoint_id)
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(
        ids.contains(&"upgrade_checkpoint".to_string())
            && ids.contains(&"after_upgrade".to_string())
    );
    let head = reopened.load("upgrade_thread").await.expect("load").expect("head");
    assert_eq!((head.checkpoint_id.as_str(), head.version), ("after_upgrade", 2));

    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
