//! The mode and standing-rule stores on PostgreSQL (spec 3.1 §7.4, §7.5, §13 Q2): the
//! conformance suites, two store instances over one database, concurrent migration, and
//! two harness replicas over one database.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them
//! (`scripts/test-postgres.sh`). Each test works in a scratch schema of its own:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-harness --features postgres --run-ignored only \
//!   --test integration -E 'test(/^shared_store_postgres_tests::/)'
//! ```

#![cfg(all(feature = "postgres", feature = "runner"))]

use crate::replicas;

use std::str::FromStr;
use std::sync::Arc;

use adk_harness::{PostgresModeStore, PostgresStandingRuleStore};
use sqlx::Connection as _;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

/// Connection options whose search path is a fresh scratch schema.
async fn scratch() -> PgConnectOptions {
    let schema = format!("adk_harness_shared_{}", uuid::Uuid::new_v4().simple());
    let mut admin = sqlx::PgConnection::connect(&database_url()).await.expect("connect");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut admin)
        .await
        .expect("create schema");
    PgConnectOptions::from_str(&database_url())
        .expect("parse DATABASE_URL")
        .options([("search_path", schema.as_str())])
}

/// A pool of its own, as a replica holds.
async fn pool(options: &PgConnectOptions) -> PgPool {
    PgPoolOptions::new().max_connections(4).connect_with(options.clone()).await.expect("connect")
}

async fn mode_store(options: &PgConnectOptions) -> PostgresModeStore {
    let store = PostgresModeStore::new(pool(options).await);
    store.migrate().await.unwrap();
    store
}

async fn rule_store(options: &PgConnectOptions) -> PostgresStandingRuleStore {
    let store = PostgresStandingRuleStore::new(pool(options).await);
    store.migrate().await.unwrap();
    store
}

mod conformance {
    use super::*;

    adk_harness::mode_store_conformance_tests!(
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        store = mode_store(&scratch().await).await
    );
    adk_harness::standing_rule_store_conformance_tests!(
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        store = rule_store(&scratch().await).await
    );
    adk_harness::shared_store_conformance_tests!(
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        modes = {
            let options = scratch().await;
            (mode_store(&options).await, mode_store(&options).await)
        },
        rules = {
            let options = scratch().await;
            (rule_store(&options).await, rule_store(&options).await)
        },
    );
}

#[tokio::test]
#[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
async fn replicas_starting_together_migrate_once() {
    let options = scratch().await;
    let mut starts = Vec::new();
    for _ in 0..4 {
        let options = options.clone();
        starts.push(tokio::spawn(async move {
            let pool = pool(&options).await;
            PostgresModeStore::new(pool.clone()).migrate().await?;
            PostgresStandingRuleStore::new(pool).migrate().await
        }));
    }
    for start in starts {
        start.await.unwrap().unwrap();
    }
    let mut connection = sqlx::PgConnection::connect_with(&options).await.unwrap();
    for registry in ["_adk_harness_mode_migrations", "_adk_harness_rule_migrations"] {
        let (applied,): (i64,) =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {registry}")))
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(applied, 1, "{registry}");
    }
}

#[tokio::test]
#[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
async fn two_harness_replicas_share_rules_and_modes() {
    let options = scratch().await;
    let replica = || async {
        let pool = pool(&options).await;
        let sessions = adk_session::PostgresSessionService::from_pool(pool.clone());
        sessions.migrate().await.unwrap();
        replicas::ReplicaStores {
            modes: Arc::new(mode_store(&options).await),
            rules: Arc::new(rule_store(&options).await),
            sessions: Arc::new(sessions),
        }
    };
    let (first, second) = (replica().await, replica().await);
    replicas::rules_and_modes_reach_every_replica("harness-replicas", first, second).await;
}

/// A `postgres://` store URL opens the plan checkpointer, and a second process reads what
/// the first wrote (spec 3.1 §7.11, §13 Q5).
#[cfg(feature = "graph")]
#[tokio::test]
#[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
async fn a_postgres_url_checkpoints_plans_across_processes() {
    use adk_graph::{Checkpoint, State};
    let thread = format!("plan-{}", uuid::Uuid::new_v4().simple());
    let mut checkpoint = Checkpoint::new(
        &thread,
        State::from([("plan".to_string(), serde_json::json!("two files"))]),
        1,
        vec!["step:1".into()],
    );
    checkpoint.version = 1;
    let first = adk_harness::checkpointer_from_url(&database_url()).await.unwrap();
    first.save(&checkpoint).await.unwrap();
    drop(first);
    let second = adk_harness::checkpointer_from_url(&database_url()).await.unwrap();
    assert_eq!(second.load(&thread).await.unwrap(), Some(checkpoint));
}

/// The file tools' read records on PostgreSQL (spec 3.1 §7.26, §10 D7): the conformance
/// cases, and a record written through one replica's store read through another's.
#[tokio::test]
#[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
async fn read_records_conform_and_reach_every_replica() {
    use adk_harness::conformance::read_records;
    async fn store(options: &PgConnectOptions) -> adk_harness::PostgresReadRecords {
        let store = adk_harness::PostgresReadRecords::new(pool(options).await);
        store.migrate().await.unwrap();
        store
    }
    read_records::round_trip(&store(&scratch().await).await).await;
    read_records::scopes_are_isolated(&store(&scratch().await).await).await;
    read_records::forget_removes_one_path(&store(&scratch().await).await).await;
    let options = scratch().await;
    read_records::records_reach_every_instance(&store(&options).await, &store(&options).await)
        .await;
}
