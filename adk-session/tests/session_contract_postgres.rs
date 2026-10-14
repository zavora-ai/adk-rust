//! The shared `SessionService` contract on the PostgreSQL backend.
//!
//! Run against a scratch database at `DATABASE_URL` (default
//! `postgres://postgres@localhost:5432/postgres`). Each run uses fresh app names:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-session --features postgres --run-ignored only \
//!   --test session_contract_postgres
//! ```

#![cfg(feature = "postgres")]

mod common;

use adk_session::PostgresSessionService;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

fn run_app(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn test_postgres_service_contract() {
    let service = PostgresSessionService::new(&database_url()).await.expect("connect to postgres");
    service.migrate().await.expect("migrate session tables");
    common::session_contract::assert_session_contract(
        &service,
        &run_app("contract"),
        &run_app("contract-other"),
    )
    .await;
}
