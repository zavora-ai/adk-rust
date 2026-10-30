//! Conformance suites over every long-term fact store and index (spec 3.1 §11
//! "Long-term memory"): `fact_store_conformance_tests!` over in-memory, file, SQLite and
//! PostgreSQL (feature `postgres`, spec 3.3 §12 phase 3); `fact_index_conformance_tests!` over
//! `LocalFactIndex` (lexical and embedded) and `MemoryServiceIndex` on the in-memory, SQLite
//! FTS5 and PostgreSQL pgvector backends.
//!
//! The PostgreSQL suites are `#[ignore]`d and run in CI's postgres job
//! (`scripts/test-postgres.sh`):
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-harness --features postgres --run-ignored only \
//!   --test fact_conformance_tests
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

    /// A database written before embedder ids gains the column; its vectors read back
    /// untagged, so recall embeds them again (spec 3.1 §7.32, I73).
    #[tokio::test]
    async fn vectors_stored_before_embedder_ids_read_back_untagged() {
        use adk_harness::{
            FactEmbedding, FactKind, FactScope, FactSource, FactStatus, FactStore, MemoryFact,
            Provenance, SqliteFactStore,
        };
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE adk_harness_fact_embeddings (id TEXT PRIMARY KEY, vector TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let store = SqliteFactStore::new(pool.clone());
        store.migrate().await.unwrap();
        let scope = FactScope::new("app", "alice", None);
        let fact = store
            .put(MemoryFact::new(
                scope.clone(),
                FactKind::Preference,
                "Use pnpm",
                FactStatus::Known,
                Provenance::new(FactSource::UserStated, "s1", "inv-1"),
            ))
            .await
            .unwrap();
        sqlx::query("INSERT INTO adk_harness_fact_embeddings (id, vector) VALUES (?, '[0.6,0.8]')")
            .bind(&fact.id)
            .execute(&pool)
            .await
            .unwrap();
        store.migrate().await.unwrap();
        assert_eq!(
            store.embeddings(&scope).await.unwrap(),
            vec![(fact.id.clone(), FactEmbedding::new("", vec![0.6, 0.8]))]
        );
        let local = FactEmbedding::new("onnx/BAAI/bge-small-en-v1.5", vec![1.0, 0.0]);
        store.put_embedding(&fact.id, local.clone()).await.unwrap();
        assert_eq!(store.embeddings(&scope).await.unwrap(), vec![(fact.id, local)]);
    }

    /// In `harness.db` the kernel's stores write beside the facts, as the run store's claims
    /// under `BEGIN IMMEDIATE` do: a fact stored or changed while another connection holds the
    /// write lock waits for it, so a consolidation tick does not fail with `database is
    /// locked` (spec 3.3 §12 phase 3).
    #[tokio::test]
    async fn a_change_waits_for_another_writer_on_the_file() {
        use std::time::Duration;

        use adk_core::principal::SubjectRef;
        use adk_harness::{
            FactKind, FactScope, FactSource, FactStatus, FactStore, MemoryFact, Provenance,
            SqliteFactStore,
        };
        use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

        let dir = tempfile::tempdir().unwrap();
        let options = SqliteConnectOptions::new()
            .filename(dir.path().join("harness.db"))
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        // One connection, opened before the lock is taken, so both changes meet the lock
        // inside their transactions rather than while connecting.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .unwrap();
        let store = SqliteFactStore::new(pool);
        store.migrate().await.unwrap();
        let fact = |text: &str| {
            MemoryFact::new(
                FactScope::new("app", "alice", None),
                FactKind::Preference,
                text,
                FactStatus::Assumed,
                Provenance::new(FactSource::AgentInferred, "s1", "inv-1"),
            )
        };
        let kept = store.put(fact("Invoices go out on the last working day.")).await.unwrap();

        // The other writer commits a change while both wait: a deferred transaction that read
        // before it could not write after it.
        let other = SqlitePoolOptions::new().connect_with(options).await.unwrap();
        let mut held = other.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("CREATE TABLE another_writer (id INTEGER)").execute(&mut *held).await.unwrap();
        let merged = fact("Invoices go out on the last working day of the month.");
        let put = tokio::spawn({
            let (store, merged) = (store.clone(), merged.clone());
            async move { store.put(merged).await }
        });
        let superseded = tokio::spawn({
            let (store, id) = (store.clone(), kept.id.clone());
            let status = FactStatus::Superseded { by: merged.id.clone() };
            async move { store.set_status(&id, status, &SubjectRef::human("alice")).await }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        held.commit().await.unwrap();

        assert_eq!(put.await.unwrap().unwrap(), merged);
        superseded.await.unwrap().unwrap();
        assert_eq!(
            store.get(&kept.id).await.unwrap().unwrap().status,
            FactStatus::Superseded { by: merged.id }
        );
    }
}

/// The fact store on PostgreSQL, the shared store of `adk-rust up --store postgres://…`
/// (spec 3.3 §12 phase 3), each test in a scratch schema of its own.
#[cfg(feature = "postgres")]
mod store_postgres {
    use std::str::FromStr;
    use std::sync::Arc;

    use adk_harness::{
        FactFilter, FactKind, FactScope, FactSource, FactStatus, FactStore, MemoryFact,
        PostgresFactStore, Provenance,
    };
    use sqlx::Connection as _;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    fn database_url() -> String {
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
    }

    /// Connection options whose search path is a fresh scratch schema.
    async fn scratch() -> PgConnectOptions {
        let schema = format!("adk_harness_fact_store_{}", uuid::Uuid::new_v4().simple());
        let mut admin = sqlx::PgConnection::connect(&database_url()).await.expect("connect");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .expect("create schema");
        PgConnectOptions::from_str(&database_url())
            .expect("parse DATABASE_URL")
            .options([("search_path", schema.as_str())])
    }

    /// A store with a pool of its own, as a replica holds, migrated.
    async fn store(options: &PgConnectOptions) -> PostgresFactStore {
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options.clone())
            .await
            .expect("connect");
        let store = PostgresFactStore::new(pool);
        store.migrate().await.unwrap();
        store
    }

    adk_harness::fact_store_conformance_tests!(
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        store = {
            let options = scratch().await;
            let store = store(&options).await;
            store.migrate().await.unwrap();
            store
        }
    );

    /// Two replicas storing one fact at the same moment keep one fact, used twice, and a
    /// status one replica sets is what the other reads.
    #[tokio::test]
    #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
    async fn two_replicas_storing_one_fact_keep_one() {
        let options = scratch().await;
        let (a, b) = (Arc::new(store(&options).await), Arc::new(store(&options).await));
        let fact = || {
            MemoryFact::new(
                FactScope::new("app", "alice", Some("p1".into())),
                FactKind::Preference,
                "Use pnpm, not npm",
                FactStatus::Assumed,
                Provenance::new(FactSource::AgentInferred, "s1", "inv-1"),
            )
        };
        let mut puts = Vec::new();
        for replica in [&a, &b, &a, &b] {
            let (replica, fact) = (replica.clone(), fact());
            puts.push(tokio::spawn(async move { replica.put(fact).await.unwrap() }));
        }
        let mut ids = Vec::new();
        for put in puts {
            ids.push(put.await.unwrap().id);
        }
        ids.dedup();
        assert_eq!(ids.len(), 1, "one fact: {ids:?}");
        let listed = b.list(FactFilter::user("app", "alice")).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].uses, 3, "the three later puts used it");
        a.set_status(
            &ids[0],
            FactStatus::Forgotten,
            &adk_core::principal::SubjectRef::human("alice"),
        )
        .await
        .unwrap();
        assert_eq!(b.get(&ids[0]).await.unwrap().unwrap().status, FactStatus::Forgotten);
    }
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

    /// A vector another embedder produced is embedded again, even when the two models'
    /// vectors share a length, instead of being compared with this embedder's query
    /// (spec 3.1 §7.32, I73).
    #[tokio::test]
    async fn a_vector_from_another_embedder_is_embedded_again() {
        use adk_harness::{
            FactEmbedding, FactIndex, FactKind, FactScope, FactSource, FactStatus, MemoryFact,
            Provenance,
        };
        let store = in_memory_store();
        let scope = FactScope::new("app", "alice", Some("p1".into()));
        let fact = store
            .put(MemoryFact::new(
                scope.clone(),
                FactKind::ProjectFact,
                "the tests need sqlite",
                FactStatus::Assumed,
                Provenance::new(FactSource::AgentInferred, "s", "i"),
            ))
            .await
            .unwrap();
        // Another model's vector, of this embedder's length, pointing nowhere near the query.
        let mut foreign = vec![0.0; DIMENSIONS];
        foreign[DIMENSIONS - 1] = 1.0;
        let foreign = FactEmbedding::new("gemini/gemini-embedding-2", foreign);
        store.put_embedding(&fact.id, foreign).await.unwrap();

        let index = LocalFactIndex::new(store.clone()).with_embedder(Arc::new(WordEmbedder));
        let hits = index.search(&scope, "sqlite tests", 5).await.unwrap();
        assert_eq!(hits.len(), 1, "the fact is found by this embedder's vector: {hits:?}");
        assert_eq!(hits[0].0, fact.id);
        let stored = store.embeddings(&scope).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].1.embedder, format!("unnamed/{DIMENSIONS}"));
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
