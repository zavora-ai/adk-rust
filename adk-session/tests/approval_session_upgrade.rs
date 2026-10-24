//! Approval stores written before requests were indexed by user and session open with the
//! current crate: `migrate` applies versions 3 and 4 and fills each stored request's user
//! and session from its record, so the requests list by session — application, user and
//! session ID (spec 3.3 §10 C1).
//!
//! Each backend starts from version 2 (no session column) and from version 3 (a session
//! column, no user column). The SQLite tests run by default; the PostgreSQL ones create
//! and drop a database on the server at `DATABASE_URL`:
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
const USER: &str = "upgrade_user";
const SESSION: &str = "upgrade_session";

/// The approval schema at `version` (2 or 3), with its migration registry, as adk-session
/// wrote it before requests carried a user column.
fn schema(version: u8, int_type: &str) -> String {
    let session_column = if version >= 3 { "session_id TEXT, " } else { "" };
    let mut sql = format!(
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
    id TEXT NOT NULL PRIMARY KEY, app_name TEXT NOT NULL, {session_column}state TEXT NOT NULL, \
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
    );
    if version >= 3 {
        sql.push_str(
            "\
CREATE INDEX adk_approval_requests_session \
    ON adk_approval_requests (session_id, created_at_us, id);\
INSERT INTO _adk_approval_migrations VALUES \
    (3, 'index approval requests by session', '2026-10-24T00:00:00+00:00');",
        );
    }
    sql
}

/// The insert of one request row at `version`; parameters as in [`bind_row`].
fn insert(version: u8) -> &'static str {
    if version >= 3 {
        "INSERT INTO adk_approval_requests \
            (id, app_name, state, open_flag, created_at_us, version, record, session_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
    } else {
        "INSERT INTO adk_approval_requests \
            (id, app_name, state, open_flag, created_at_us, version, record) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)"
    }
}

fn request(id: &str, user: &str, session: &str, offset_secs: i64) -> ApprovalRequest {
    let call = ToolConfirmationRequest::new("send_invoice", json!({ "invoice": id }))
        .with_function_call_id(format!("call-{id}"));
    let mut request = ApprovalRequest::new(
        id,
        ApprovalScope::new(APP, user, session),
        &call,
        ApprovalPolicy::new([ApproverSelector::human("owner")]),
    );
    request.created_at =
        chrono::DateTime::from_timestamp(1_790_000_000 + offset_secs, 0).expect("a valid instant");
    request
}

/// The requests stored before the upgrade: one of the session, one of another user's
/// session with the same ID.
fn legacy() -> [ApprovalRequest; 2] {
    [
        request("upgrade-legacy", USER, SESSION, 0),
        request("upgrade-legacy-other-user", "another_user", SESSION, 5),
    ]
}

/// Lists the stored requests by their session, and then the requests written after the
/// upgrade alongside them.
async fn assert_upgraded(store: &dyn ApprovalStore, legacy: &[ApprovalRequest; 2]) {
    let by_session =
        ApprovalRequestQuery::default().with_session(&ApprovalScope::new(APP, USER, SESSION));
    assert_eq!(
        store.list_requests(&by_session).await.expect("list"),
        std::slice::from_ref(&legacy[0]),
        "the stored request lists by its session, without the other user's"
    );
    let later = request("upgrade-later", USER, SESSION, 60);
    let elsewhere = request("upgrade-elsewhere", USER, "another_session", 30);
    store.create_request(&later).await.expect("create");
    store.create_request(&elsewhere).await.expect("create");
    assert_eq!(
        store.list_requests(&by_session).await.expect("list"),
        [legacy[0].clone(), later],
        "requests written after the upgrade list beside it"
    );
    let other_user = ApprovalRequestQuery::default().with_session(&ApprovalScope::new(
        APP,
        "another_user",
        SESSION,
    ));
    assert_eq!(
        store.list_requests(&other_user).await.expect("list"),
        std::slice::from_ref(&legacy[1]),
        "the other user's session lists alone"
    );
}

#[cfg(feature = "sqlite")]
async fn sqlite_upgrade(version: u8) {
    use adk_session::SqliteApprovalStore;
    use sqlx::{Connection, SqliteConnection};

    let path = std::env::temp_dir()
        .join(format!("adk-approval-session-upgrade-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let legacy = legacy();
    let mut conn = SqliteConnection::connect(&url).await.expect("create database");
    sqlx::raw_sql(sqlx::AssertSqlSafe(schema(version, "INTEGER")))
        .execute(&mut conn)
        .await
        .expect("load the old schema");
    for request in &legacy {
        let mut row = sqlx::query(insert(version))
            .bind(&request.id)
            .bind(APP)
            .bind(request.state.as_str())
            .bind(1_i64)
            .bind(request.created_at.timestamp_micros())
            .bind(1_i64)
            .bind(serde_json::to_string(request).expect("encode"));
        if version >= 3 {
            row = row.bind(&request.scope.session_id);
        }
        row.execute(&mut conn).await.expect("insert an old request");
    }
    conn.close().await.expect("close");

    let store = SqliteApprovalStore::new(&url).await.expect("open");
    store.migrate().await.expect("migrate");
    store.migrate().await.expect("migrate is idempotent");
    let registry: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _adk_approval_migrations ORDER BY version")
            .fetch_all(store.pool())
            .await
            .expect("read registry");
    assert_eq!(registry, [1, 2, 3, 4]);
    assert_upgraded(&store, &legacy).await;

    store.pool().close().await;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_version_2_sqlite_approval_store_lists_by_session_after_migrate() {
    sqlite_upgrade(2).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_version_3_sqlite_approval_store_lists_by_session_after_migrate() {
    sqlite_upgrade(3).await;
}

#[cfg(feature = "postgres")]
async fn postgres_upgrade(version: u8) {
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

    let legacy = legacy();
    let mut conn = PgConnection::connect_with(&options).await.expect("connect");
    sqlx::raw_sql(sqlx::AssertSqlSafe(schema(version, "BIGINT")))
        .execute(&mut conn)
        .await
        .expect("load the old schema");
    for request in &legacy {
        let mut row = sqlx::query(insert(version))
            .bind(&request.id)
            .bind(APP)
            .bind(request.state.as_str())
            .bind(1_i64)
            .bind(request.created_at.timestamp_micros())
            .bind(1_i64)
            .bind(serde_json::to_string(request).expect("encode"));
        if version >= 3 {
            row = row.bind(&request.scope.session_id);
        }
        row.execute(&mut conn).await.expect("insert an old request");
    }
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
    assert_eq!(registry, [1, 2, 3, 4]);
    assert_upgraded(&store, &legacy).await;

    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE {database} WITH (FORCE)")))
        .execute(&mut admin)
        .await
        .expect("drop database");
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn a_version_2_postgres_approval_store_lists_by_session_after_migrate() {
    postgres_upgrade(2).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn a_version_3_postgres_approval_store_lists_by_session_after_migrate() {
    postgres_upgrade(3).await;
}
