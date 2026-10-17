//! The session, shared-state, and identifier contracts of the Redis backend, on a standalone
//! server and on a Redis Cluster.
//!
//! The standalone tests use the server at `REDIS_URL` (default `redis://localhost:6379`), the
//! cluster tests the comma-separated `host:port` seed nodes in `REDIS_CLUSTER_NODES`. Each run
//! uses fresh app names:
//!
//! ```bash
//! REDIS_URL=redis://127.0.0.1:6379 \
//! REDIS_CLUSTER_NODES=127.0.0.1:7001,127.0.0.1:7002,127.0.0.1:7003 \
//!     cargo nextest run -p adk-session --features redis --run-ignored only \
//!     --test session_contract_redis
//! ```

#![cfg(feature = "redis")]

mod common;

use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, GetRequest, ListRequest,
    RedisSessionConfig, RedisSessionService, SessionService,
};
use serde_json::json;
use std::collections::HashMap;

async fn standalone() -> RedisSessionService {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
    RedisSessionService::new(RedisSessionConfig { url, ttl: None, cluster_nodes: None })
        .await
        .expect("connect to redis")
}

async fn cluster() -> RedisSessionService {
    let nodes: Vec<String> = std::env::var("REDIS_CLUSTER_NODES")
        .expect("REDIS_CLUSTER_NODES names the cluster's seed nodes")
        .split(',')
        .map(|node| node.trim().to_string())
        .collect();
    let url = format!("redis-cluster://{}", nodes[0]);
    RedisSessionService::new(RedisSessionConfig { url, ttl: None, cluster_nodes: Some(nodes) })
        .await
        .expect("connect to the redis cluster")
}

fn run_app(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

/// Create, append through both paths with all three state tiers, list, delete, and
/// delete-all for identifiers containing the key delimiter and hash tag braces.
async fn assert_lifecycle_with_delimiters(service: &RedisSessionService) {
    let app = run_app("lifecycle:{app}");
    let user = "user:{1}";
    let get = |session: &str| GetRequest {
        app_name: app.clone(),
        user_id: user.to_string(),
        session_id: session.to_string(),
        num_recent_events: None,
        after: None,
    };
    let list = || ListRequest {
        app_name: app.clone(),
        user_id: user.to_string(),
        limit: None,
        offset: None,
    };

    for session in ["s{1}", "s}{2"] {
        service
            .create(CreateRequest {
                app_name: app.clone(),
                user_id: user.to_string(),
                session_id: Some(session.to_string()),
                state: HashMap::from([
                    ("app:created".to_string(), json!(session)),
                    ("user:created".to_string(), json!(session)),
                    ("topic".to_string(), json!(session)),
                ]),
            })
            .await
            .expect("create");
    }

    let mut by_id = Event::new("inv-1");
    by_id.actions.state_delta.insert("app:by_id".to_string(), json!(1));
    by_id.actions.state_delta.insert("user:by_id".to_string(), json!(2));
    by_id.actions.state_delta.insert("by_id".to_string(), json!(3));
    service.append_event("s{1}", by_id).await.expect("append by session id");

    let mut by_identity = Event::new("inv-2");
    by_identity.actions.state_delta.insert("app:by_identity".to_string(), json!(4));
    by_identity.actions.state_delta.insert("user:by_identity".to_string(), json!(5));
    by_identity.actions.state_delta.insert("by_identity".to_string(), json!(6));
    service
        .append_event_for_identity(AppendEventRequest {
            identity: adk_core::identity::AdkIdentity::new(
                app.as_str().try_into().unwrap(),
                user.try_into().unwrap(),
                "s{1}".try_into().unwrap(),
            ),
            event: by_identity,
        })
        .await
        .expect("append by identity");

    let session = service.get(get("s{1}")).await.expect("get");
    let state = session.state().all();
    assert_eq!(session.events().len(), 2);
    for (key, value) in [
        ("app:created", json!("s}{2")),
        ("user:created", json!("s}{2")),
        ("topic", json!("s{1}")),
        ("app:by_id", json!(1)),
        ("user:by_id", json!(2)),
        ("by_id", json!(3)),
        ("app:by_identity", json!(4)),
        ("user:by_identity", json!(5)),
        ("by_identity", json!(6)),
    ] {
        assert_eq!(state.get(key), Some(&value), "state key {key}");
    }

    let mut listed: Vec<String> =
        service.list(list()).await.expect("list").iter().map(|s| s.id().to_string()).collect();
    listed.sort();
    assert_eq!(listed, ["s{1}", "s}{2"]);

    service
        .delete(DeleteRequest {
            app_name: app.clone(),
            user_id: user.to_string(),
            session_id: "s{1}".into(),
        })
        .await
        .expect("delete");
    assert!(service.get(get("s{1}")).await.err().expect("deleted").is_not_found());
    assert!(service.append_event("s{1}", Event::new("inv-3")).await.is_err());

    service.delete_all_sessions(&app, user).await.expect("delete all");
    assert!(service.list(list()).await.expect("list").is_empty());
    assert!(service.get(get("s}{2")).await.err().expect("deleted").is_not_found());
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_standalone_session_contract() {
    common::session_contract::assert_session_contract(
        &standalone().await,
        &run_app("contract"),
        &run_app("contract-other"),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_standalone_lifecycle_with_delimiters() {
    assert_lifecycle_with_delimiters(&standalone().await).await;
}

#[tokio::test]
#[ignore = "requires a Redis Cluster at REDIS_CLUSTER_NODES"]
async fn redis_cluster_session_contract() {
    common::session_contract::assert_session_contract(
        &cluster().await,
        &run_app("contract"),
        &run_app("contract-other"),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a Redis Cluster at REDIS_CLUSTER_NODES"]
async fn redis_cluster_lifecycle_with_delimiters() {
    assert_lifecycle_with_delimiters(&cluster().await).await;
}

#[tokio::test]
#[ignore = "requires a Redis Cluster at REDIS_CLUSTER_NODES"]
async fn redis_cluster_shared_tiers_are_read_at_get_time() {
    common::state_contract::assert_shared_state_contract(
        &cluster().await,
        &run_app("shared"),
        "user1",
        "user2",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a Redis Cluster at REDIS_CLUSTER_NODES"]
async fn redis_cluster_concurrent_writes_keep_every_delta() {
    common::state_contract::assert_concurrent_state_writes(
        &cluster().await,
        &run_app("concurrent"),
        "user1",
        12,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a Redis Cluster at REDIS_CLUSTER_NODES"]
async fn redis_cluster_event_filters() {
    common::state_contract::assert_event_filter_contract(
        &cluster().await,
        &run_app("filters"),
        "user1",
    )
    .await;
}

async fn assert_integrity(service: &RedisSessionService) {
    let app = run_app("integrity:{app}");
    common::integrity_contract::assert_create_and_append_errors_contract(service, &app).await;
    common::integrity_contract::assert_event_order_contract(service, &app).await;
    common::integrity_contract::assert_nul_round_trip_contract(service, &app).await;
    common::integrity_contract::assert_list_order_contract(service, &app).await;
    common::integrity_contract::assert_rewind_unsupported(service, &app).await;
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_standalone_integrity_contract() {
    assert_integrity(&standalone().await).await;
}

#[tokio::test]
#[ignore = "requires a Redis Cluster at REDIS_CLUSTER_NODES"]
async fn redis_cluster_integrity_contract() {
    assert_integrity(&cluster().await).await;
}

/// Sessions a standalone server holds from earlier releases — events scored by millisecond
/// timestamp, sessions only in the set index — stay readable, keep their order when appended
/// to, and are listed newest first.
#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_standalone_reads_data_written_by_earlier_releases() {
    use adk_session::{events_key, index_key, session_key};
    use chrono::{Duration, TimeZone, Utc};
    use fred::prelude::*;

    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
    let raw = Builder::from_config(Config::from_url(&url).unwrap()).build().unwrap();
    raw.init().await.unwrap();

    let app = run_app("legacy");
    let user = "user1";
    let base = Utc.timestamp_millis_opt(1_700_000_000_000).unwrap();
    let mut legacy_ids = Vec::new();
    for (session, updated) in [("old", 0), ("older", -60)] {
        let updated_at = base + Duration::seconds(updated);
        let _: () = raw
            .hset(
                session_key(&app, user, session),
                vec![
                    ("app_name", app.clone()),
                    ("user_id", user.to_string()),
                    ("session_id", session.to_string()),
                    ("state", r#"{"seeded":true}"#.to_string()),
                    ("created_at", updated_at.to_rfc3339()),
                    ("updated_at", updated_at.to_rfc3339()),
                ],
            )
            .await
            .unwrap();
        let _: () = raw.sadd(index_key(&app, user), session).await.unwrap();
        for n in 0..2 {
            let mut event = Event::new("legacy");
            event.timestamp = updated_at - Duration::seconds(2 - n);
            if session == "old" {
                legacy_ids.push(event.id.clone());
            }
            let _: () = raw
                .zadd(
                    events_key(&app, user, session),
                    None,
                    None,
                    false,
                    false,
                    (
                        event.timestamp.timestamp_millis() as f64,
                        serde_json::to_string(&event).unwrap(),
                    ),
                )
                .await
                .unwrap();
        }
    }

    let service = standalone().await;
    let new_event = Event::new("current");
    let mut expected = legacy_ids.clone();
    expected.push(new_event.id.clone());
    service
        .append_event_for_identity(AppendEventRequest {
            identity: adk_core::identity::AdkIdentity::new(
                app.as_str().try_into().unwrap(),
                user.try_into().unwrap(),
                "old".try_into().unwrap(),
            ),
            event: new_event,
        })
        .await
        .unwrap();
    let session = service
        .get(GetRequest {
            app_name: app.clone(),
            user_id: user.to_string(),
            session_id: "old".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    let read: Vec<String> = session.events().all().into_iter().map(|event| event.id).collect();
    assert_eq!(read, expected, "legacy events first, the new one after them");
    assert_eq!(session.state().get("seeded"), Some(json!(true)));

    service
        .create(CreateRequest {
            app_name: app.clone(),
            user_id: user.to_string(),
            session_id: Some("new".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let listed: Vec<String> = service
        .list(ListRequest {
            app_name: app.clone(),
            user_id: user.to_string(),
            limit: None,
            offset: None,
        })
        .await
        .unwrap()
        .iter()
        .map(|session| session.id().to_string())
        .collect();
    assert_eq!(listed, ["new", "old", "older"]);

    service.delete_all_sessions(&app, user).await.unwrap();
    let _: () = raw.quit().await.unwrap();
}
