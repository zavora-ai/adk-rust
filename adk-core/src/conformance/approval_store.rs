//! Conformance cases for [`ApprovalStore`] backends.
//!
//! | Case | Contract |
//! |------|----------|
//! | [`pending_requests_are_kept_once_oldest_first`] | A fingerprint is pending once, keeps its first request, and pending calls list oldest first within their scope |
//! | [`decision_is_consumed_once`] | `decide` clears the pending entry; `has_decision` does not consume; `take_decision` returns the decision once |
//! | [`later_decision_replaces_earlier`] | Deciding the same fingerprint again replaces the earlier decision |
//! | [`expired_decisions_are_discarded`] | A decision past its expiry is taken as `None` |
//! | [`decisions_stay_in_their_scope`] | A decision in one session never authorizes a call in another |
//! | [`concurrent_take_has_one_winner`] | Concurrent takes of one decision, across two handles, return it once |
//! | [`records_survive_a_restart`] | Pending requests and decisions read back unchanged after a restart |
//!
//! `has_decision` may report `true` when no live decision exists (the trait's default
//! does), so the suite checks only that it never misses a live decision and never
//! consumes one.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;

use super::unique;
use crate::{
    ApprovalScope, ApprovalStore, PendingApproval, ToolApproval, ToolConfirmationDecision,
    ToolConfirmationRequest,
};

/// Opens an [`ApprovalStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::conformance::ApprovalStoreHarness;
/// use adk_core::{ApprovalStore, InMemoryApprovalStore, async_trait};
///
/// struct InMemory(Arc<InMemoryApprovalStore>);
///
/// #[async_trait]
/// impl ApprovalStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn ApprovalStore> {
///         self.0.clone()
///     }
/// }
/// ```
#[async_trait]
pub trait ApprovalStoreHarness: Send + Sync {
    /// Opens the store.
    ///
    /// For a durable backend every call returns a new handle over the same stored data,
    /// as a restarted process or a second process would see it. A process-local backend
    /// returns the same store every time. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn ApprovalStore>;
}

fn scope() -> ApprovalScope {
    ApprovalScope::new(unique("app"), unique("user"), unique("session"))
}

fn request(amount: u64, call_id: &str) -> ToolConfirmationRequest {
    ToolConfirmationRequest {
        tool_name: "conformance_transfer".into(),
        function_call_id: Some(call_id.into()),
        args: json!({ "amount": amount, "to": "acct:1/main" }),
    }
}

async fn pending(store: &dyn ApprovalStore, scope: &ApprovalScope) -> Vec<PendingApproval> {
    store.pending(scope).await.expect("pending reads the store")
}

async fn take(
    store: &dyn ApprovalStore,
    scope: &ApprovalScope,
    fingerprint: &str,
) -> Option<ToolConfirmationDecision> {
    store.take_decision(scope, fingerprint).await.expect("take_decision reads the store")
}

async fn has(store: &dyn ApprovalStore, scope: &ApprovalScope, fingerprint: &str) -> bool {
    store.has_decision(scope, fingerprint).await.expect("has_decision reads the store")
}

/// Recording a fingerprint twice keeps one entry with the first request and time, and
/// pending calls list oldest first, only within their own scope.
pub async fn pending_requests_are_kept_once_oldest_first<H: ApprovalStoreHarness + ?Sized>(
    harness: &H,
) {
    let store = harness.open().await;
    let scope = scope();
    let older = request(10, "call-1");
    let newer = request(20, "call-2");

    store.record_pending(&scope, &older).await.expect("record");
    tokio::time::sleep(Duration::from_millis(20)).await;
    store.record_pending(&scope, &newer).await.expect("record");
    tokio::time::sleep(Duration::from_millis(20)).await;
    store.record_pending(&scope, &request(10, "call-3")).await.expect("re-record");

    let listed = pending(&*store, &scope).await;
    let summary: Vec<_> = listed.iter().map(|entry| (&entry.fingerprint, &entry.request)).collect();
    assert_eq!(
        summary,
        [(&older.fingerprint(), &older), (&newer.fingerprint(), &newer)],
        "one entry per fingerprint, keeping its first request, oldest first"
    );
    assert!(listed[0].requested_at < listed[1].requested_at, "{listed:?}");

    for other in [
        ApprovalScope::new(&scope.app_name, &scope.user_id, unique("session")),
        ApprovalScope::new(&scope.app_name, unique("user"), &scope.session_id),
        ApprovalScope::new(unique("app"), &scope.user_id, &scope.session_id),
    ] {
        assert_eq!(pending(&*store, &other).await, [], "nothing leaks into {other:?}");
    }
}

/// `decide` clears the pending entry, `has_decision` reports the decision without
/// consuming it, and `take_decision` returns it exactly once.
pub async fn decision_is_consumed_once<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let scope = scope();
    let decided = request(10, "call-1");
    let waiting = request(20, "call-2");
    store.record_pending(&scope, &decided).await.expect("record");
    store.record_pending(&scope, &waiting).await.expect("record");

    store.decide(&scope, &decided.fingerprint(), ToolApproval::approve()).await.expect("decide");
    let listed = pending(&*store, &scope).await;
    assert_eq!(
        listed.iter().map(|entry| &entry.fingerprint).collect::<Vec<_>>(),
        [&waiting.fingerprint()],
        "deciding clears the pending entry"
    );

    assert!(has(&*store, &scope, &decided.fingerprint()).await, "a live decision is reported");
    assert!(has(&*store, &scope, &decided.fingerprint()).await, "has_decision does not consume");
    assert!(!has(&*store, &scope, &waiting.fingerprint()).await, "a pending call has no decision");

    assert_eq!(
        take(&*store, &scope, &decided.fingerprint()).await,
        Some(ToolConfirmationDecision::Approve)
    );
    assert_eq!(take(&*store, &scope, &decided.fingerprint()).await, None, "taken once");
    assert_eq!(take(&*store, &scope, &waiting.fingerprint()).await, None, "never decided");
}

/// Deciding a fingerprint again replaces the earlier decision.
pub async fn later_decision_replaces_earlier<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let scope = scope();
    let fingerprint = request(10, "call-1").fingerprint();

    store.decide(&scope, &fingerprint, ToolApproval::approve()).await.expect("approve");
    store.decide(&scope, &fingerprint, ToolApproval::deny()).await.expect("then deny");
    assert_eq!(take(&*store, &scope, &fingerprint).await, Some(ToolConfirmationDecision::Deny));
    assert_eq!(take(&*store, &scope, &fingerprint).await, None);
}

/// A decision past its expiry is discarded and taken as `None`; one that has not
/// expired is taken normally.
pub async fn expired_decisions_are_discarded<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let scope = scope();
    let fingerprint = request(10, "call-1").fingerprint();

    let expired = ToolApproval::approve().expires_at(Utc::now() - chrono::Duration::seconds(1));
    store.decide(&scope, &fingerprint, expired).await.expect("decide");
    assert_eq!(take(&*store, &scope, &fingerprint).await, None, "an expired decision");

    let live = ToolApproval::deny().expires_in(Duration::from_secs(3_600));
    store.decide(&scope, &fingerprint, live).await.expect("decide");
    assert!(has(&*store, &scope, &fingerprint).await, "a live decision is reported");
    assert_eq!(take(&*store, &scope, &fingerprint).await, Some(ToolConfirmationDecision::Deny));
}

/// A decision recorded for one session is never taken by another session, user, or
/// application.
pub async fn decisions_stay_in_their_scope<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let scope = scope();
    let fingerprint = request(10, "call-1").fingerprint();
    store.decide(&scope, &fingerprint, ToolApproval::approve()).await.expect("decide");

    for other in [
        ApprovalScope::new(&scope.app_name, &scope.user_id, unique("session")),
        ApprovalScope::new(&scope.app_name, unique("user"), &scope.session_id),
        ApprovalScope::new(unique("app"), &scope.user_id, &scope.session_id),
    ] {
        assert_eq!(take(&*store, &other, &fingerprint).await, None, "{other:?}");
    }
    assert_eq!(
        take(&*store, &scope, &fingerprint).await,
        Some(ToolConfirmationDecision::Approve),
        "the decision is still there for its own scope"
    );
}

/// Concurrent takes of one decision, split across two handles, return it to exactly one
/// caller, so one approval authorizes one execution.
pub async fn concurrent_take_has_one_winner<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let first = harness.open().await;
    let second = harness.open().await;
    let scope = scope();
    let fingerprint = request(10, "call-1").fingerprint();
    first.decide(&scope, &fingerprint, ToolApproval::approve()).await.expect("decide");

    let tasks: Vec<_> = (0..16)
        .map(|i| {
            let store = if i % 2 == 0 { first.clone() } else { second.clone() };
            let scope = scope.clone();
            let fingerprint = fingerprint.clone();
            tokio::spawn(async move { store.take_decision(&scope, &fingerprint).await })
        })
        .collect();
    let mut taken = Vec::new();
    for task in tasks {
        if let Some(decision) = task.await.expect("take task completes").expect("take succeeds") {
            taken.push(decision);
        }
    }
    assert_eq!(taken, [ToolConfirmationDecision::Approve], "exactly one take wins");
}

/// Pending requests and decisions read back unchanged through a new handle.
pub async fn records_survive_a_restart<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let scope = scope();
    let waiting = request(10, "call-1");
    let decided = request(20, "call-2");
    let before = {
        let store = harness.open().await;
        store.record_pending(&scope, &waiting).await.expect("record");
        store.record_pending(&scope, &decided).await.expect("record");
        let approval = ToolApproval::approve().expires_in(Duration::from_secs(3_600));
        store.decide(&scope, &decided.fingerprint(), approval).await.expect("decide");
        pending(&*store, &scope).await
    };
    assert_eq!(before.len(), 1, "{before:?}");

    let store = harness.open().await;
    assert_eq!(pending(&*store, &scope).await, before, "pending requests survive");
    assert!(has(&*store, &scope, &decided.fingerprint()).await, "the decision survives");
    assert_eq!(
        take(&*store, &scope, &decided.fingerprint()).await,
        Some(ToolConfirmationDecision::Approve)
    );
}
