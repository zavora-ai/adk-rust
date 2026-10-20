//! Every `MemoryService` backend round-trips `author` and honours `project_id` in
//! `add_entry_to_project`, `search` and `delete_entries_in_project` (spec 3.1 §10 M2).
//!
//! The harness's `MemoryServiceIndex` links each long-term fact to its entry by `author`
//! and scopes it by project, and removes it with `delete_entries_in_project` on the fact
//! id it carries as a second text part. The PostgreSQL suite is `#[ignore]`d and runs in
//! CI's postgres job (`scripts/test-postgres.sh`).

use adk_core::{Content, Part};
use adk_memory::{MemoryEntry, MemoryService, SearchRequest};

const APP: &str = "m2-app";
const USER: &str = "m2-user";

fn entry(author: &str, parts: &[&str]) -> MemoryEntry {
    MemoryEntry {
        content: Content {
            role: "user".to_string(),
            parts: parts.iter().map(|text| Part::Text { text: (*text).to_string() }).collect(),
        },
        author: author.to_string(),
        timestamp: chrono::Utc::now(),
    }
}

async fn authors(service: &dyn MemoryService, query: &str, project: Option<&str>) -> Vec<String> {
    let mut authors: Vec<String> = service
        .search(SearchRequest {
            query: query.to_string(),
            user_id: USER.to_string(),
            app_name: APP.to_string(),
            limit: Some(20),
            min_score: None,
            project_id: project.map(str::to_string),
        })
        .await
        .expect("search")
        .memories
        .into_iter()
        .map(|memory| memory.author)
        .collect();
    authors.sort();
    authors
}

/// `author` and every text part come back unchanged from a project search.
async fn author_round_trips(service: &dyn MemoryService) {
    assert!(service.supports_project_scoping());
    service
        .add_entry_to_project(
            APP,
            USER,
            "p1",
            entry(
                "adk-harness:fact:01J0000000000000000000000A",
                &["pnpm installs dependencies", "01J0000000000000000000000A"],
            ),
        )
        .await
        .unwrap();
    let found = service
        .search(SearchRequest {
            query: "pnpm".to_string(),
            user_id: USER.to_string(),
            app_name: APP.to_string(),
            limit: Some(5),
            min_score: None,
            project_id: Some("p1".to_string()),
        })
        .await
        .unwrap()
        .memories;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].author, "adk-harness:fact:01J0000000000000000000000A");
    let texts: Vec<&str> = found[0].content.parts.iter().filter_map(Part::text).collect();
    assert_eq!(texts, ["pnpm installs dependencies", "01J0000000000000000000000A"]);
}

/// A project search sees its project and the global entries; a global search only the
/// global ones; no search sees another project.
async fn search_honours_the_project(service: &dyn MemoryService) {
    service
        .add_entry_to_project(APP, USER, "p1", entry("one", &["release checklist alpha"]))
        .await
        .unwrap();
    service
        .add_entry_to_project(APP, USER, "p2", entry("two", &["release checklist beta"]))
        .await
        .unwrap();
    service.add_entry(APP, USER, entry("global", &["release checklist shared"])).await.unwrap();
    assert_eq!(authors(service, "release", Some("p1")).await, ["global", "one"]);
    assert_eq!(authors(service, "release", Some("p2")).await, ["global", "two"]);
    assert_eq!(authors(service, "release", None).await, ["global"]);
}

/// `delete_entries_in_project` removes the matching entries of that project only,
/// matching on any text part.
async fn delete_honours_the_project(service: &dyn MemoryService) {
    service
        .add_entry_to_project(
            APP,
            USER,
            "p1",
            entry("a", &["cache warmup", "01JAAAAAAAAAAAAAAAAAAAAAAA"]),
        )
        .await
        .unwrap();
    service
        .add_entry_to_project(
            APP,
            USER,
            "p1",
            entry("b", &["cache eviction", "01JBBBBBBBBBBBBBBBBBBBBBBB"]),
        )
        .await
        .unwrap();
    service
        .add_entry_to_project(
            APP,
            USER,
            "p2",
            entry("c", &["cache warmup", "01JAAAAAAAAAAAAAAAAAAAAAAA"]),
        )
        .await
        .unwrap();
    service
        .add_entry(APP, USER, entry("d", &["cache warmup", "01JAAAAAAAAAAAAAAAAAAAAAAA"]))
        .await
        .unwrap();
    let removed = service
        .delete_entries_in_project(APP, USER, "p1", "01JAAAAAAAAAAAAAAAAAAAAAAA")
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(authors(service, "cache", Some("p1")).await, ["b", "d"]);
    assert_eq!(authors(service, "cache", Some("p2")).await, ["c", "d"]);
}

macro_rules! memory_service_conformance_tests {
    ($(#[$attr:meta])* service = $service:expr $(,)?) => {
        $(#[$attr])*
        #[tokio::test]
        async fn m2_author_round_trips() {
            let service = $service;
            super::author_round_trips(&service).await;
        }
        $(#[$attr])*
        #[tokio::test]
        async fn m2_search_honours_the_project() {
            let service = $service;
            super::search_honours_the_project(&service).await;
        }
        $(#[$attr])*
        #[tokio::test]
        async fn m2_delete_honours_the_project() {
            let service = $service;
            super::delete_honours_the_project(&service).await;
        }
    };
}

mod in_memory {
    memory_service_conformance_tests!(service = adk_memory::InMemoryMemoryService::new());
}

#[cfg(feature = "sqlite-memory")]
mod sqlite {
    memory_service_conformance_tests!(
        service = {
            let service =
                adk_memory::sqlite::SqliteMemoryService::new("sqlite::memory:").await.unwrap();
            service.migrate().await.unwrap();
            service
        }
    );
}

#[cfg(feature = "database-memory")]
mod postgres {
    use std::str::FromStr;

    fn database_url() -> String {
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
    }

    /// A service in a schema of its own, keyword search only.
    async fn service() -> adk_memory::PostgresMemoryService {
        use sqlx::Connection;
        let schema = format!("adk_memory_m2_{}", uuid::Uuid::new_v4().simple());
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
        service
    }

    memory_service_conformance_tests!(
        #[ignore = "needs DATABASE_URL with pgvector; CI's postgres job runs it"]
        service = service().await
    );
}
