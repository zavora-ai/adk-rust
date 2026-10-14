//! `PostgresMemoryService` add, search, and delete against a live server with pgvector.
//!
//! One test stores embeddings in a `vector` column with a direct index; the other
//! reports more than 2000 dimensions, so the index and the search use `halfvec` casts.
//! Each test runs in a fresh schema, which it drops; the extension itself lives in
//! `public`, so concurrent tests share it:
//!
//! ```bash
//! docker run -d -p 5432:5432 -e POSTGRES_PASSWORD=postgres pgvector/pgvector:pg17
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-memory --features database-memory --run-ignored only \
//!   --test postgres_memory_round_trip_tests
//! ```

#![cfg(feature = "database-memory")]

use std::str::FromStr;
use std::sync::Arc;

use adk_core::{Content, Part};
use adk_memory::{
    EmbeddingProvider, MemoryEntry, MemoryService, PostgresMemoryService, SearchRequest,
};
use chrono::Utc;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

/// Topics the fake embedder recognises; each maps to its own axis.
const TOPICS: [&str; 3] = ["apple", "rust", "tide"];

/// Embeds a text as the axes of the topics it mentions, plus a constant last axis so
/// no vector is zero. Deterministic, so search order is known in advance.
struct TopicEmbedder {
    dimensions: usize,
}

#[async_trait::async_trait]
impl EmbeddingProvider for TopicEmbedder {
    async fn embed(&self, texts: &[String]) -> adk_core::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut vector = vec![0.0; self.dimensions];
                for (axis, topic) in TOPICS.iter().enumerate() {
                    if text.to_lowercase().contains(topic) {
                        vector[axis] = 1.0;
                    }
                }
                vector[self.dimensions - 1] = 0.1;
                vector
            })
            .collect())
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

/// A schema of its own, with pgvector installed in `public` beforehand.
struct Scratch {
    admin: PgConnection,
    schema: String,
    pool: PgPool,
}

impl Scratch {
    async fn new() -> Self {
        let schema = format!("adk_memory_rt_{}", uuid::Uuid::new_v4().simple());
        let mut admin = PgConnection::connect(&database_url()).await.expect("connect admin");
        // `CREATE EXTENSION IF NOT EXISTS` races with itself, and an extension created
        // by `migrate` inside a scratch schema would be dropped with it.
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
        Self { admin, schema, pool }
    }

    async fn drop(mut self) {
        self.pool.close().await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {} CASCADE", self.schema)))
            .execute(&mut self.admin)
            .await
            .expect("drop schema");
    }
}

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("user").with_text(text),
        author: "user".to_string(),
        timestamp: Utc::now(),
    }
}

fn texts(memories: &[MemoryEntry]) -> Vec<String> {
    memories
        .iter()
        .flat_map(|memory| &memory.content.parts)
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

async fn search(service: &PostgresMemoryService, query: &str, limit: usize) -> Vec<String> {
    let response = service
        .search(SearchRequest {
            query: query.to_string(),
            user_id: "user".to_string(),
            app_name: "app".to_string(),
            limit: Some(limit),
            min_score: None,
            project_id: None,
        })
        .await
        .expect("search");
    texts(&response.memories)
}

/// Adds two sessions, searches by similarity, then deletes by keyword and by session.
async fn assert_round_trip(dimensions: usize) {
    let scratch = Scratch::new().await;
    let service = PostgresMemoryService::from_pool(
        scratch.pool.clone(),
        Some(Arc::new(TopicEmbedder { dimensions })),
    );
    service.migrate().await.expect("migrate");
    service.migrate().await.expect("migrate is idempotent");
    assert_eq!(service.schema_version().await.expect("schema version"), 2);

    service
        .add_session(
            "app",
            "user",
            "first",
            vec![entry("An apple pie recipe"), entry("Rust's borrow checker, explained")],
        )
        .await
        .expect("add first session");
    service
        .add_session("app", "user", "second", vec![entry("Tide tables for O'Brien's harbour")])
        .await
        .expect("add second session");

    assert_eq!(search(&service, "apple", 1).await, ["An apple pie recipe"]);
    assert_eq!(search(&service, "tide", 1).await, ["Tide tables for O'Brien's harbour"]);
    assert_eq!(search(&service, "rust", 3).await.len(), 3);

    assert_eq!(service.delete_entries("app", "user", "borrow").await.expect("delete"), 1);
    assert_eq!(search(&service, "rust", 3).await.len(), 2);

    service.delete_session("app", "user", "second").await.expect("delete session");
    assert_eq!(search(&service, "tide", 3).await, ["An apple pie recipe"]);

    service.delete_user("app", "user").await.expect("delete user");
    assert_eq!(search(&service, "apple", 3).await, Vec::<String>::new());

    scratch.drop().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server with the pgvector extension at DATABASE_URL"]
async fn postgres_memory_round_trip_with_vector_index() {
    assert_round_trip(8).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server with the pgvector extension at DATABASE_URL"]
async fn postgres_memory_round_trip_with_halfvec_index() {
    assert_round_trip(2048).await;
}
