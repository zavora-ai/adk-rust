//! Conformance suites over every long-term fact store and index (spec 3.1 §11
//! "Long-term memory"): `fact_store_conformance_tests!` over in-memory, file and SQLite;
//! `fact_index_conformance_tests!` over `LocalFactIndex` (lexical and embedded) and
//! `MemoryServiceIndex` on the in-memory, SQLite FTS5 and PostgreSQL pgvector backends.
//!
//! The PostgreSQL suite is `#[ignore]`d and runs in CI's postgres job
//! (`scripts/test-postgres.sh`):
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-harness --run-ignored only --test fact_conformance_tests
//! ```

#![cfg(feature = "memory")]

use std::sync::Arc;

use adk_harness::{FactStore, InMemoryFactStore, LocalFactIndex, MemoryServiceIndex};

/// Embeds a text as a bag of hashed words, so cosine similarity follows shared words.
struct WordEmbedder;

const DIMENSIONS: usize = 1024;

#[async_trait::async_trait]
impl adk_memory::EmbeddingProvider for WordEmbedder {
    async fn embed(&self, texts: &[String]) -> adk_core::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut vector = vec![0.0; DIMENSIONS];
                for word in adk_memory::text::extract_words(text) {
                    let hash = word.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
                    });
                    vector[(hash % DIMENSIONS as u64) as usize] += 1.0;
                }
                vector
            })
            .collect())
    }

    fn dimensions(&self) -> usize {
        DIMENSIONS
    }
}

mod store_in_memory {
    adk_harness::fact_store_conformance_tests!(store = adk_harness::InMemoryFactStore::new());
}

mod store_file {
    adk_harness::fact_store_conformance_tests!(
        store = adk_harness::FileFactStore::new(tempfile::tempdir().unwrap().keep())
    );

    #[tokio::test]
    async fn facts_survive_a_reopen_in_a_private_file() {
        use adk_harness::{
            FactKind, FactScope, FactSource, FactStatus, FactStore, FileFactStore, MemoryFact,
            Provenance,
        };
        let dir = tempfile::tempdir().unwrap();
        let fact = MemoryFact::new(
            FactScope::new("app", "alice", None),
            FactKind::Preference,
            "Use pnpm, not npm",
            FactStatus::Known,
            Provenance::new(FactSource::UserStated, "s1", "inv-1"),
        );
        FileFactStore::new(dir.path()).put(fact.clone()).await.unwrap();
        assert_eq!(FileFactStore::new(dir.path()).get(&fact.id).await.unwrap(), Some(fact));
        let file = std::fs::read_dir(dir.path()).unwrap().next().unwrap().unwrap();
        assert!(!file.file_name().to_string_lossy().contains("alice"), "no identifier in a path");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}

#[cfg(feature = "sqlite")]
mod store_sqlite {
    adk_harness::fact_store_conformance_tests!(
        store = {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .unwrap();
            let store = adk_harness::SqliteFactStore::new(pool);
            store.migrate().await.unwrap();
            store.migrate().await.unwrap();
            store
        }
    );
}

fn in_memory_store() -> Arc<InMemoryFactStore> {
    Arc::new(InMemoryFactStore::new())
}

mod index_lexical {
    use super::*;
    adk_harness::fact_index_conformance_tests!(
        setup = {
            let store = in_memory_store();
            (Arc::new(LocalFactIndex::new(store.clone())), store)
        }
    );
}

mod index_embedded {
    use super::*;
    adk_harness::fact_index_conformance_tests!(
        setup = {
            let store = in_memory_store();
            (
                Arc::new(LocalFactIndex::new(store.clone()).with_embedder(Arc::new(WordEmbedder))),
                store,
            )
        }
    );

    #[tokio::test]
    async fn embeddings_are_stored_with_the_facts_and_backfilled() {
        use adk_harness::{
            FactIndex, FactKind, FactScope, FactSource, FactStatus, MemoryFact, Provenance,
        };
        let store = in_memory_store();
        let scope = FactScope::new("app", "alice", Some("p1".into()));
        let fact = MemoryFact::new(
            scope.clone(),
            FactKind::ProjectFact,
            "the tests need sqlite",
            FactStatus::Assumed,
            Provenance::new(FactSource::AgentInferred, "s", "i"),
        );
        // Stored before an embedder existed: the first search embeds it.
        store.put(fact.clone()).await.unwrap();
        let index = LocalFactIndex::new(store.clone()).with_embedder(Arc::new(WordEmbedder));
        assert!(store.embeddings(&scope).await.unwrap().is_empty());
        let hits = index.search(&scope, "sqlite tests", 5).await.unwrap();
        assert_eq!(hits[0].0, fact.id);
        assert_eq!(store.embeddings(&scope).await.unwrap().len(), 1);
    }
}

mod index_memory_service_in_memory {
    use super::*;
    adk_harness::fact_index_conformance_tests!(
        setup = {
            (
                Arc::new(MemoryServiceIndex::new(Arc::new(
                    adk_memory::InMemoryMemoryService::new(),
                ))),
                in_memory_store(),
            )
        }
    );
}

mod index_memory_service_sqlite {
    use super::*;
    adk_harness::fact_index_conformance_tests!(
        setup = {
            let service =
                adk_memory::sqlite::SqliteMemoryService::new("sqlite::memory:").await.unwrap();
            service.migrate().await.unwrap();
            (Arc::new(MemoryServiceIndex::new(Arc::new(service))), in_memory_store())
        }
    );
}

mod index_memory_service_postgres {
    use std::str::FromStr;

    use super::*;

    fn database_url() -> String {
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
    }

    /// A PostgreSQL memory service with pgvector in a schema of its own.
    async fn service() -> adk_memory::PostgresMemoryService {
        use sqlx::Connection;
        let schema = format!("adk_harness_facts_{}", uuid::Uuid::new_v4().simple());
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
        let service =
            adk_memory::PostgresMemoryService::from_pool(pool, Some(Arc::new(WordEmbedder)));
        service.migrate().await.expect("migrate");
        service
    }

    adk_harness::fact_index_conformance_tests!(
        #[ignore = "needs DATABASE_URL with pgvector; CI's postgres job runs it"]
        setup = {
            let index = MemoryServiceIndex::new(Arc::new(service().await)).with_min_score(0.3);
            (Arc::new(index), in_memory_store())
        }
    );
}
