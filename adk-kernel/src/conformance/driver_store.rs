//! Conformance cases for [`DriverStore`] backends (spec 3.2 §7.7, §11, §13 Q8).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`put_get_round_trip`] | A record reads back exactly as put: every field, every [`EffectClass`], [`DriverSandbox`] and [`DriverIsolation`] variant |
//! | [`put_replaces`] | A second put of the same driver replaces its record |
//! | [`list_orders_by_id`] | `list` returns every record in driver-id byte order |
//! | [`unknown_id`] | `get` of an unknown driver is `None`; `delete` answers `false` |
//! | [`delete`] | `delete` removes the record and answers `true`, then `false`, and leaves its parks |
//! | [`records_survive_a_restart`] | Records and parks read back through a new handle (durable backends only) |
//! | [`concurrent_writers`] | Concurrent puts from two handles over distinct drivers and calls lose none |
//! | [`park_round_trip_per_kind`] | A park reads back exactly as put for every kind: `mrtr`, `task { taskId }`, `legacy` |
//! | [`parks_are_keyed_by_scope_and_call`] | One call id in two sessions is two parks; a second put of one key replaces it |
//! | [`list_parks_filters_by_driver`] | `list_parks` filters by driver and orders by scope and call id |
//! | [`delete_park`] | `delete_park` removes one park and answers `true`, then `false` |
//!
//! Every case writes under fresh driver ids and scopes and deletes what it wrote, so a
//! harness may point every case at one shared database.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::{ApprovalScope, ToolConfirmationRequest};
use adk_tool::mcp::ParkedMcpCall;
use adk_tool::mcp::manager::{McpLifecycleMode, McpServerConfig, RestartPolicy};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;

use crate::drivers::{
    DriverAccess, DriverIsolation, DriverLifecycle, DriverManifest, DriverPark, DriverRecord,
    DriverSandbox, DriverStore, EffectClass,
};
use crate::partition::PartitionId;

/// Opens a [`DriverStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::conformance::DriverStoreHarness;
/// use adk_kernel::drivers::DriverStore;
/// use adk_kernel::stores::InMemoryDriverStore;
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl DriverStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn DriverStore> {
///         Arc::new(InMemoryDriverStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait DriverStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn DriverStore>;

    /// Whether data written through one handle is visible through the next.
    fn is_durable(&self) -> bool {
        true
    }
}

/// A fresh driver id: ASCII, as manifests require.
fn driver_id(prefix: &str) -> String {
    format!("conformance-{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// A fresh scope with the characters a backend must escape.
fn scope() -> ApprovalScope {
    let id = uuid::Uuid::new_v4();
    ApprovalScope::new(format!("app:ü/{id}"), format!("user/日本:{id}"), format!("session %2F{id}"))
}

/// An instant with nanoseconds, so a backend that rounds timestamps is caught.
fn instant(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + seconds, 123_456_789).single().expect("valid instant")
}

/// A record with every field set, sandboxed as `sandbox`.
fn full_record(id: &str, sandbox: DriverSandbox, isolation: DriverIsolation) -> DriverRecord {
    let server = McpServerConfig {
        command: "/opt/mcp/ü bin/mail-server".into(),
        args: vec!["--imap".into(), "it's \"quoted\"".into()],
        env: [
            ("EMAIL_TOKEN".to_string(), "secret://email/token".to_string()),
            ("LOG_LEVEL".to_string(), "debug".to_string()),
        ]
        .into(),
        restart_policy: Some(RestartPolicy {
            initial_delay_ms: 250,
            max_delay_ms: 8_000,
            backoff_multiplier: 1.5,
            max_restart_attempts: 4,
        }),
        lifecycle: McpLifecycleMode::Discover,
        ..McpServerConfig::default()
    };
    let mut manifest = DriverManifest::new(id, server)
        .expect("a valid manifest")
        .with_partitions([PartitionId::new("work"), PartitionId::new("日本")])
        .with_sandbox(sandbox)
        .with_lifecycle(DriverLifecycle::Eager)
        .with_sampling(true)
        .with_access({
            let mut access = DriverAccess::default();
            access.allow_read.push("/srv/mail".into());
            access.allow_write.push("/srv/mail/drafts".into());
            access.allow_network = false;
            access
        });
    for (index, class) in EffectClass::ALL.into_iter().enumerate() {
        manifest = manifest.with_tool(format!("tool_{index}_{class}"), class);
    }
    manifest.data_classes = vec!["email".into(), "contacts".into()];
    manifest.declaration_digests = [
        ("tool_0_read".to_string(), "sha256:aa".to_string()),
        ("ü".to_string(), "sha256:bb".to_string()),
    ]
    .into();
    let mut record = DriverRecord::new(manifest, "alice ü", instant(0));
    record.isolation = Some(isolation);
    record.stale = [("tool_3_send_external".to_string(), "sha256:cc".to_string())].into();
    record.updated_at = instant(60);
    record
}

fn parked(driver: &str, call_id: &str, kind: serde_json::Value) -> ParkedMcpCall {
    use adk_tool::mcp::rmcp::model::{
        ElicitRequest, ElicitRequestParams, InputRequest, InputRequests,
    };
    let schema = serde_json::from_value(
        json!({ "type": "object", "properties": { "ok": { "type": "boolean" } } }),
    )
    .expect("a schema");
    let question = ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: "Send to ü?".into(),
        requested_schema: schema,
    };
    let requests: InputRequests =
        [("confirm".to_string(), InputRequest::Elicitation(ElicitRequest::new(question)))].into();
    serde_json::from_value(json!({
        "serverId": driver,
        "toolName": "send_draft",
        "arguments": { "draftId": "d-7", "to": ["ops@example.com"] },
        "callId": call_id,
        "requestState": "opaque/state:ü",
        "inputRequests": requests,
        "inputRequestsDigest": ParkedMcpCall::digest_input_requests(&requests),
        "inputRounds": 2,
        "kind": kind,
    }))
    .expect("a parked call")
}

fn park(driver: &str, scope: &ApprovalScope, call_id: &str, kind: serde_json::Value) -> DriverPark {
    let request = ToolConfirmationRequest::new(
        format!("driver:{driver}/elicitation"),
        json!({ "driver": driver, "questions": { "confirm": { "message": "Send to ü?" } } }),
    )
    .with_function_call_id(call_id);
    let mut park = DriverPark::new(scope.clone(), call_id, parked(driver, call_id, kind), request)
        .with_run_id(format!("run/{call_id}"))
        .with_expires_at(instant(3_600));
    park.created_at = instant(1);
    park.carries_approval = true;
    park
}

async fn own_ids(store: &dyn DriverStore, ids: &[String]) -> Vec<String> {
    store
        .list()
        .await
        .expect("list")
        .into_iter()
        .map(|record| record.id().to_string())
        .filter(|id| ids.contains(id))
        .collect()
}

/// A record reads back exactly as put, for every sandbox and isolation.
pub async fn put_get_round_trip(harness: &dyn DriverStoreHarness) {
    let store = harness.open().await;
    let cases = [
        (
            DriverSandbox::Os,
            DriverIsolation::OsSandbox { enforcer: "seatbelt".into(), program_paths: Vec::new() },
        ),
        (
            DriverSandbox::Container,
            DriverIsolation::Unsandboxed { reason: "no container ü".into() },
        ),
        (DriverSandbox::Unsandboxed, DriverIsolation::Unsandboxed { reason: "asked".into() }),
    ];
    for (sandbox, isolation) in cases {
        let id = driver_id("round-trip");
        let record = full_record(&id, sandbox, isolation);
        store.put(record.clone()).await.expect("put");
        assert_eq!(store.get(&id).await.expect("get"), Some(record));
        assert!(store.delete(&id).await.expect("delete"));
    }
}

/// A second put of the same driver replaces its record.
pub async fn put_replaces(harness: &dyn DriverStoreHarness) {
    let store = harness.open().await;
    let id = driver_id("replace");
    let first = full_record(
        &id,
        DriverSandbox::Os,
        DriverIsolation::OsSandbox { enforcer: "seatbelt".into(), program_paths: Vec::new() },
    );
    store.put(first.clone()).await.expect("put");
    let mut second = first.clone();
    second.stale.clear();
    second.approved_by = "bob".into();
    second.manifest.declaration_digests.insert("tool_3_send_external".into(), "sha256:cc".into());
    store.put(second.clone()).await.expect("put again");
    assert_eq!(store.get(&id).await.expect("get"), Some(second));
    let only = std::slice::from_ref(&id);
    assert_eq!(own_ids(store.as_ref(), only).await, only);
    store.delete(&id).await.expect("delete");
}

/// `list` returns every record in driver-id byte order.
pub async fn list_orders_by_id(harness: &dyn DriverStoreHarness) {
    let store = harness.open().await;
    let stem = driver_id("order");
    let ids: Vec<String> =
        ["b", "A", "a-2", "a_1", "0"].iter().map(|suffix| format!("{stem}-{suffix}")).collect();
    for id in &ids {
        let record = full_record(
            id,
            DriverSandbox::Os,
            DriverIsolation::OsSandbox {
                enforcer: "bubblewrap".into(),
                program_paths: vec!["/opt/mcp/bin".into()],
            },
        );
        store.put(record).await.expect("put");
    }
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(own_ids(store.as_ref(), &ids).await, expected);
    for id in &ids {
        store.delete(id).await.expect("delete");
    }
}

/// An unknown driver reads as `None` and deletes as `false`.
pub async fn unknown_id(harness: &dyn DriverStoreHarness) {
    let store = harness.open().await;
    let id = driver_id("unknown");
    assert_eq!(store.get(&id).await.expect("get"), None);
    assert!(!store.delete(&id).await.expect("delete"));
    assert_eq!(store.get_park(&scope(), "call-1").await.expect("get park"), None);
    assert!(!store.delete_park(&scope(), "call-1").await.expect("delete park"));
}

/// `delete` removes the record, answers `true` then `false`, and leaves its parks.
pub async fn delete(harness: &dyn DriverStoreHarness) {
    let store = harness.open().await;
    let id = driver_id("delete");
    let scope = scope();
    store
        .put(full_record(
            &id,
            DriverSandbox::Os,
            DriverIsolation::OsSandbox { enforcer: "seatbelt".into(), program_paths: Vec::new() },
        ))
        .await
        .expect("put");
    store.put_park(park(&id, &scope, "call-1", json!("mrtr"))).await.expect("put park");
    assert!(store.delete(&id).await.expect("delete"));
    assert!(!store.delete(&id).await.expect("delete again"));
    assert_eq!(store.get(&id).await.expect("get"), None);
    assert!(own_ids(store.as_ref(), std::slice::from_ref(&id)).await.is_empty());
    assert!(store.get_park(&scope, "call-1").await.expect("get park").is_some());
    store.delete_park(&scope, "call-1").await.expect("delete park");
}

/// Records and parks read back through a new handle (durable backends only).
pub async fn records_survive_a_restart(harness: &dyn DriverStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let id = driver_id("restart");
    let scope = scope();
    let record = full_record(
        &id,
        DriverSandbox::Os,
        DriverIsolation::OsSandbox { enforcer: "seatbelt".into(), program_paths: Vec::new() },
    );
    let held = park(&id, &scope, "call-1", json!({ "task": { "taskId": "t-1" } }));
    {
        let store = harness.open().await;
        store.put(record.clone()).await.expect("put");
        store.put_park(held.clone()).await.expect("put park");
    }
    let reopened = harness.open().await;
    assert_eq!(reopened.get(&id).await.expect("get"), Some(record));
    assert_eq!(reopened.get_park(&scope, "call-1").await.expect("get park"), Some(held));
    reopened.delete(&id).await.expect("delete");
    reopened.delete_park(&scope, "call-1").await.expect("delete park");
}

/// Concurrent puts from two handles over distinct drivers and calls lose none.
pub async fn concurrent_writers(harness: &dyn DriverStoreHarness) {
    let (first, second) = if harness.is_durable() {
        (harness.open().await, harness.open().await)
    } else {
        let store = harness.open().await;
        (store.clone(), store)
    };
    let scope = scope();
    let stem = driver_id("concurrent");
    let mut tasks = Vec::new();
    for index in 0..24 {
        let store = if index % 2 == 0 { first.clone() } else { second.clone() };
        let id = format!("{stem}-{index}");
        let scope = scope.clone();
        tasks.push(tokio::spawn(async move {
            let record = full_record(
                &id,
                DriverSandbox::Os,
                DriverIsolation::OsSandbox {
                    enforcer: "seatbelt".into(),
                    program_paths: Vec::new(),
                },
            );
            store.put(record).await.expect("put");
            store
                .put_park(park(&id, &scope, &format!("call-{index}"), json!("legacy")))
                .await
                .expect("put park");
            id
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.expect("writer"));
    }
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(own_ids(first.as_ref(), &ids).await, expected);
    for (index, id) in ids.iter().enumerate() {
        assert!(
            second.get_park(&scope, &format!("call-{index}")).await.expect("get park").is_some()
        );
        second.delete_park(&scope, &format!("call-{index}")).await.expect("delete park");
        second.delete(id).await.expect("delete");
    }
}

/// A park reads back exactly as put for every kind.
pub async fn park_round_trip_per_kind(harness: &dyn DriverStoreHarness) {
    let store = harness.open().await;
    let id = driver_id("kinds");
    let scope = scope();
    for (index, kind) in
        [json!("mrtr"), json!({ "task": { "taskId": "task/ü:1" } }), json!("legacy")]
            .into_iter()
            .enumerate()
    {
        let call_id = format!("call/{index}:ü");
        let held = park(&id, &scope, &call_id, kind);
        store.put_park(held.clone()).await.expect("put park");
        let read = store.get_park(&scope, &call_id).await.expect("get park");
        assert_eq!(read, Some(held));
        store.delete_park(&scope, &call_id).await.expect("delete park");
    }
}

/// One call id in two sessions is two parks; a second put of one key replaces it.
pub async fn parks_are_keyed_by_scope_and_call(harness: &dyn DriverStoreHarness) {
    let store = harness.open().await;
    let id = driver_id("keys");
    let (one, two) = (scope(), scope());
    let first = park(&id, &one, "call-1", json!("mrtr"));
    let other = park(&id, &two, "call-1", json!("legacy"));
    store.put_park(first.clone()).await.expect("put park");
    store.put_park(other.clone()).await.expect("put park");
    assert_eq!(store.get_park(&one, "call-1").await.expect("get"), Some(first.clone()));
    assert_eq!(store.get_park(&two, "call-1").await.expect("get"), Some(other));
    let mut replaced = first.clone();
    replaced.call.input_rounds = 3;
    replaced.run_id = None;
    store.put_park(replaced.clone()).await.expect("replace");
    assert_eq!(store.get_park(&one, "call-1").await.expect("get"), Some(replaced));
    store.delete_park(&one, "call-1").await.expect("delete");
    store.delete_park(&two, "call-1").await.expect("delete");
}

/// `list_parks` filters by driver and orders by scope and call id.
pub async fn list_parks_filters_by_driver(harness: &dyn DriverStoreHarness) {
    let store = harness.open().await;
    let (mail, calendar) = (driver_id("mail"), driver_id("calendar"));
    let scope = scope();
    for call in ["call-b", "call-a"] {
        store.put_park(park(&mail, &scope, call, json!("mrtr"))).await.expect("put park");
    }
    store.put_park(park(&calendar, &scope, "call-c", json!("mrtr"))).await.expect("put park");
    let mail_calls: Vec<String> = store
        .list_parks(Some(&mail))
        .await
        .expect("list")
        .into_iter()
        .map(|park| park.call_id)
        .collect();
    assert_eq!(mail_calls, ["call-a", "call-b"]);
    let all: Vec<(String, String)> = store
        .list_parks(None)
        .await
        .expect("list")
        .into_iter()
        .filter(|park| park.driver == mail || park.driver == calendar)
        .map(|park| (park.driver, park.call_id))
        .collect();
    assert_eq!(all.len(), 3);
    let calls: Vec<&str> = all.iter().map(|(_, call)| call.as_str()).collect();
    assert_eq!(calls, ["call-a", "call-b", "call-c"]);
    for call in ["call-a", "call-b", "call-c"] {
        store.delete_park(&scope, call).await.expect("delete");
    }
    assert!(store.list_parks(Some(&mail)).await.expect("list").is_empty());
}

/// `delete_park` removes one park and answers `true`, then `false`.
pub async fn delete_park(harness: &dyn DriverStoreHarness) {
    let store = harness.open().await;
    let id = driver_id("delete-park");
    let scope = scope();
    store.put_park(park(&id, &scope, "call-1", json!("mrtr"))).await.expect("put");
    store.put_park(park(&id, &scope, "call-2", json!("mrtr"))).await.expect("put");
    assert!(store.delete_park(&scope, "call-1").await.expect("delete"));
    assert!(!store.delete_park(&scope, "call-1").await.expect("delete again"));
    let left: BTreeMap<String, DriverPark> = store
        .list_parks(Some(&id))
        .await
        .expect("list")
        .into_iter()
        .map(|park| (park.call_id.clone(), park))
        .collect();
    assert_eq!(left.keys().collect::<Vec<_>>(), ["call-2"]);
    store.delete_park(&scope, "call-2").await.expect("delete");
}
