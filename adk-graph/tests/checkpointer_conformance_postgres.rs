//! The PostgreSQL checkpointer passes the shared conformance suite against a live
//! server.
//!
//! Every `open` is a new connection pool, as a restarted process would create. Cases
//! write under fresh identifiers, so they share one scratch database at `DATABASE_URL`
//! (default `postgres://postgres@localhost:5432/postgres`):
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-graph --features postgres --run-ignored only \
//!   --test checkpointer_conformance_postgres
//! ```

#![cfg(feature = "postgres")]

use std::sync::Arc;

use adk_graph::PostgresCheckpointer;
use adk_graph::checkpoint::Checkpointer;
use adk_graph::conformance::CheckpointerHarness;
use sqlx::postgres::PgPoolOptions;

struct Postgres;

#[async_trait::async_trait]
impl CheckpointerHarness for Postgres {
    async fn open(&self) -> Arc<dyn Checkpointer> {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string());
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect to postgres");
        let checkpointer = PostgresCheckpointer::from_pool(pool);
        checkpointer.migrate().await.expect("checkpoint schema migrates");
        Arc::new(checkpointer)
    }
}

adk_graph::checkpointer_conformance_tests!(
    #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
    harness = Postgres,
);
