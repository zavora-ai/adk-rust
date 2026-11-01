//! `MemorySubjectData` passes `adk_core::subject_data_store_conformance_tests!` on every
//! memory backend: after an erasure no read path — `export_user`, a global search, a
//! project search — returns the subject's memories, and other users' memories stay.
//!
//! The PostgreSQL suite is `#[ignore]`d and runs in CI's postgres job
//! (`scripts/test-postgres.sh`); the Redis, MongoDB, and Neo4j suites are `#[ignore]`d and
//! run in the nightly store-conformance job against `REDIS_URL`, `MONGODB_URL`, and
//! `NEO4J_URI`. The Vertex AI Memory Bank backend is covered against a mock in
//! `memory_contract_vertex.rs`.

use std::sync::Arc;

use adk_core::conformance::SubjectDataStoreHarness;
use adk_core::subject::SubjectDataStore;
use adk_core::{Content, async_trait};
use adk_memory::{MemoryEntry, MemoryService, MemorySubjectData, SearchRequest};

const PROJECT: &str = "conformance-project";

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("user").with_text(text),
        author: "user".to_string(),
        timestamp: chrono::Utc::now(),
    }
}

struct Harness(Arc<dyn MemoryService>);

impl Harness {
    fn new(service: impl MemoryService + 'static) -> Self {
        Self(Arc::new(service))
    }

    async fn search(&self, app_name: &str, user_id: &str, project_id: Option<&str>) -> usize {
        self.0
            .search(SearchRequest {
                query: "nairobi".to_string(),
                user_id: user_id.to_string(),
                app_name: app_name.to_string(),
                limit: Some(50),
                min_score: None,
                project_id: project_id.map(str::to_string),
            })
            .await
            .expect("search")
            .memories
            .len()
    }
}

#[async_trait]
impl SubjectDataStoreHarness for Harness {
    async fn open(&self) -> Arc<dyn SubjectDataStore> {
        Arc::new(MemorySubjectData::new(Arc::clone(&self.0)))
    }

    async fn seed(&self, app_name: &str, user_id: &str) {
        self.0
            .add_session(app_name, user_id, "session-1", vec![entry("Hometown: Nairobi.")])
            .await
            .expect("add session");
        if self.0.supports_project_scoping() {
            self.0
                .add_entry_to_project(app_name, user_id, PROJECT, entry("Nairobi office plans."))
                .await
                .expect("add project entry");
        }
    }

    async fn visible(&self, app_name: &str, user_id: &str) -> usize {
        let exported = self.0.export_user(app_name, user_id).await.expect("export").len();
        let projects = if self.0.supports_project_scoping() {
            self.search(app_name, user_id, Some(PROJECT)).await
        } else {
            0
        };
        exported + self.search(app_name, user_id, None).await + projects
    }
}

mod in_memory {
    adk_core::subject_data_store_conformance_tests!(
        harness = super::Harness::new(adk_memory::InMemoryMemoryService::new())
    );
}

#[cfg(feature = "sqlite-memory")]
mod sqlite {
    async fn harness() -> super::Harness {
        let service =
            adk_memory::sqlite::SqliteMemoryService::new("sqlite::memory:").await.unwrap();
        service.migrate().await.unwrap();
        super::Harness::new(service)
    }

    adk_core::subject_data_store_conformance_tests!(harness = harness().await);
}

#[cfg(feature = "graph-memory")]
mod graph {
    async fn harness() -> super::Harness {
        let service = adk_memory::GraphMemoryService::new("sqlite::memory:").await.unwrap();
        service.migrate().await.unwrap();
        super::Harness::new(service)
    }

    adk_core::subject_data_store_conformance_tests!(harness = harness().await);
}

#[cfg(feature = "database-memory")]
mod postgres {
    use std::str::FromStr;

    fn database_url() -> String {
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
    }

    /// A service in a schema of its own, keyword search only.
    async fn harness() -> super::Harness {
        use sqlx::Connection;
        let schema = format!("adk_memory_subject_{}", uuid::Uuid::new_v4().simple());
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
        super::Harness::new(service)
    }

    adk_core::subject_data_store_conformance_tests!(
        #[ignore = "needs DATABASE_URL with pgvector; CI's postgres job runs it"]
        harness = harness().await
    );
}

#[cfg(feature = "redis-memory")]
mod redis {
    async fn harness() -> super::Harness {
        let url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
        let service =
            adk_memory::RedisMemoryService::new(adk_memory::RedisMemoryConfig { url, ttl: None })
                .await
                .expect("connect to redis");
        super::Harness::new(service)
    }

    adk_core::subject_data_store_conformance_tests!(
        #[ignore = "needs a Redis server at REDIS_URL; the nightly store-conformance job runs it"]
        harness = harness().await
    );
}

#[cfg(feature = "mongodb-memory")]
mod mongodb {
    async fn harness() -> super::Harness {
        let url = std::env::var("MONGODB_URL")
            .unwrap_or_else(|_| "mongodb://localhost:27017".to_string());
        let client = ::mongodb::Client::with_uri_str(&url).await.expect("connect to mongodb");
        let database = format!("adk_memory_subject_{}", uuid::Uuid::new_v4().simple());
        let service =
            adk_memory::MongoMemoryService::new(client, &database, None).expect("service");
        service.migrate().await.expect("migrate");
        super::Harness::new(service)
    }

    adk_core::subject_data_store_conformance_tests!(
        #[ignore = "needs MongoDB at MONGODB_URL; the nightly store-conformance job runs it"]
        harness = harness().await
    );
}

#[cfg(feature = "neo4j-memory")]
mod neo4j {
    async fn harness() -> super::Harness {
        let var =
            |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.to_string());
        let graph = neo4rs::Graph::new(
            var("NEO4J_URI", "bolt://localhost:7687"),
            var("NEO4J_USER", "neo4j"),
            var("NEO4J_PASSWORD", "password"),
        )
        .await
        .expect("connect to neo4j");
        let service = adk_memory::Neo4jMemoryService::new(graph, None).expect("service");
        service.migrate().await.expect("migrate");
        super::Harness::new(service)
    }

    adk_core::subject_data_store_conformance_tests!(
        #[ignore = "needs Neo4j at NEO4J_URI; the nightly store-conformance job runs it"]
        harness = harness().await
    );
}
