//! Databases written by adk-session 2.3.0, the last SQLx 0.8 release, open with the
//! current crate: the data reads back, the migration registry continues without a
//! repeated or failed step, and new writes land.
//!
//! The fixtures and the program that wrote them are in `tests/fixtures/sqlx-0.8/`. The
//! SQLite test runs by default; the PostgreSQL test creates and drops a database on the
//! server at `DATABASE_URL`:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-session --features sqlite,postgres --run-ignored all \
//!   --test upgrade_from_sqlx_0_8
//! ```

#![cfg(any(feature = "sqlite", feature = "postgres"))]

use adk_core::{AdkIdentity, AppName, Content, Event, SessionId, UserId};
use adk_session::{AppendEventRequest, CreateRequest, GetRequest, SessionService};
use serde_json::json;

const APP: &str = "upgrade_app";
const USER: &str = "upgrade_user";
const SESSION: &str = "upgrade_session";

fn texts(session: &dyn adk_session::Session) -> Vec<String> {
    let events = session.events();
    (0..events.len())
        .filter_map(|index| events.at(index))
        .filter_map(|event| event.content().cloned())
        .flat_map(|content| content.parts)
        .filter_map(|part| part.text().map(str::to_string))
        .collect()
}

async fn get(service: &dyn SessionService) -> Box<dyn adk_session::Session> {
    service
        .get(GetRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: SESSION.into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("read the 2.3.0 session")
}

/// Reads the 2.3.0 session, appends to it, and creates a new one.
async fn assert_upgraded(service: &dyn SessionService) {
    let session = get(service).await;
    assert_eq!(texts(session.as_ref()), ["When is high tide?", "High tide is at 06:12."]);
    for (key, value) in [
        ("app:plan", json!("team")),
        ("user:name", json!("O'Brien")),
        ("topic", json!("tides")),
        ("turns", json!(2)),
    ] {
        assert_eq!(session.state().get(key), Some(value), "state key {key}");
    }

    let mut event = Event::new("invocation-2");
    event.author = "user".to_string();
    event.llm_response.content = Some(Content::new("user").with_text("And low tide?"));
    event.actions.state_delta.insert("turns".to_string(), json!(3));
    let identity = AdkIdentity::new(
        AppName::new(APP).expect("app name"),
        UserId::new(USER).expect("user id"),
        SessionId::new(SESSION).expect("session id"),
    );
    service
        .append_event_for_identity(AppendEventRequest { identity, event })
        .await
        .expect("append to the 2.3.0 session");

    let session = get(service).await;
    assert_eq!(texts(session.as_ref()).last().map(String::as_str), Some("And low tide?"));
    assert_eq!(session.state().get("turns"), Some(json!(3)));

    let created = service
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: None,
            state: Default::default(),
        })
        .await
        .expect("create a session after the upgrade");
    // App and user state written by 2.3.0 reach new sessions.
    assert_eq!(created.state().get("app:plan"), Some(json!("team")));
    assert_eq!(created.state().get("user:name"), Some(json!("O'Brien")));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_2_3_sqlite_database_upgrades() {
    use adk_session::SqliteSessionService;
    use sqlx::{Connection, SqliteConnection};

    let path =
        std::env::temp_dir().join(format!("adk-session-upgrade-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let mut conn = SqliteConnection::connect(&url).await.expect("create database");
    sqlx::raw_sql(include_str!("fixtures/sqlx-0.8/session_sqlite.sql"))
        .execute(&mut conn)
        .await
        .expect("load the 2.3.0 fixture");
    conn.close().await.expect("close");

    let service = SqliteSessionService::new(&url).await.expect("open");
    service.migrate().await.expect("migrate");
    service.migrate().await.expect("migrate is idempotent");
    assert_eq!(service.schema_version().await.expect("schema version"), 1);
    assert_upgraded(&service).await;

    let mut conn = SqliteConnection::connect(&url).await.expect("connect");
    let registry: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _adk_session_migrations ORDER BY version")
            .fetch_all(&mut conn)
            .await
            .expect("read registry");
    assert_eq!(registry, [1]);
    conn.close().await.expect("close");
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn a_2_3_postgres_database_upgrades() {
    use adk_session::PostgresSessionService;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use sqlx::{Connection, PgConnection};
    use std::str::FromStr;

    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string());
    let database = format!("adk_session_upgrade_{}", uuid::Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&url).await.expect("connect admin");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&mut admin)
        .await
        .expect("create database");
    let options = PgConnectOptions::from_str(&url).expect("parse DATABASE_URL").database(&database);

    let mut conn = PgConnection::connect_with(&options).await.expect("connect");
    sqlx::raw_sql(include_str!("fixtures/sqlx-0.8/session_postgres.sql"))
        .execute(&mut conn)
        .await
        .expect("load the 2.3.0 fixture");
    conn.close().await.expect("close");

    let pool = PgPoolOptions::new().max_connections(4).connect_with(options).await.expect("pool");
    let service = PostgresSessionService::from_pool(pool.clone());
    service.migrate().await.expect("migrate");
    service.migrate().await.expect("migrate is idempotent");
    assert_eq!(service.schema_version().await.expect("schema version"), 1);
    assert_upgraded(&service).await;
    let registry: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _adk_session_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("read registry");
    assert_eq!(registry, [1]);

    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE {database} WITH (FORCE)")))
        .execute(&mut admin)
        .await
        .expect("drop database");
}
