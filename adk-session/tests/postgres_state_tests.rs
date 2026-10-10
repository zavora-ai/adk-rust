//! Shared state, concurrent writes, event filters, and schema detection of the
//! PostgreSQL session backend against a live server.
//!
//! Run against a scratch database at `DATABASE_URL` (default
//! `postgres://postgres@localhost:5432/postgres`). The tests create the session tables,
//! use a fresh app name per run, and drop the schemas they create:
//!
//! ```bash
//! DATABASE_URL=postgres://user@localhost/scratch \
//!   cargo nextest run -p adk-session --features postgres --run-ignored only postgres_state
//! ```

#![cfg(feature = "postgres")]

mod common;

use adk_session::{CreateRequest, PostgresSessionService, SessionService};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection};
use std::collections::HashMap;
use std::str::FromStr;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

async fn service() -> PostgresSessionService {
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(&database_url())
        .await
        .expect("connect to postgres");
    let service = PostgresSessionService::from_pool(pool);
    service.migrate().await.expect("migrate session tables");
    service
}

fn run_app(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn postgres_state_shared_tiers_are_read_at_get_time() {
    let service = service().await;
    common::state_contract::assert_shared_state_contract(
        &service,
        &run_app("shared"),
        "user1",
        "user2",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn postgres_state_concurrent_writes_keep_every_delta() {
    let service = service().await;
    common::state_contract::assert_concurrent_state_writes(
        &service,
        &run_app("concurrent"),
        "user1",
        12,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn postgres_state_event_filters() {
    let service = service().await;
    common::state_contract::assert_event_filter_contract(&service, &run_app("filters"), "user1")
        .await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn postgres_state_migrate_ignores_tables_in_other_schemas() {
    let mut admin = PgConnection::connect(&database_url()).await.expect("connect admin");
    let run = uuid::Uuid::new_v4().simple().to_string();
    let (other, target) = (format!("adk_other_{run}"), format!("adk_target_{run}"));
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA {other}; CREATE TABLE {other}.sessions (id INT); CREATE SCHEMA {target};"
    ))
    .execute(&mut admin)
    .await
    .expect("create schemas");

    let options = PgConnectOptions::from_str(&database_url())
        .expect("parse DATABASE_URL")
        .options([("search_path", target.as_str())]);
    let pool =
        PgPoolOptions::new().max_connections(2).connect_with(options).await.expect("connect");
    let service = PostgresSessionService::from_pool(pool);

    // A `sessions` table in another schema is not this schema's baseline.
    let result = async {
        service.migrate().await?;
        service
            .create(CreateRequest {
                app_name: "schema-app".to_string(),
                user_id: "user1".to_string(),
                session_id: None,
                state: HashMap::new(),
            })
            .await
    }
    .await;

    sqlx::raw_sql(&format!("DROP SCHEMA {other} CASCADE; DROP SCHEMA {target} CASCADE;"))
        .execute(&mut admin)
        .await
        .expect("drop schemas");
    result.expect("migrate creates the session tables in the current schema");
}
