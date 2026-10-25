//! `Harness::decide` records the person's decision only for a call still waiting for one,
//! and refuses it without touching the inbox when it cannot be taken as given
//! (spec 3.3 §7.1, §7.3, §11 S-I2).

#![cfg(feature = "runner")]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adk_core::{
    ApprovalDecision, ApprovalDelegation, ApprovalPolicy, ApprovalRequest, ApprovalScope,
    ApprovalService, ApprovalState, ApprovalStore, ApprovalTick, ApproverSelector,
    InMemoryApprovalStore, InboxPage, InboxQuery, StoreApprovalService, SubjectRef,
    ToolConfirmationDecision,
};
use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, HarnessDecision, OriginPin, PromptAnswer, RuleSuggestion,
    SuggestedScope,
};
use adk_model::ScriptedResponse;
use chrono::{DateTime, Utc};
use common::fixture;
use serde_json::json;

const TIMEOUT: Duration = Duration::from_millis(50);

/// A person who never answers, so every prompt times out and the call waits.
#[derive(Debug)]
struct Away;

#[async_trait::async_trait]
impl ApprovalPrompter for Away {
    async fn prompt(&self, _prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        std::future::pending().await
    }
}

fn append_line() -> ScriptedResponse {
    ScriptedResponse::function_call("bash", json!({ "command": "echo ran >> ran.txt" }))
}

/// An inbox whose requests the `owners` group decides.
fn inbox(store: Arc<dyn ApprovalStore>) -> Arc<StoreApprovalService> {
    Arc::new(
        StoreApprovalService::new(store)
            .with_router(ApprovalPolicy::new([ApproverSelector::group("owners")])),
    )
}

/// Every request of `service` for `scope`, in any state.
async fn requests(service: &dyn ApprovalService, scope: &ApprovalScope) -> Vec<ApprovalRequest> {
    let query = InboxQuery::default().with_session(scope).with_states(ApprovalState::ALL);
    service.inbox(query).await.unwrap().items
}

/// Wave 2 review finding 16: a rule that cannot be remembered refuses the decision before
/// the inbox request is withdrawn, so the call still waits where every surface shows it.
#[tokio::test]
async fn a_rule_that_cannot_be_remembered_leaves_the_request_waiting() {
    let service = inbox(Arc::new(InMemoryApprovalStore::new()));
    let f = fixture(vec![append_line(), ScriptedResponse::text("done")], |b| {
        b.prompter(Arc::new(Away)).confirmation_timeout(TIMEOUT).approval_service(service.clone())
    })
    .await;
    f.send("append a line").await;
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    let unpinned = RuleSuggestion {
        label: "any bash".into(),
        scope: SuggestedScope::Session,
        tool: "bash".into(),
        origin: OriginPin::AnyDeclaration { source: "static".into() },
        predicates: Vec::new(),
        runs_scripts: false,
    };

    let error = f
        .harness
        .decide(
            &f.user,
            &f.session,
            &pending[0].fingerprint,
            HarnessDecision::ApproveAndRemember(unpinned),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "harness.decision.invalid");
    let scope = ApprovalScope::new(common::APP, f.user.to_string(), f.session.to_string());
    let waiting = requests(service.as_ref(), &scope).await;
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].state, ApprovalState::Pending, "the call still waits in the inbox");
    assert_eq!(f.harness.pending_approvals(&f.user, &f.session).await.unwrap().len(), 1);
}

/// The service of `inbox`, except that once armed, an owner rejects the session's open
/// request the next time the inbox is read — between `Harness::decide`'s pending check and
/// its inbox scan.
#[derive(Debug)]
struct RejectedDuringTheScan {
    inner: Arc<StoreApprovalService>,
    armed: AtomicBool,
}

#[async_trait::async_trait]
impl ApprovalService for RejectedDuringTheScan {
    async fn submit(&self, request: ApprovalRequest) -> adk_core::Result<ApprovalRequest> {
        self.inner.submit(request).await
    }
    async fn submit_call(
        &self,
        call: adk_core::ApprovalCall,
    ) -> adk_core::Result<Option<ApprovalRequest>> {
        self.inner.submit_call(call).await
    }
    async fn get(&self, id: &str) -> adk_core::Result<Option<ApprovalRequest>> {
        self.inner.get(id).await
    }
    async fn inbox(&self, query: InboxQuery) -> adk_core::Result<InboxPage> {
        if self.armed.swap(false, Ordering::SeqCst) {
            let open =
                self.inner.inbox(query.clone().with_states([ApprovalState::Pending])).await?;
            let request = &open.items[0];
            let reject =
                ApprovalDecision::reject(SubjectRef::human("bob"), "no").with_groups(["owners"]);
            self.inner.decide(&request.id, request.version, reject).await?;
        }
        self.inner.inbox(query).await
    }
    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> adk_core::Result<ApprovalRequest> {
        self.inner.decide(id, expected_version, decision).await
    }
    async fn delegate(
        &self,
        delegation: ApprovalDelegation,
    ) -> adk_core::Result<ApprovalDelegation> {
        self.inner.delegate(delegation).await
    }
    async fn revoke_delegation(&self, id: &str) -> adk_core::Result<bool> {
        self.inner.revoke_delegation(id).await
    }
    async fn delegations_to(&self, to: &SubjectRef) -> adk_core::Result<Vec<ApprovalDelegation>> {
        self.inner.delegations_to(to).await
    }
    async fn cancel(&self, id: &str, reason: Option<String>) -> adk_core::Result<ApprovalRequest> {
        self.inner.cancel(id, reason).await
    }
    async fn withdraw(
        &self,
        id: &str,
        reason: Option<String>,
    ) -> adk_core::Result<ApprovalRequest> {
        self.inner.withdraw(id, reason).await
    }
    async fn tick(&self, now: DateTime<Utc>) -> adk_core::Result<ApprovalTick> {
        self.inner.tick(now).await
    }
    fn approval_store(&self) -> Option<Arc<dyn ApprovalStore>> {
        self.inner.approval_store()
    }
}

/// Wave 2 review findings 17–20: an approver who rejects the call's request after the
/// person's decision passed its pending check keeps the decision; the harness refuses the
/// person's instead of writing an approval for a call that is no longer waiting.
#[tokio::test]
async fn an_approver_who_settles_the_call_during_the_decision_keeps_it() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let service = Arc::new(RejectedDuringTheScan {
        inner: inbox(store.clone()),
        armed: AtomicBool::new(false),
    });
    let f = fixture(vec![append_line(), ScriptedResponse::text("done")], |b| {
        b.prompter(Arc::new(Away)).confirmation_timeout(TIMEOUT).approval_service(service.clone())
    })
    .await;
    f.send("append a line").await;
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    let fingerprint = pending[0].fingerprint.clone();

    service.armed.store(true, Ordering::SeqCst);
    let error = f
        .harness
        .decide(&f.user, &f.session, &fingerprint, HarnessDecision::Approve)
        .await
        .unwrap_err();
    assert_eq!(error.code, "harness.decision.invalid");
    let scope = ApprovalScope::new(common::APP, f.user.to_string(), f.session.to_string());
    let left = store.take_approval(&scope, &fingerprint).await.unwrap();
    assert_eq!(
        left.map(|approval| approval.decision),
        Some(ToolConfirmationDecision::Deny),
        "the approver's rejection stands"
    );
}
