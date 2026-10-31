//! The PostgreSQL agent store (spec 3.3 §5.7, §10 K7): the conformance suite, and replicas
//! migrating one database together.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them
//! (`scripts/test-postgres.sh`). Each harness works in a scratch schema of its own, and every
//! `open` is a new pool over it, as a restarted or second replica would create:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-kernel --features postgres,redis,channels --run-ignored only \
//!   --test integration -E 'test(/^agents_store_postgres::/)'
//! ```

#![cfg(feature = "postgres")]

use crate::agents_support;

use std::str::FromStr;
use std::sync::Arc;

use adk_kernel::KernelError;
use adk_kernel::agents::AgentStore;
use adk_kernel::conformance::AgentStoreHarness;
use adk_kernel::stores::PostgresAgentStore;
use sqlx::Connection as _;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

/// A scratch schema, created on first use.
struct Scratch {
    schema: String,
}

impl Scratch {
    fn new() -> Self {
        Self { schema: format!("adk_kernel_agents_{}", uuid::Uuid::new_v4().simple()) }
    }

    /// Connection options whose search path is the scratch schema.
    async fn options(&self) -> PgConnectOptions {
        let mut admin = sqlx::PgConnection::connect(&database_url()).await.expect("connect");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA IF NOT EXISTS {}", self.schema)))
            .execute(&mut admin)
            .await
            .expect("create schema");
        PgConnectOptions::from_str(&database_url())
            .expect("parse DATABASE_URL")
            .options([("search_path", self.schema.as_str())])
    }

    /// A pool of its own, as a replica holds.
    async fn pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(4)
            .connect_with(self.options().await)
            .await
            .expect("connect")
    }
}

#[async_trait::async_trait]
impl AgentStoreHarness for Scratch {
    async fn open(&self) -> Arc<dyn AgentStore> {
        let store = PostgresAgentStore::new(self.pool().await);
        store.migrate().await.expect("agent schema migrates");
        Arc::new(store)
    }
}

mod conformance {
    use super::*;

    adk_kernel::agent_store_conformance_tests! {
        #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
        harness = Scratch::new()
    }
}

/// Replicas starting together migrate one at a time: every migration succeeds and the
/// registry records the step once.
#[tokio::test]
#[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
async fn replicas_starting_together_migrate_once() {
    let scratch = Arc::new(Scratch::new());
    scratch.options().await;
    let mut starts = Vec::new();
    for _ in 0..4 {
        let scratch = scratch.clone();
        starts.push(tokio::spawn(async move {
            PostgresAgentStore::new(scratch.pool().await).migrate().await
        }));
    }
    for start in starts {
        start.await.expect("start completes").expect("migrate");
    }
    let pool = scratch.pool().await;
    let (applied,): (i64,) = sqlx::query_as("SELECT count(*) FROM _adk_kernel_agent_migrations")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(applied, 1);
}

/// An unmigrated schema answers with a store error rather than a panic.
#[tokio::test]
#[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
async fn an_unmigrated_store_fails_with_a_store_error() {
    let unmigrated = PostgresAgentStore::new(Scratch::new().pool().await);
    let error = unmigrated.get("finance").await.expect_err("no table");
    assert!(matches!(error, KernelError::Store { backend: "postgres", .. }), "{error}");
}

/// Two kernels on one database converge within a tick: an agent one adds is hosted by the
/// other, which never saw the change, and its run completes there (spec 3.3 §5.7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
async fn two_kernels_on_one_database_converge_within_a_tick() {
    use adk_core::{NewRun, RunState};
    use adk_kernel::agents::{AgentChange, AgentDefinition};
    use adk_kernel::{KernelEvent, ProcessKind, ProcessSpec, QueueClass};

    let scratch = Scratch::new();
    let open = || async {
        let store = PostgresAgentStore::new(scratch.pool().await);
        store.migrate().await.expect("migrate");
        Arc::new(store) as Arc<dyn AgentStore>
    };
    let world = agents_support::World::new();
    let (first, second) = (open().await, open().await);
    let adding = world.kernel_with("k1", |builder| builder.agent_store(first));
    let serving = world.kernel_with("k2", |builder| builder.agent_store(second));
    serving.start().await.unwrap();
    let mut events = serving.events();
    agents_support::add(&adding, AgentDefinition::new("Finance", "default")).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let record = events.recv().await.expect("the bus stays open");
            if let KernelEvent::Agent { id, change: AgentChange::Hosted, .. } = record.event
                && id == "finance"
            {
                return;
            }
        }
    })
    .await
    .expect("hosted within a tick");
    let run = NewRun::new(
        QueueClass::INTERACTIVE,
        agents_support::APP,
        agents_support::PERSON,
        "finance",
        serde_json::json!("hi"),
    )
    .with_session_id("finance-1");
    let pid = serving
        .processes()
        .spawn(run, ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    agents_support::in_state(&world.runs, &pid, RunState::Completed).await;
    serving.stop().await.unwrap();
}
