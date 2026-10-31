//! The mode and standing-rule stores on Redis (spec 3.1 §7.4, §7.5, §13 Q2): the
//! conformance suites, two store instances over one server, and two harness replicas over
//! one server.
//!
//! The tests are `#[ignore]`d because they need a server; CI's postgres job runs them with
//! its Redis service. Each test keeps its keys under a namespace of its own:
//!
//! ```bash
//! REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-harness --features redis --run-ignored only \
//!   --test integration -E 'test(/^shared_store_redis_tests::/)'
//! ```

#![cfg(all(feature = "redis", feature = "runner"))]

use crate::replicas;

use std::sync::Arc;
use std::time::Duration;

use adk_harness::{RedisModeStore, RedisStandingRuleStore};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn namespace() -> String {
    format!("adk-harness-test:{}", uuid::Uuid::new_v4().simple())
}

/// A store with a connection of its own, as a replica holds.
async fn mode_store(namespace: &str) -> RedisModeStore {
    RedisModeStore::connect(&redis_url()).await.expect("connect").with_namespace(namespace)
}

async fn rule_store(namespace: &str) -> RedisStandingRuleStore {
    let store = RedisStandingRuleStore::connect(&redis_url())
        .await
        .expect("connect")
        .with_namespace(namespace);
    store.migrate().await.unwrap();
    store
}

mod conformance {
    use super::*;

    adk_harness::mode_store_conformance_tests!(
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        store = mode_store(&namespace()).await
    );
    adk_harness::standing_rule_store_conformance_tests!(
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        store = rule_store(&namespace()).await
    );
    adk_harness::shared_store_conformance_tests!(
        #[ignore = "needs REDIS_URL; CI's postgres job runs it"]
        modes = {
            let namespace = namespace();
            (mode_store(&namespace).await, mode_store(&namespace).await)
        },
        rules = {
            let namespace = namespace();
            (rule_store(&namespace).await, rule_store(&namespace).await)
        },
    );
}

#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn namespaces_keep_deployments_apart() {
    use adk_harness::{HarnessMode, ModeRecord, ModeSource, ModeStore, StandingRuleStore};
    let (first, second) = (namespace(), namespace());
    mode_store(&first)
        .await
        .set("app", "alice", "s1", ModeRecord::new(HarnessMode::Auto, ModeSource::User))
        .await
        .unwrap();
    assert!(mode_store(&second).await.get("app", "alice", "s1").await.unwrap().is_none());
    let rule =
        adk_harness::conformance::standing_rule_store::rule_for_tests("r1", "app", "alice", None);
    rule_store(&first).await.grant(rule).await.unwrap();
    assert!(rule_store(&second).await.list("app", "alice").await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn two_harness_replicas_share_rules_and_modes() {
    let namespace = namespace();
    // The session keys are namespaced by app, so each run uses an app of its own.
    let app = format!("harness-replicas-{}", uuid::Uuid::new_v4().simple());
    let replica = || async {
        let sessions =
            adk_session::redis::RedisSessionService::new(adk_session::redis::RedisSessionConfig {
                url: redis_url(),
                ttl: Some(Duration::from_secs(600)),
                cluster_nodes: None,
            })
            .await
            .expect("connect");
        replicas::ReplicaStores {
            modes: Arc::new(mode_store(&namespace).await),
            rules: Arc::new(rule_store(&namespace).await),
            sessions: Arc::new(sessions),
        }
    };
    let (first, second) = (replica().await, replica().await);
    replicas::rules_and_modes_reach_every_replica(&app, first, second).await;
}

/// A `redis://` store URL opens the plan checkpointer, and a second process reads what the
/// first wrote (spec 3.1 §7.11, §13 Q5).
#[cfg(feature = "graph")]
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn a_redis_url_checkpoints_plans_across_processes() {
    use adk_graph::{Checkpoint, State};
    let thread = format!("plan-{}", uuid::Uuid::new_v4().simple());
    let mut checkpoint = Checkpoint::new(
        &thread,
        State::from([("plan".to_string(), serde_json::json!("two files"))]),
        1,
        vec!["step:1".into()],
    );
    checkpoint.version = 1;
    let first = adk_harness::checkpointer_from_url(&redis_url()).await.unwrap();
    first.save(&checkpoint).await.unwrap();
    drop(first);
    let second = adk_harness::checkpointer_from_url(&redis_url()).await.unwrap();
    assert_eq!(second.load(&thread).await.unwrap(), Some(checkpoint));
}

/// The file tools' read records on Redis (spec 3.1 §7.26, §10 D7): the conformance cases,
/// and a record written through one replica's store read through another's.
#[tokio::test]
#[ignore = "needs REDIS_URL; CI's postgres job runs it"]
async fn read_records_conform_and_reach_every_replica() {
    use adk_harness::conformance::read_records;
    async fn store(namespace: &str) -> adk_harness::RedisReadRecords {
        adk_harness::RedisReadRecords::connect(&redis_url())
            .await
            .expect("connect")
            .with_namespace(namespace)
    }
    read_records::round_trip(&store(&namespace()).await).await;
    read_records::scopes_are_isolated(&store(&namespace()).await).await;
    read_records::forget_removes_one_path(&store(&namespace()).await).await;
    let namespace = namespace();
    read_records::records_reach_every_instance(&store(&namespace).await, &store(&namespace).await)
        .await;
}
