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

async fn service() -> PostgresSessionService {
    let service = PostgresSessionService::new(&database_url()).await.expect("connect to postgres");
    service.migrate().await.expect("migrate session tables");
    service
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn test_postgres_service_contract() {
    let service = service().await;
    common::session_contract::assert_session_contract(
        &service,
        &run_app("contract"),
        &run_app("contract-other"),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn test_postgres_integrity_contract() {
    let service = service().await;
    let app = run_app("integrity");
    common::integrity_contract::assert_create_and_append_errors_contract(&service, &app).await;
    common::integrity_contract::assert_event_order_contract(&service, &app).await;
    common::integrity_contract::assert_nul_round_trip_contract(&service, &app).await;
    common::integrity_contract::assert_list_order_contract(&service, &app).await;
    common::integrity_contract::assert_rewind_unsupported(&service, &app).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn test_postgres_delete_all_sessions_erases_the_user_tier() {
    common::state_contract::assert_delete_all_sessions_contract(
        &service().await,
        &run_app("erase"),
        &run_app("erase-other"),
        true,
    )
    .await;
}

/// Opens the backend for the subject data store suite; every call is a new client.
struct Backend;

#[adk_core::async_trait]
impl common::subject_data::OpenSessions for Backend {
    async fn service(&self) -> std::sync::Arc<dyn adk_session::SessionService> {
        std::sync::Arc::new(service().await)
    }
}

mod subject_data {
    adk_core::subject_data_store_conformance_tests!(
        #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
        harness = crate::common::subject_data::SessionData(super::Backend),
    );
}
