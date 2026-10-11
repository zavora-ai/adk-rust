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
//! | [`decisions_carry_edits_and_messages`] | `take_approval` returns a decision's edited arguments and message once; `take_decision` reports the bare decision |
//! | [`requests_are_created_once`] | `create_request` keeps the first request under an ID and returns it unchanged |
//! | [`concurrent_replace_has_one_winner`] | Of concurrent `replace_request` calls at one version, across two handles, exactly one succeeds |
//! | [`requests_list_in_order_with_filters`] | `list_requests` filters by state, openness, and application, orders by creation time then ID, and pages by cursor |
//! | [`delegations_round_trip`] | Delegations are stored, replaced by ID, listed per delegate by ID, and deleted once |
//! | [`approval_requests_survive_a_restart`] | Requests and delegations read back unchanged after a restart |
//!
//! Every request carries an [`ActingContext`], and the cases compare whole requests,
//! so each backend proves the acting agent is recorded with the held call.
//!
//! `has_decision` may report `true` when no live decision exists (the trait's default
//! does), so the suite checks only that it never misses a live decision and never
//! consumes one.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;

use super::{base_time, unique, unique_unicode};
use crate::principal::SubjectRef;
use crate::{
    ActingContext, AgentId, AgentPrincipal, ApprovalCursor, ApprovalDecision, ApprovalDelegation,
    ApprovalPolicy, ApprovalRequest, ApprovalRequestQuery, ApprovalScope, ApprovalState,
    ApprovalStore, ApproverSelector, DelegationScope, DelegationWindow, PendingApproval,
    ToolApproval, ToolConfirmationDecision, ToolConfirmationRequest,
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
        // Every case compares whole requests, so each backend proves the acting context
        // round-trips with the call.
        acting: Some(Box::new(
            ActingContext::new(
                AgentPrincipal::new(
                    AgentId::new("spiffe://conformance.test/agent/treasurer").unwrap(),
                )
                .with_tenant("conformance")
                .with_instance("replica-1"),
                format!("inv-{call_id}"),
            )
            .on_behalf_of(SubjectRef::human("alice")),
        )),
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

/// `take_approval` returns the edited arguments and message a decision carries, once;
/// `take_decision` reports a decision without them.
pub async fn decisions_carry_edits_and_messages<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let scope = scope();
    let edited = request(10, "call-1").fingerprint();
    let refused = request(20, "call-2").fingerprint();
    let bare = request(30, "call-3").fingerprint();

    let edit = ToolApproval::approve()
        .with_edited_args(json!({ "amount": 5, "to": "acct:1/main", "memo": "ü-日本" }));
    let denial = ToolApproval::deny().with_message("the approver rejected it: over budget");
    store.decide(&scope, &edited, edit.clone()).await.expect("decide");
    store.decide(&scope, &refused, denial.clone()).await.expect("decide");
    store.decide(&scope, &bare, ToolApproval::approve()).await.expect("decide");

    assert!(has(&*store, &scope, &edited).await, "an edited approval is a live decision");
    let take_full = |fingerprint: String| {
        let store = store.clone();
        let scope = scope.clone();
        async move { store.take_approval(&scope, &fingerprint).await.expect("take_approval") }
    };
    assert_eq!(take_full(edited.clone()).await, Some(edit), "edited arguments round-trip");
    assert_eq!(take_full(edited).await, None, "taken once");
    assert_eq!(take_full(refused).await, Some(denial), "the message round-trips");
    assert_eq!(
        take(&*store, &scope, &bare).await,
        Some(ToolConfirmationDecision::Approve),
        "take_decision reports the bare decision"
    );
}

fn approval_request(app: &str, id: &str, offset_secs: i64) -> ApprovalRequest {
    let mut request = ApprovalRequest::new(
        id,
        ApprovalScope::new(app, unique("user"), unique("session")),
        &request(u64::try_from(offset_secs).unwrap_or(0), "call-1"),
        ApprovalPolicy::new([ApproverSelector::group("conformance:approvers")]).with_quorum(2),
    )
    .with_summary("Transfer to ü-日本")
    .with_requester(SubjectRef::human(unique("requester")));
    request.created_at = base_time() + chrono::Duration::seconds(offset_secs);
    request
}

async fn stored(store: &dyn ApprovalStore, id: &str) -> Option<ApprovalRequest> {
    store.get_request(id).await.expect("get_request reads the store")
}

/// `create_request` stores a request once: creating the same ID again returns the first
/// request unchanged, and `get_request` reads it back exactly.
pub async fn requests_are_created_once<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let app = unique("app");
    let first = approval_request(&app, &unique_unicode("request"), 0);
    assert_eq!(store.create_request(&first).await.expect("create"), first, "created");
    assert_eq!(stored(&*store, &first.id).await, Some(first.clone()), "reads back exactly");

    let mut second = approval_request(&app, &first.id, 30);
    second.summary = "a different request under the same id".into();
    assert_eq!(
        store.create_request(&second).await.expect("create again"),
        first,
        "creating an existing id returns the stored request"
    );
    assert_eq!(stored(&*store, &first.id).await, Some(first), "the first request is kept");
    assert_eq!(stored(&*store, &unique("missing")).await, None, "an unknown id reads as None");
}

/// Concurrent `replace_request` calls at the same expected version, split across two
/// handles, have exactly one winner, whose write is the one stored.
pub async fn concurrent_replace_has_one_winner<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let first = harness.open().await;
    let second = harness.open().await;
    let original = approval_request(&unique("app"), &unique_unicode("request"), 0);
    first.create_request(&original).await.expect("create");

    let tasks: Vec<_> = (0..16)
        .map(|i| {
            let store = if i % 2 == 0 { first.clone() } else { second.clone() };
            let mut next = original.clone();
            next.version = 2;
            next.decisions
                .push(ApprovalDecision::approve(SubjectRef::human(format!("approver-{i}"))));
            tokio::spawn(async move {
                let won = store.replace_request(&next, 1).await.expect("replace succeeds");
                won.then_some(next)
            })
        })
        .collect();
    let mut winners = Vec::new();
    for task in tasks {
        if let Some(next) = task.await.expect("replace task completes") {
            winners.push(next);
        }
    }
    assert_eq!(winners.len(), 1, "exactly one replace wins");
    assert_eq!(
        stored(&*second, &original.id).await,
        Some(winners.remove(0)),
        "the winner is stored"
    );

    let mut stale = original.clone();
    stale.version = 2;
    assert!(
        !first.replace_request(&stale, 1).await.expect("replace"),
        "a replace at a superseded version is refused"
    );
    let mut missing = original.clone();
    missing.id = unique("missing");
    assert!(!first.replace_request(&missing, 1).await.expect("replace"), "a missing request");
}

/// `list_requests` filters by state, openness, and application, orders by creation time
/// and then ID, and pages from a cursor.
pub async fn requests_list_in_order_with_filters<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let app = unique_unicode("app");
    let prefix = unique("request");
    let id = |suffix: &str| format!("{prefix}:{suffix}");
    // Two requests share a creation time, so the ID breaks the tie.
    let mut pending_late = approval_request(&app, &id("b"), 20);
    let pending_tied = approval_request(&app, &id("a"), 20);
    let pending_early = approval_request(&app, &id("z"), 10);
    let mut approved_unapplied = approval_request(&app, &id("c"), 30);
    approved_unapplied.state = ApprovalState::Approved;
    let mut rejected_applied = approval_request(&app, &id("d"), 40);
    rejected_applied.state = ApprovalState::Rejected;
    rejected_applied.outcome_applied = true;
    pending_late.summary = "late".into();
    let other_app = approval_request(&unique("app"), &unique("other"), 15);
    for request in [
        &pending_late,
        &pending_tied,
        &pending_early,
        &approved_unapplied,
        &rejected_applied,
        &other_app,
    ] {
        store.create_request(request).await.expect("create");
    }
    let ids = |requests: Vec<ApprovalRequest>| -> Vec<String> {
        requests.into_iter().map(|request| request.id).collect()
    };
    let list = |query: ApprovalRequestQuery| {
        let store = store.clone();
        async move { store.list_requests(&query).await.expect("list_requests reads the store") }
    };
    let in_app = ApprovalRequestQuery::default().with_app_name(app.clone());

    assert_eq!(
        ids(list(in_app.clone()).await),
        [id("z"), id("a"), id("b"), id("c"), id("d")],
        "every request of the application, by creation time and then id"
    );
    assert_eq!(
        ids(list(in_app.clone().with_states([ApprovalState::Pending])).await),
        [id("z"), id("a"), id("b")],
        "pending only"
    );
    assert_eq!(
        ids(list(in_app.clone().with_states([ApprovalState::Approved, ApprovalState::Rejected]))
            .await),
        [id("c"), id("d")],
        "either of two states"
    );
    assert_eq!(
        ids(list(in_app.clone().open_only()).await),
        [id("z"), id("a"), id("b"), id("c")],
        "open requests: pending, or resolved and not yet applied"
    );
    let first_page = list(in_app.clone().with_limit(2)).await;
    assert_eq!(ids(first_page.clone()), [id("z"), id("a")], "a page of two");
    let cursor = first_page.last().map(ApprovalRequest::cursor);
    assert_eq!(
        ids(list(in_app.clone().with_after(cursor).with_limit(2)).await),
        [id("b"), id("c")],
        "the next page starts after the cursor, inside a tie on creation time"
    );
    let decoded = ApprovalCursor::decode(&pending_late.cursor().encode()).expect("decode");
    assert_eq!(
        ids(list(in_app.with_after(Some(decoded))).await),
        [id("c"), id("d")],
        "an encoded cursor pages the same way"
    );
    assert_eq!(
        list(ApprovalRequestQuery::default().with_app_name(app.clone()).with_limit(1)).await[0],
        pending_early,
        "listed requests read back exactly"
    );
}

/// Delegations are stored, replaced by ID, listed for their delegate ordered by ID, and
/// deleted once.
pub async fn delegations_round_trip<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let delegate = SubjectRef::human(unique_unicode("delegate"));
    let window = DelegationWindow::new(base_time(), base_time() + chrono::Duration::days(7));
    let delegation = |id: &str, from: &str, to: &SubjectRef| {
        let mut delegation = ApprovalDelegation::new(SubjectRef::human(from), to.clone(), window)
            .with_from_groups(["conformance:finance"]);
        delegation.id = id.to_string();
        delegation.created_at = base_time();
        delegation
    };
    let second = delegation(&format!("{}/b", unique("delegation")), "ana", &delegate);
    let first = delegation(&format!("{}/a", second.id.trim_end_matches("/b")), "bo", &delegate);
    let elsewhere = delegation(&unique("delegation"), "cy", &SubjectRef::human(unique("other")));
    for delegation in [&second, &first, &elsewhere] {
        store.put_delegation(delegation).await.expect("put_delegation");
    }
    let listed = store.delegations_to(&delegate).await.expect("delegations_to");
    assert_eq!(listed, [first.clone(), second.clone()], "the delegate's delegations, by id");

    let mut narrowed = second.clone();
    narrowed.scope = DelegationScope { app_name: Some("payments".into()), tool_names: vec![] };
    store.put_delegation(&narrowed).await.expect("replace");
    assert_eq!(
        store.delegations_to(&delegate).await.expect("delegations_to"),
        [first.clone(), narrowed.clone()],
        "putting an existing id replaces it"
    );

    assert!(store.delete_delegation(&first.id).await.expect("delete"), "deleted");
    assert!(!store.delete_delegation(&first.id).await.expect("delete"), "deleted once");
    assert_eq!(store.delegations_to(&delegate).await.expect("delegations_to"), [narrowed]);
    assert_eq!(
        store
            .delegations_to(&SubjectRef::service(delegate.id.clone()))
            .await
            .expect("delegations_to"),
        [],
        "a subject of another kind with the same id has none"
    );
}

/// Requests and delegations read back unchanged through a new handle.
pub async fn approval_requests_survive_a_restart<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let app = unique("app");
    let mut request = approval_request(&app, &unique_unicode("request"), 0);
    let delegate = SubjectRef::human(unique("delegate"));
    let mut delegation = ApprovalDelegation::new(
        SubjectRef::human("ana"),
        delegate.clone(),
        DelegationWindow::new(base_time(), base_time() + chrono::Duration::hours(1)),
    );
    delegation.created_at = base_time();
    {
        let store = harness.open().await;
        store.create_request(&request).await.expect("create");
        let mut next = request.clone();
        next.version = 2;
        next.decisions.push(ApprovalDecision::approve(SubjectRef::human("ana")));
        assert!(store.replace_request(&next, 1).await.expect("replace"));
        request = next;
        store.put_delegation(&delegation).await.expect("put_delegation");
    }
    let store = harness.open().await;
    assert_eq!(stored(&*store, &request.id).await, Some(request.clone()), "the request survives");
    assert_eq!(
        store
            .list_requests(&ApprovalRequestQuery::default().with_app_name(app).open_only())
            .await
            .expect("list"),
        [request],
        "and lists"
    );
    assert_eq!(store.delegations_to(&delegate).await.expect("list"), [delegation]);
}
