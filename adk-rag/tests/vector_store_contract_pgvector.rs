//! Shared [`VectorStore`] contract suite run against the pgvector backend.
//!
//! Needs a PostgreSQL server with the pgvector extension available at `DATABASE_URL`,
//! such as the `pgvector/pgvector:pg17` image. The suite runs in a fresh schema, which
//! it drops; the extension itself lives in `public`, so concurrent suites share it:
//!
//! ```bash
//! docker run -d -p 5432:5432 -e POSTGRES_PASSWORD=postgres pgvector/pgvector:pg17
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-rag --features pgvector --run-ignored only \
//!   --test vector_store_contract_pgvector
//! ```

#![cfg(feature = "pgvector")]

mod common;

use std::str::FromStr;

use adk_rag::pgvector::PgVectorStore;
use common::vector_store_contract::{ContractOptions, assert_vector_store_contract};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a PostgreSQL server with the pgvector extension at DATABASE_URL"]
async fn test_pgvector_vector_store_contract() {
    let schema = format!(
        "rag_contract_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_nanos()
    );
    let mut admin = PgConnection::connect(&database_url()).await.expect("connect admin");
    // `CREATE EXTENSION IF NOT EXISTS` races with itself, and an extension created in
    // a scratch schema would be dropped with it.
    let mut tx = admin.begin().await.expect("begin");
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('adk_test_pgvector_extension'))")
        .execute(&mut *tx)
        .await
        .expect("lock");
    sqlx::query("CREATE EXTENSION IF NOT EXISTS vector SCHEMA public")
        .execute(&mut *tx)
        .await
        .expect("pgvector is available");
    tx.commit().await.expect("commit");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut admin)
        .await
        .expect("create schema");

    let search_path = format!("{schema}, public");
    let options = PgConnectOptions::from_str(&database_url())
        .expect("parse DATABASE_URL")
        .options([("search_path", search_path.as_str())]);
    let pool =
        PgPoolOptions::new().max_connections(4).connect_with(options).await.expect("connect");
    let store = PgVectorStore::from_pool(pool.clone());

    assert_vector_store_contract(&store, ContractOptions::default()).await;

    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&mut admin)
        .await
        .expect("drop schema");
}
