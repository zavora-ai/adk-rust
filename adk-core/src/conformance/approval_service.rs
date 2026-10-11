//! Conformance cases for the [`StoreApprovalService`] over an [`ApprovalStore`] backend.
//!
//! The cases run the service against the harness's store, driven by a [`ManualClock`],
//! so they check the service's behaviour and the store primitives it relies on together.
//!
//! | Case | Contract |
//! |------|----------|
//! | [`expiry_denies_the_call`] | `tick` past the expiry expires the request once and records a denial for the call |
//! | [`quorum_needs_distinct_approvers`] | With a quorum of 2 one approval keeps the request pending and records no decision; the same approver cannot count twice; a second approver resolves it |
//! | [`self_approval_is_refused`] | The requester cannot approve their own call unless the policy allows it |
//! | [`delegation_applies_only_in_its_window`] | A delegate decides only inside the delegation's window, and counts for the delegator |
//! | [`concurrent_decide_has_one_winner`] | Concurrent decisions on one version, across two handles, have exactly one winner |
//! | [`edit_substitutes_arguments`] | An edit resolves the request with the edited arguments on the recorded decision |
//! | [`escalation_fires_once`] | An escalation step fires once however often `tick` runs, and only then admits its approvers |
//! | [`inbox_lists_what_an_approver_may_decide`] | The inbox holds what the approver may decide, directly or by delegation, and pages by cursor |
//! | [`cancel_and_reject_deny_the_call`] | Cancelling or rejecting records a denial carrying the reason |
//! | [`requests_survive_a_restart`] | A partly approved request survives a restart and is resolved through a new handle |
//!
//! Each case lists and ticks only its own application's requests, so cases sharing one
//! database do not expire or escalate each other's requests.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::approval_store::ApprovalStoreHarness;
use super::{base_time, unique, unique_unicode};
use crate::principal::SubjectRef;
use crate::{
    ApprovalAction, ApprovalDecision, ApprovalDelegation, ApprovalEventKind, ApprovalNotification,
    ApprovalNotifier, ApprovalPolicy, ApprovalRequest, ApprovalRequestQuery, ApprovalScope,
    ApprovalService, ApprovalState, ApprovalStore, ApprovalTick, ApproverSelector, Clock,
    DelegationScope, DelegationWindow, InboxQuery, ManualClock, PendingApproval, Result,
    StoreApprovalService, ToolApproval, ToolConfirmationDecision, ToolConfirmationRequest,
};

/// Restricts listings to one application, so a case's `tick` and inbox see only its
/// own requests in a shared database.
#[derive(Debug)]
struct AppScoped {
    inner: Arc<dyn ApprovalStore>,
    app: String,
}

#[async_trait]
impl ApprovalStore for AppScoped {
    async fn record_pending(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> Result<()> {
        self.inner.record_pending(scope, request).await
    }

    async fn pending(&self, scope: &ApprovalScope) -> Result<Vec<PendingApproval>> {
        self.inner.pending(scope).await
    }

    async fn decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: ToolApproval,
    ) -> Result<()> {
        self.inner.decide(scope, fingerprint, approval).await
    }

    async fn take_decision(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<ToolConfirmationDecision>> {
        self.inner.take_decision(scope, fingerprint).await
    }

    async fn has_decision(&self, scope: &ApprovalScope, fingerprint: &str) -> Result<bool> {
        self.inner.has_decision(scope, fingerprint).await
    }

    async fn take_approval(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<ToolApproval>> {
        self.inner.take_approval(scope, fingerprint).await
    }

    async fn create_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
        self.inner.create_request(request).await
    }

    async fn get_request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        self.inner.get_request(id).await
    }

    async fn replace_request(
        &self,
        request: &ApprovalRequest,
        expected_version: u64,
    ) -> Result<bool> {
        self.inner.replace_request(request, expected_version).await
    }

    async fn list_requests(&self, query: &ApprovalRequestQuery) -> Result<Vec<ApprovalRequest>> {
        let mut query = query.clone();
        query.app_name = Some(self.app.clone());
        self.inner.list_requests(&query).await
    }

    async fn put_delegation(&self, delegation: &ApprovalDelegation) -> Result<()> {
        self.inner.put_delegation(delegation).await
    }

    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
        self.inner.delegations_to(to).await
    }

    async fn delete_delegation(&self, id: &str) -> Result<bool> {
        self.inner.delete_delegation(id).await
    }
}

/// Records every notification.
#[derive(Debug, Default)]
struct Recorder(Mutex<Vec<ApprovalNotification>>);

impl Recorder {
    fn kinds(&self) -> Vec<ApprovalEventKind> {
        self.0.lock().expect("recorder lock").iter().map(|n| n.kind).collect()
    }
}

#[async_trait]
impl ApprovalNotifier for Recorder {
    async fn notify(&self, notification: &ApprovalNotification) -> Result<()> {
        self.0.lock().expect("recorder lock").push(notification.clone());
        Ok(())
    }
}

/// One case's service, clock, notifications, and application.
struct Fixture {
    service: StoreApprovalService,
    store: Arc<dyn ApprovalStore>,
    clock: ManualClock,
    recorder: Arc<Recorder>,
    app: String,
}

impl Fixture {
    async fn open<H: ApprovalStoreHarness + ?Sized>(harness: &H, app: &str) -> Self {
        let store = harness.open().await;
        let clock = ManualClock::new(base_time());
        let recorder = Arc::new(Recorder::default());
        let scoped: Arc<dyn ApprovalStore> =
            Arc::new(AppScoped { inner: store.clone(), app: app.to_string() });
        let service = StoreApprovalService::new(scoped)
            .with_clock(Arc::new(clock.clone()))
            .with_notifier(recorder.clone());
        Self { service, store, clock, recorder, app: app.to_string() }
    }

    async fn new<H: ApprovalStoreHarness + ?Sized>(harness: &H) -> Self {
        Self::open(harness, &unique_unicode("app")).await
    }

    fn call(&self, amount: u64) -> (ApprovalScope, ToolConfirmationRequest) {
        let scope = ApprovalScope::new(&self.app, unique("user"), unique("session"));
        let call = ToolConfirmationRequest {
            tool_name: "conformance_transfer".into(),
            function_call_id: Some(unique("call")),
            args: json!({ "amount": amount, "to": "acct:1/main" }),
            acting: None,
        };
        (scope, call)
    }

    /// Holds a call in the store and submits its request under `policy`.
    async fn submit(&self, policy: ApprovalPolicy) -> ApprovalRequest {
        let (scope, call) = self.call(250);
        self.store.record_pending(&scope, &call).await.expect("record_pending");
        let request = ApprovalRequest::new(unique_unicode("request"), scope, &call, policy)
            .with_requester(SubjectRef::human("conformance:requester"));
        self.service.submit(request).await.expect("submit")
    }

    async fn tick(&self) -> ApprovalTick {
        self.service.tick(self.clock.now()).await.expect("tick")
    }

    async fn taken(&self, request: &ApprovalRequest) -> Option<ToolApproval> {
        self.store
            .take_approval(&request.scope, &request.fingerprint)
            .await
            .expect("take_approval reads the store")
    }

    async fn has_decision(&self, request: &ApprovalRequest) -> bool {
        self.store
            .has_decision(&request.scope, &request.fingerprint)
            .await
            .expect("has_decision reads the store")
    }
}

fn finance() -> ApprovalPolicy {
    ApprovalPolicy::new([ApproverSelector::group("conformance:finance")])
}

fn member(name: &str) -> ApprovalDecision {
    ApprovalDecision::approve(SubjectRef::human(name)).with_groups(["conformance:finance"])
}

fn code(result: Result<ApprovalRequest>) -> String {
    match result {
        Ok(request) => panic!("expected an error, got {:?}", request.state),
        Err(error) => error.code.to_string(),
    }
}

fn minutes(n: u64) -> Duration {
    Duration::from_secs(n * 60)
}

fn at(offset: Duration) -> DateTime<Utc> {
    base_time() + chrono::Duration::from_std(offset).expect("offset fits")
}

/// A request nobody resolves expires at its first `tick` past the expiry, once, and the
/// call's decision is a denial.
pub async fn expiry_denies_the_call<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let fx = Fixture::new(harness).await;
    let request = fx.submit(finance().expires_after(minutes(10))).await;
    assert_eq!(request.expires_at, Some(at(minutes(10))), "expiry set from the policy");

    fx.clock.advance(minutes(5));
    assert_eq!(fx.tick().await, ApprovalTick::default(), "nothing is due before the expiry");
    assert!(!fx.has_decision(&request).await, "no decision while pending");

    fx.clock.advance(minutes(6));
    let report = fx.tick().await;
    assert_eq!(report.expired, std::slice::from_ref(&request.id), "the request expires");
    assert_eq!(fx.tick().await, ApprovalTick::default(), "and expires once");

    let expired = fx.service.get(&request.id).await.expect("get").expect("stored");
    assert_eq!(expired.state, ApprovalState::Expired);
    assert!(expired.outcome_applied, "the denial was written");
    let decision = fx.taken(&request).await.expect("a decision for the call");
    assert_eq!(decision.decision, ToolConfirmationDecision::Deny, "expiry denies the call");
    assert!(decision.message.is_some_and(|message| message.contains("expired")));
    assert_eq!(
        code(fx.service.decide(&request.id, expired.version, member("ana")).await),
        "approval.already_resolved",
        "an expired request takes no decisions"
    );
    assert_eq!(
        fx.recorder.kinds(),
        [ApprovalEventKind::Submitted, ApprovalEventKind::Expired],
        "notified on submission and expiry"
    );
}

/// With a quorum of 2, the first approval leaves the request pending and the call
/// without a decision; the same approver cannot approve twice; a second approver
/// resolves it and the call is approved.
pub async fn quorum_needs_distinct_approvers<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let fx = Fixture::new(harness).await;
    let request = fx.submit(finance().with_quorum(2)).await;

    let first = fx.service.decide(&request.id, request.version, member("ana")).await.expect("ana");
    assert_eq!(first.state, ApprovalState::Pending, "one of two approvals");
    assert_eq!(first.version, request.version + 1);
    assert!(!fx.has_decision(&first).await, "the call stays held");

    assert_eq!(
        code(fx.service.decide(&first.id, first.version, member("ana")).await),
        "approval.already_decided",
        "the same approver counts once"
    );
    assert_eq!(
        code(
            fx.service
                .decide(
                    &first.id,
                    first.version,
                    ApprovalDecision::approve(SubjectRef::human("eve"))
                )
                .await
        ),
        "approval.not_an_approver",
        "someone outside the group cannot approve"
    );

    let second = fx.service.decide(&first.id, first.version, member("bo")).await.expect("bo");
    assert_eq!(second.state, ApprovalState::Approved);
    assert!(second.outcome_applied);
    assert_eq!(second.decisions.len(), 2);
    assert_eq!(fx.taken(&second).await, Some(ToolApproval::approve()), "the call is approved");
}

/// The requester cannot approve their own call, even through a delegation, unless the
/// policy allows self-approval.
pub async fn self_approval_is_refused<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let fx = Fixture::new(harness).await;
    let requester = || member("conformance:requester");
    let request = fx.submit(finance()).await;
    assert_eq!(
        code(fx.service.decide(&request.id, request.version, requester()).await),
        "approval.self_approval"
    );

    let delegate = SubjectRef::human(unique("delegate"));
    fx.service
        .delegate(
            ApprovalDelegation::new(
                SubjectRef::human("conformance:requester"),
                delegate.clone(),
                DelegationWindow::new(at(Duration::ZERO), at(minutes(60))),
            )
            .with_from_groups(["conformance:finance"]),
        )
        .await
        .expect("delegate");
    assert_eq!(
        code(
            fx.service
                .decide(&request.id, request.version, ApprovalDecision::approve(delegate))
                .await
        ),
        "approval.self_approval",
        "a delegate of the requester cannot approve for them"
    );

    let allowed = fx.submit(finance().allow_self_approval()).await;
    let decided =
        fx.service.decide(&allowed.id, allowed.version, requester()).await.expect("allowed");
    assert_eq!(decided.state, ApprovalState::Approved);
}

/// A delegate decides in the delegator's place only inside the delegation's window and
/// scope, and their approval counts for the delegator.
pub async fn delegation_applies_only_in_its_window<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let fx = Fixture::new(harness).await;
    let ana = SubjectRef::human(unique("ana"));
    let bo = SubjectRef::human(unique("bo"));
    let request = fx
        .submit(ApprovalPolicy::new([ApproverSelector::Subject(ana.clone())]).with_quorum(2))
        .await;
    // Quorum 2 with one listed approver: ana's own approval and her delegate's count once.
    let delegation = fx
        .service
        .delegate(
            ApprovalDelegation::new(
                ana.clone(),
                bo.clone(),
                DelegationWindow::new(at(minutes(60)), at(minutes(120))),
            )
            .with_scope(DelegationScope { app_name: Some(fx.app.clone()), tool_names: vec![] }),
        )
        .await
        .expect("delegate");
    let by_bo = || ApprovalDecision::approve(bo.clone());

    assert_eq!(
        code(fx.service.decide(&request.id, request.version, by_bo()).await),
        "approval.delegation_inactive",
        "before the window"
    );
    fx.clock.advance(minutes(90));
    let decided = fx.service.decide(&request.id, request.version, by_bo()).await.expect("bo");
    assert_eq!(decided.decisions[0].via_delegation, Some(ana.clone()), "acted for ana");
    assert_eq!(
        code(fx.service.decide(&decided.id, decided.version, ApprovalDecision::approve(ana)).await),
        "approval.already_decided",
        "the delegate's approval counted for the delegator"
    );

    fx.clock.advance(minutes(60));
    let other = fx.submit(ApprovalPolicy::new([ApproverSelector::Subject(delegation.from)])).await;
    assert_eq!(
        code(fx.service.decide(&other.id, other.version, by_bo()).await),
        "approval.delegation_inactive",
        "after the window"
    );
    assert!(fx.service.revoke_delegation(&delegation.id).await.expect("revoke"));
    assert_eq!(
        code(fx.service.decide(&other.id, other.version, by_bo()).await),
        "approval.not_an_approver",
        "a revoked delegation no longer applies"
    );
}

/// Concurrent decisions on the same version, split across two handles, have one winner;
/// the losers see a conflict and the stored request holds the winner's decision only.
pub async fn concurrent_decide_has_one_winner<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let app = unique_unicode("app");
    let first = Arc::new(Fixture::open(harness, &app).await);
    let second = Arc::new(Fixture::open(harness, &app).await);
    let request = first.submit(finance()).await;

    let tasks: Vec<_> = (0..12)
        .map(|i| {
            let fx = if i % 2 == 0 { first.clone() } else { second.clone() };
            let id = request.id.clone();
            let version = request.version;
            tokio::spawn(async move {
                fx.service.decide(&id, version, member(&format!("approver-{i}"))).await
            })
        })
        .collect();
    let mut winners = Vec::new();
    for task in tasks {
        match task.await.expect("decide task completes") {
            Ok(request) => winners.push(request),
            Err(error) => assert!(error.is_conflict(), "a loser sees a conflict: {error}"),
        }
    }
    assert_eq!(winners.len(), 1, "exactly one decision wins");
    let stored = second.service.get(&request.id).await.expect("get").expect("stored");
    assert_eq!(stored.decisions, winners[0].decisions, "only the winner's decision is stored");
    assert_eq!(stored.state, ApprovalState::Approved);
    assert_eq!(first.taken(&stored).await, Some(ToolApproval::approve()), "one decision recorded");
    assert_eq!(first.taken(&stored).await, None, "and taken once");
}

/// An edit resolves the request; the recorded decision carries the edited arguments,
/// and with a quorum the edit restarts the count.
pub async fn edit_substitutes_arguments<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let fx = Fixture::new(harness).await;
    let edited: Value = json!({ "amount": 40, "to": "acct:1/main", "memo": "ü-日本" });
    let request = fx.submit(finance().with_quorum(2)).await;

    let approved =
        fx.service.decide(&request.id, request.version, member("ana")).await.expect("ana");
    let edit = ApprovalDecision::edit(SubjectRef::human("bo"), edited.clone())
        .with_groups(["conformance:finance"]);
    let after_edit = fx.service.decide(&approved.id, approved.version, edit).await.expect("edit");
    assert_eq!(after_edit.state, ApprovalState::Pending, "the edit restarts the quorum");
    assert_eq!(after_edit.quorum_progress().1, Some(edited.clone()));

    let resolved =
        fx.service.decide(&after_edit.id, after_edit.version, member("ana")).await.expect("ana");
    assert_eq!(resolved.state, ApprovalState::Approved);
    assert_eq!(
        fx.taken(&resolved).await,
        Some(ToolApproval::approve().with_edited_args(edited)),
        "the decision carries the edited arguments"
    );

    let (scope, call) = fx.call(1);
    let other = fx
        .service
        .submit(
            ApprovalRequest::new(unique("request"), scope, &call, finance())
                .with_allowed_actions([ApprovalAction::Approve, ApprovalAction::Reject]),
        )
        .await
        .expect("submit");
    let edit = ApprovalDecision::edit(SubjectRef::human("bo"), json!({}))
        .with_groups(["conformance:finance"]);
    assert_eq!(
        code(fx.service.decide(&other.id, other.version, edit).await),
        "approval.action_not_allowed",
        "a request can forbid edits"
    );
}

/// An escalation step fires once, at the first `tick` past its delay, and admits its
/// approvers only from then on.
pub async fn escalation_fires_once<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let fx = Fixture::new(harness).await;
    let request = fx
        .submit(finance().escalate_after(minutes(15), [ApproverSelector::group("conformance:cfo")]))
        .await;
    let cfo =
        || ApprovalDecision::approve(SubjectRef::human("cy")).with_groups(["conformance:cfo"]);
    assert_eq!(
        code(fx.service.decide(&request.id, request.version, cfo()).await),
        "approval.not_an_approver",
        "not yet escalated"
    );

    fx.clock.advance(minutes(10));
    assert_eq!(fx.tick().await, ApprovalTick::default(), "not due yet");
    fx.clock.advance(minutes(6));
    let report = fx.tick().await;
    assert_eq!(report.escalated, [(request.id.clone(), 0)], "the step fires");
    fx.clock.advance(minutes(30));
    assert_eq!(fx.tick().await, ApprovalTick::default(), "and fires once");

    let escalated = fx.service.get(&request.id).await.expect("get").expect("stored");
    assert_eq!(escalated.escalations_fired, 1);
    let decided =
        fx.service.decide(&escalated.id, escalated.version, cfo()).await.expect("cfo decides");
    assert_eq!(decided.state, ApprovalState::Approved);
    let escalations = fx
        .recorder
        .0
        .lock()
        .expect("recorder lock")
        .iter()
        .filter(|n| n.kind == ApprovalEventKind::Escalated)
        .map(|n| n.escalation_step)
        .collect::<Vec<_>>();
    assert_eq!(escalations, [Some(0)], "one escalation notification");
}

/// The inbox holds the pending requests an approver may decide — through a selector
/// naming them, one of their groups, or a live delegation — and pages by cursor.
pub async fn inbox_lists_what_an_approver_may_decide<H: ApprovalStoreHarness + ?Sized>(
    harness: &H,
) {
    let fx = Fixture::new(harness).await;
    let ana = SubjectRef::human(unique("ana"));
    let by_group = fx.submit(finance()).await;
    fx.clock.advance(Duration::from_secs(1));
    let by_name = fx.submit(ApprovalPolicy::new([ApproverSelector::Subject(ana.clone())])).await;
    fx.clock.advance(Duration::from_secs(1));
    let ops = fx.submit(ApprovalPolicy::new([ApproverSelector::group("conformance:ops")])).await;
    fx.clock.advance(Duration::from_secs(1));
    let delegated = fx.submit(ApprovalPolicy::new([ApproverSelector::human("dee")])).await;
    fx.service
        .delegate(ApprovalDelegation::new(
            SubjectRef::human("dee"),
            ana.clone(),
            DelegationWindow::new(at(Duration::ZERO), at(minutes(60))),
        ))
        .await
        .expect("delegate");
    let ids = |page: crate::InboxPage| page.items.into_iter().map(|r| r.id).collect::<Vec<_>>();

    let inbox = InboxQuery::for_approver(ana.clone(), ["conformance:finance"]);
    assert_eq!(
        ids(fx.service.inbox(inbox.clone()).await.expect("inbox")),
        [by_group.id.clone(), by_name.id.clone(), delegated.id.clone()],
        "by group, by name, and by delegation, oldest first"
    );
    let first = fx.service.inbox(inbox.clone().with_limit(2)).await.expect("inbox");
    assert_eq!(ids(first.clone()), [by_group.id.clone(), by_name.id.clone()]);
    let next = fx.service.inbox(inbox.with_limit(2).with_cursor(first.next_cursor)).await;
    assert_eq!(ids(next.expect("next page")), std::slice::from_ref(&delegated.id));
    assert_eq!(
        ids(fx.service.inbox(InboxQuery::for_group("conformance:ops")).await.expect("inbox")),
        std::slice::from_ref(&ops.id),
        "a group's queue"
    );

    fx.service
        .decide(&by_name.id, by_name.version, ApprovalDecision::approve(ana.clone()))
        .await
        .expect("decide");
    let resolved = InboxQuery::for_approver(ana, ["conformance:finance"])
        .with_states([ApprovalState::Approved]);
    assert_eq!(
        ids(fx.service.inbox(resolved).await.expect("inbox")),
        [by_name.id],
        "a decided request leaves the pending inbox"
    );
}

/// Cancelling or rejecting a request records a denial for the call that carries the
/// reason, and a resolved request cannot be cancelled.
pub async fn cancel_and_reject_deny_the_call<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let fx = Fixture::new(harness).await;
    let cancelled = fx.submit(finance()).await;
    let after = fx
        .service
        .cancel(&cancelled.id, Some("superseded by INV-43".into()))
        .await
        .expect("cancel");
    assert_eq!(after.state, ApprovalState::Cancelled);
    let denial = fx.taken(&after).await.expect("a decision");
    assert_eq!(denial.decision, ToolConfirmationDecision::Deny);
    assert!(denial.message.is_some_and(|m| m.contains("superseded by INV-43")));
    assert_eq!(code(fx.service.cancel(&cancelled.id, None).await), "approval.already_resolved");

    let rejected = fx.submit(finance()).await;
    let reject = ApprovalDecision::reject(SubjectRef::human("ana"), "over budget")
        .with_groups(["conformance:finance"]);
    let after = fx.service.decide(&rejected.id, rejected.version, reject).await.expect("reject");
    assert_eq!(after.state, ApprovalState::Rejected);
    let denial = fx.taken(&after).await.expect("a decision");
    assert_eq!(denial.decision, ToolConfirmationDecision::Deny);
    assert!(denial.message.is_some_and(|m| m.contains("over budget")));
}

/// A request with one of two approvals survives a restart: a new handle reads it back,
/// lists it in the inbox, and resolves it with the second approval.
pub async fn requests_survive_a_restart<H: ApprovalStoreHarness + ?Sized>(harness: &H) {
    let app = unique_unicode("app");
    let half = {
        let fx = Fixture::open(harness, &app).await;
        let request = fx.submit(finance().with_quorum(2)).await;
        fx.service.decide(&request.id, request.version, member("ana")).await.expect("ana")
    };

    let fx = Fixture::open(harness, &app).await;
    assert_eq!(fx.service.get(&half.id).await.expect("get"), Some(half.clone()));
    let inbox = fx
        .service
        .inbox(InboxQuery::for_approver(SubjectRef::human("bo"), ["conformance:finance"]))
        .await
        .expect("inbox");
    assert_eq!(inbox.items, std::slice::from_ref(&half), "still in the inbox");
    let resolved = fx.service.decide(&half.id, half.version, member("bo")).await.expect("bo");
    assert_eq!(resolved.state, ApprovalState::Approved);
    assert_eq!(fx.taken(&resolved).await, Some(ToolApproval::approve()));
}
