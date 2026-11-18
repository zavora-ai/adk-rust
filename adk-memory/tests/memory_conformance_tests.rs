//! Every `MemoryService` backend runs the id and expiry conformance suite
//! (`adk_memory::conformance`).
//!
//! The in-memory, SQLite and knowledge-graph suites run everywhere. The suites that need a
//! server are `#[ignore]`d:
//!
//! | Backend | Server | Run |
//! |---------|--------|-----|
//! | PostgreSQL | `DATABASE_URL` with pgvector | CI's postgres job (`scripts/test-postgres.sh`) |
//! | Redis | `REDIS_URL` (default `redis://localhost:6379`) | `cargo nextest run -p adk-memory --features redis-memory --run-ignored only redis` |
//! | MongoDB | `MONGODB_URL` (default `mongodb://localhost:27017`) | `cargo nextest run -p adk-memory --features mongodb-memory --run-ignored only mongodb` |
//! | Neo4j | `NEO4J_URL`, `NEO4J_USER`, `NEO4J_PASSWORD` | `cargo nextest run -p adk-memory --features neo4j-memory --run-ignored only neo4j` |
//!
//! Vertex AI Memory Bank generates memories rather than storing entries, so its contract
//! tests (`memory_contract_vertex.rs`) cover ids and expiry against a mock server.

use adk_memory::MemoryService;
use adk_memory::conformance::MemoryServiceHarness;
use std::sync::Arc;
use std::time::Duration;

struct InMemory;

#[async_trait::async_trait]
impl MemoryServiceHarness for InMemory {
    async fn open(&self, default_ttl: Option<Duration>) -> Arc<dyn MemoryService> {
        let service = adk_memory::InMemoryMemoryService::new();
        Arc::new(match default_ttl {
            Some(ttl) => service.with_default_ttl(ttl),
            None => service,
        })
    }
}

mod in_memory {
    adk_memory::memory_conformance_tests!(harness = super::InMemory);
}

#[cfg(feature = "sqlite-memory")]
mod sqlite {
    use super::*;

    struct Sqlite;

    #[async_trait::async_trait]
    impl MemoryServiceHarness for Sqlite {
        async fn open(&self, default_ttl: Option<Duration>) -> Arc<dyn MemoryService> {
            let service =
                adk_memory::SqliteMemoryService::new("sqlite::memory:").await.expect("open");
            service.migrate().await.expect("migrate");
            Arc::new(match default_ttl {
                Some(ttl) => service.with_default_ttl(ttl),
                None => service,
            })
        }
    }

    adk_memory::memory_conformance_tests!(harness = Sqlite);
}

#[cfg(feature = "graph-memory")]
mod graph {
    use super::*;

    struct Graph;

    #[async_trait::async_trait]
    impl MemoryServiceHarness for Graph {
        async fn open(&self, default_ttl: Option<Duration>) -> Arc<dyn MemoryService> {
            let service =
                adk_memory::GraphMemoryService::new("sqlite::memory:").await.expect("open");
            service.migrate().await.expect("migrate");
            Arc::new(match default_ttl {
                Some(ttl) => service.with_default_ttl(ttl),
                None => service,
            })
        }
    }

    adk_memory::memory_conformance_tests!(harness = Graph);
}

#[cfg(feature = "redis-memory")]
mod redis {
    use super::*;

    struct Redis;

    #[async_trait::async_trait]
    impl MemoryServiceHarness for Redis {
        async fn open(&self, default_ttl: Option<Duration>) -> Arc<dyn MemoryService> {
            let url =
                std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
            let config = adk_memory::RedisMemoryConfig { url, ttl: default_ttl };
            Arc::new(adk_memory::RedisMemoryService::new(config).await.expect("connect to redis"))
        }
    }

    adk_memory::memory_conformance_tests!(
        #[ignore = "needs a Redis server at REDIS_URL"]
        harness = Redis
    );
}

#[cfg(feature = "database-memory")]
mod postgres {
    use super::*;
    use std::str::FromStr;

    struct Postgres;

    fn database_url() -> String {
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
    }

    #[async_trait::async_trait]
    impl MemoryServiceHarness for Postgres {
        /// A service in a schema of its own, keyword search only.
        async fn open(&self, default_ttl: Option<Duration>) -> Arc<dyn MemoryService> {
            use sqlx::Connection;
            let schema = format!("adk_memory_ttl_{}", uuid::Uuid::new_v4().simple());
            let mut admin = sqlx::PgConnection::connect(&database_url()).await.expect("connect");
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
            let options = sqlx::postgres::PgConnectOptions::from_str(&database_url())
                .expect("parse DATABASE_URL")
                .options([("search_path", search_path.as_str())]);
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(4)
                .connect_with(options)
                .await
                .expect("connect");
            let service = adk_memory::PostgresMemoryService::from_pool(pool, None);
            service.migrate().await.expect("migrate");
            Arc::new(match default_ttl {
                Some(ttl) => service.with_default_ttl(ttl),
                None => service,
            })
        }
    }

    adk_memory::memory_conformance_tests!(
        #[ignore = "needs DATABASE_URL with pgvector; CI's postgres job runs it"]
        harness = Postgres
    );
}

#[cfg(feature = "mongodb-memory")]
mod mongodb {
    use super::*;

    struct Mongo;

    #[async_trait::async_trait]
    impl MemoryServiceHarness for Mongo {
        /// A service in a database of its own.
        async fn open(&self, default_ttl: Option<Duration>) -> Arc<dyn MemoryService> {
            let url = std::env::var("MONGODB_URL")
                .unwrap_or_else(|_| "mongodb://localhost:27017".to_string());
            let client = ::mongodb::Client::with_uri_str(&url).await.expect("connect to mongodb");
            let database = format!("adk_memory_ttl_{}", uuid::Uuid::new_v4().simple());
            let service =
                adk_memory::MongoMemoryService::new(client, &database, None).expect("service");
            service.migrate().await.expect("migrate");
            Arc::new(match default_ttl {
                Some(ttl) => service.with_default_ttl(ttl),
                None => service,
            })
        }
    }

    adk_memory::memory_conformance_tests!(
        #[ignore = "needs a MongoDB server at MONGODB_URL"]
        harness = Mongo
    );
}

#[cfg(feature = "neo4j-memory")]
mod neo4j {
    use super::*;

    struct Neo4j;

    #[async_trait::async_trait]
    impl MemoryServiceHarness for Neo4j {
        async fn open(&self, default_ttl: Option<Duration>) -> Arc<dyn MemoryService> {
            let env = |name: &str, default: &str| {
                std::env::var(name).unwrap_or_else(|_| default.to_string())
            };
            let graph = neo4rs::Graph::new(
                env("NEO4J_URL", "bolt://localhost:7687"),
                env("NEO4J_USER", "neo4j"),
                env("NEO4J_PASSWORD", "password"),
            )
            .await
            .expect("connect to neo4j");
            let service = adk_memory::Neo4jMemoryService::new(graph, None).expect("service");
            service.migrate().await.expect("migrate");
            Arc::new(match default_ttl {
                Some(ttl) => service.with_default_ttl(ttl),
                None => service,
            })
        }
    }

    adk_memory::memory_conformance_tests!(
        #[ignore = "needs a Neo4j server at NEO4J_URL"]
        harness = Neo4j
    );
}
