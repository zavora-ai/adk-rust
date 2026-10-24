//! Approval stores written before requests were indexed by session open with the current
//! crate: `migrate` applies version 3 and fills each stored request's session from its
//! record, so the requests list by session (spec 3.3 §10 C1).
//!
//! The SQLite test runs by default; the PostgreSQL test creates and drops a database on
//! the server at `DATABASE_URL`:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-session --features sqlite,postgres --run-ignored all \
//!   --test approval_session_upgrade
//! ```

#![cfg(any(feature = "sqlite", feature = "postgres"))]

use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalRequestQuery, ApprovalScope, ApprovalStore,
    ApproverSelector, ToolConfirmationRequest,
};
use serde_json::json;

const APP: &str = "upgrade_app";
const SESSION: &str = "upgrade_session";

/// The approval schema at version 2, with its migration registry, as adk-session wrote it
/// before requests carried a session column.
fn version_2_schema(int_type: &str) -> String {
    format!(
        "\
CREATE TABLE adk_approval_pending (\
    app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
    fingerprint TEXT NOT NULL, request TEXT NOT NULL, requested_at_us BIGINT NOT NULL, \
    PRIMARY KEY (app_name, user_id, session_id, fingerprint));\
CREATE TABLE adk_approval_decisions (\
    app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
    fingerprint TEXT NOT NULL, approval TEXT NOT NULL, \
    PRIMARY KEY (app_name, user_id, session_id, fingerprint));\
CREATE TABLE adk_approval_requests (\
    id TEXT NOT NULL PRIMARY KEY, app_name TEXT NOT NULL, state TEXT NOT NULL, \
    open_flag BIGINT NOT NULL, created_at_us BIGINT NOT NULL, version BIGINT NOT NULL, \
    record TEXT NOT NULL);\
CREATE INDEX adk_approval_requests_open ON adk_approval_requests (open_flag, created_at_us, id);\
CREATE INDEX adk_approval_requests_app ON adk_approval_requests (app_name, created_at_us, id);\
CREATE TABLE adk_approval_delegations (\
    id TEXT NOT NULL PRIMARY KEY, to_kind TEXT NOT NULL, to_id TEXT NOT NULL, \
    record TEXT NOT NULL);\
CREATE INDEX adk_approval_delegations_to ON adk_approval_delegations (to_kind, to_id, id);\
CREATE TABLE _adk_approval_migrations (\
    version {int_type} PRIMARY KEY, description TEXT NOT NULL, applied_at TEXT NOT NULL);\
INSERT INTO _adk_approval_migrations VALUES \
    (1, 'create approval tables', '2026-10-01T00:00:00+00:00'), \
    (2, 'create approval request and delegation tables', '2026-10-01T00:00:00+00:00');"
    )
}

const INSERT_V2_REQUEST: &str = "\
INSERT INTO adk_approval_requests \
    (id, app_name, state, open_flag, created_at_us, version, record) \
VALUES ($1, $2, $3, $4, $5, $6, $7)";

fn request(id: &str, session: &str, offset_secs: i64) -> ApprovalRequest {
    let call = ToolConfirmationRequest::new("send_invoice", json!({ "invoice": id }))
        .with_function_call_id(format!("call-{id}"));
    let mut request = ApprovalRequest::new(
        id,
        ApprovalScope::new(APP, "upgrade_user", session),
        &call,
        ApprovalPolicy::new([ApproverSelector::human("upgrade_user")]),
    );
    request.created_at =
        chrono::DateTime::from_timestamp(1_790_000_000 + offset_secs, 0).expect("a valid instant");
    request
}

/// Lists the version 2 request by its session, and then the requests written after the
/// upgrade alongside it.
async fn assert_upgraded(store: &dyn ApprovalStore, legacy: &ApprovalRequest) {
    let by_session =
        ApprovalRequestQuery::default().with_app_name(APP).with_session_id(SESSION.to_string());
    assert_eq!(
        store.list_requests(&by_session).await.expect("list"),
        std::slice::from_ref(legacy),
        "the request stored at version 2 lists by its session"
    );
    let later = request("upgrade-later", SESSION, 60);
    let elsewhere = request("upgrade-elsewhere", "another_session", 30);
    store.create_request(&later).await.expect("create");
    store.create_request(&elsewhere).await.expect("create");
    assert_eq!(
        store.list_requests(&by_session).await.expect("list"),
        [legacy.clone(), later],
        "requests written after the upgrade list beside it"
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_version_2_sqlite_approval_store_lists_by_session_after_migrate() {
    use adk_session::SqliteApprovalStore;
    use sqlx::{Connection, SqliteConnection};

    let path = std::env::temp_dir()
        .join(format!("adk-approval-session-upgrade-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let legacy = request("upgrade-legacy", SESSION, 0);
    let mut conn = SqliteConnection::connect(&url).await.expect("create database");
    sqlx::raw_sql(sqlx::AssertSqlSafe(version_2_schema("INTEGER")))
        .execute(&mut conn)
        .await
        .expect("load the version 2 schema");
    sqlx::query(INSERT_V2_REQUEST)
        .bind(&legacy.id)
        .bind(APP)
        .bind(legacy.state.as_str())
        .bind(1_i64)
        .bind(legacy.created_at.timestamp_micros())
        .bind(1_i64)
        .bind(serde_json::to_string(&legacy).expect("encode"))
        .execute(&mut conn)
        .await
        .expect("insert a version 2 request");
    conn.close().await.expect("close");

    let store = SqliteApprovalStore::new(&url).await.expect("open");
    store.migrate().await.expect("migrate");
    store.migrate().await.expect("migrate is idempotent");
    let registry: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _adk_approval_migrations ORDER BY version")
            .fetch_all(store.pool())
            .await
            .expect("read registry");
    assert_eq!(registry, [1, 2, 3]);
    assert_upgraded(&store, &legacy).await;

    store.pool().close().await;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn a_version_2_postgres_approval_store_lists_by_session_after_migrate() {
    use adk_session::PostgresApprovalStore;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use sqlx::{Connection, PgConnection};
    use std::str::FromStr;

    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string());
    let database = format!("adk_approval_upgrade_{}", uuid::Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&url).await.expect("connect admin");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&mut admin)
        .await
        .expect("create database");
    let options = PgConnectOptions::from_str(&url).expect("parse DATABASE_URL").database(&database);

    let legacy = request("upgrade-legacy", SESSION, 0);
    let mut conn = PgConnection::connect_with(&options).await.expect("connect");
    sqlx::raw_sql(sqlx::AssertSqlSafe(version_2_schema("BIGINT")))
        .execute(&mut conn)
        .await
        .expect("load the version 2 schema");
    sqlx::query(INSERT_V2_REQUEST)
        .bind(&legacy.id)
        .bind(APP)
        .bind(legacy.state.as_str())
        .bind(1_i64)
        .bind(legacy.created_at.timestamp_micros())
        .bind(1_i64)
        .bind(serde_json::to_string(&legacy).expect("encode"))
        .execute(&mut conn)
        .await
        .expect("insert a version 2 request");
    conn.close().await.expect("close");

    let pool = PgPoolOptions::new().max_connections(4).connect_with(options).await.expect("pool");
    let store = PostgresApprovalStore::from_pool(pool.clone());
    store.migrate().await.expect("migrate");
    store.migrate().await.expect("migrate is idempotent");
    let registry: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _adk_approval_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("read registry");
    assert_eq!(registry, [1, 2, 3]);
    assert_upgraded(&store, &legacy).await;

    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE {database} WITH (FORCE)")))
        .execute(&mut admin)
        .await
        .expect("drop database");
}
