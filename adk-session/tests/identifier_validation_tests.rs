//! Every session backend rejects an invalid app name, user ID, or session ID on every method,
//! before it reaches storage.
//!
//! The PostgreSQL, MongoDB, and Neo4j services here point at unreachable servers: a backend
//! that validates first answers `InvalidInput` at once, and one that does not returns its
//! connection error, which fails the contract. The SQLite, PostgreSQL, and Vertex AI contract
//! tests run the same checks through `assert_session_contract`, and the Redis ones need a
//! reachable server, since the client connects eagerly (`session_contract_redis.rs`).
//!
//! ```bash
//! cargo nextest run -p adk-session --features postgres,mongodb,neo4j,encrypted-session \
//!     --test identifier_validation_tests
//! ```

mod common;

use adk_session::InMemorySessionService;
use common::session_contract::assert_identifier_validation_contract;

#[tokio::test]
async fn in_memory_rejects_invalid_identifiers() {
    assert_identifier_validation_contract(&InMemorySessionService::new()).await;
}

#[cfg(feature = "encrypted-session")]
#[tokio::test]
async fn encrypted_session_rejects_invalid_identifiers() {
    use adk_session::{EncryptedSession, EncryptionKey};

    let service =
        EncryptedSession::new(InMemorySessionService::new(), EncryptionKey::generate(), vec![]);
    assert_identifier_validation_contract(&service).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_rejects_invalid_identifiers_before_connecting() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(500))
        .connect_lazy("postgres://adk@127.0.0.1:1/adk")
        .expect("a lazy pool parses the URL without connecting");
    let service = adk_session::PostgresSessionService::from_pool(pool);
    assert_identifier_validation_contract(&service).await;
}

#[cfg(feature = "mongodb")]
#[tokio::test]
async fn mongodb_rejects_invalid_identifiers_before_connecting() {
    let service = adk_session::MongoSessionService::new(
        "mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=500",
        "adk_identifier_validation",
    )
    .await
    .expect("the client is created without a reachable server");
    assert_identifier_validation_contract(&service).await;
}

#[cfg(feature = "neo4j")]
#[tokio::test]
async fn neo4j_rejects_invalid_identifiers_before_connecting() {
    let service = adk_session::Neo4jSessionService::new("127.0.0.1:1", "neo4j", "password")
        .await
        .expect("the connection pool is created without a reachable server");
    assert_identifier_validation_contract(&service).await;
}
