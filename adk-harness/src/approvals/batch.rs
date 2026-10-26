//! The batches a [`PromptingConfirmationHandler`](super::PromptingConfirmationHandler)
//! keeps, and the change-set decisions it answers their calls from (spec 3.1 §7.26).

use std::collections::{HashMap, HashSet, VecDeque};

use adk_core::{
    AdkError, BatchCall, PolicyCall, PolicyDecision, Result, ToolApproval, ToolConfirmationRequest,
    ToolOrigin,
};
use serde_json::{Value, json};

use super::changeset::{ChangeSet, ChangeSetAnswer, ChangeSetPrompt};
use super::rule::EDIT_TOOLS;
use crate::policy::{HarnessPolicy, PromptContext};

/// Most batches a handler keeps; the oldest is dropped first.
const BATCHES_CAP: usize = 256;

/// A call of a model turn, as `batch_started` reported it.
#[derive(Clone, Debug)]
pub(super) struct BatchedCall {
    pub(super) name: String,
    pub(super) id: String,
    pub(super) args: Value,
    pub(super) origin: Option<ToolOrigin>,
}

/// Where a batch's change set stands.
#[derive(Debug)]
pub(super) enum CardState {
    /// Not composed yet: composed at the batch's first held edit.
    Pending,
    /// Decided: each card call's original arguments and its answer, taken once.
    Decided(HashMap<String, (Value, ToolApproval)>),
    /// Fewer than two held edits, or a prompter that asks one call at a time.
    NotShown,
}

#[derive(Debug)]
struct Batch {
    calls: Vec<BatchedCall>,
    card: CardState,
}

/// The batches of recent model turns, by call ID.
#[derive(Debug, Default)]
pub(super) struct BatchBook {
    batches: HashMap<u64, Batch>,
    by_call: HashMap<String, u64>,
    order: VecDeque<u64>,
    next: u64,
}

impl BatchBook {
    /// Keeps a turn's batch when it holds two edits or more; others cannot form a card.
    pub(super) fn started(&mut self, calls: &[BatchCall<'_>]) {
        let edits = calls.iter().filter(|call| EDIT_TOOLS.contains(&call.name)).count();
        if edits < 2 {
            return;
        }
        self.next += 1;
        let id = self.next;
        let calls: Vec<BatchedCall> = calls
            .iter()
            .filter_map(|call| {
                Some(BatchedCall {
                    name: call.name.to_string(),
                    id: call.function_call_id?.to_string(),
                    args: call.args.clone(),
                    origin: call.origin.cloned(),
                })
            })
            .collect();
        for call in &calls {
            self.by_call.insert(call.id.clone(), id);
        }
        self.batches.insert(id, Batch { calls, card: CardState::Pending });
        self.order.push_back(id);
        while self.order.len() > BATCHES_CAP {
            if let Some(oldest) = self.order.pop_front()
                && let Some(batch) = self.batches.remove(&oldest)
            {
                for call in batch.calls {
                    self.by_call.remove(&call.id);
                }
            }
        }
    }

    /// The decision a shown change set took for `request`, taken once; `None` when the
    /// call is not in a decided card or its arguments differ from the reviewed ones.
    pub(super) fn decided(&mut self, request: &ToolConfirmationRequest) -> Option<ToolApproval> {
        let id = request.function_call_id.as_deref()?;
        let batch = self.batches.get_mut(self.by_call.get(id)?)?;
        let CardState::Decided(decisions) = &mut batch.card else {
            return None;
        };
        let (args, approval) = decisions.remove(id)?;
        (args == request.args).then_some(approval)
    }

    /// The batch of `request` when its card is still to be composed, marking it taken.
    fn pending(&mut self, request: &ToolConfirmationRequest) -> Option<(u64, Vec<BatchedCall>)> {
        let id = request.function_call_id.as_deref()?;
        let batch_id = *self.by_call.get(id)?;
        let batch = self.batches.get_mut(&batch_id)?;
        if !matches!(batch.card, CardState::Pending) {
            return None;
        }
        batch.card = CardState::NotShown;
        Some((batch_id, batch.calls.clone()))
    }

    fn settle(&mut self, batch_id: u64, card: CardState) {
        if let Some(batch) = self.batches.get_mut(&batch_id) {
            batch.card = card;
        }
    }
}

/// How the policy answers one edit of the batch, as the handler's re-check sees it.
enum Membership {
    /// Held, and a rule may cover it: in the card.
    Card,
    /// Allowed by a standing rule: runs without being in the card.
    UnderRule,
    /// Held non-waivably, such as a protected path: asked about on its own.
    Separate,
    /// Allowed by the mode or denied: not shown.
    Outside,
}

/// The arguments a person's edit gave each call, or the batch's own.
fn args_of(calls: &[BatchedCall], edits: &[(String, Value)]) -> Vec<(String, String, Value)> {
    calls
        .iter()
        .map(|call| {
            let args = edits
                .iter()
                .find(|(id, _)| *id == call.id)
                .map_or_else(|| call.args.clone(), |(_, args)| args.clone());
            (call.id.clone(), call.name.clone(), args)
        })
        .collect()
}

impl super::PromptingConfirmationHandler {
    /// Shows the batch of `request` as one change set when it holds two edits or more that
    /// need approval, and answers `request` from the decision (spec 3.1 §7.26). `None`
    /// leaves the call to its own prompt: no batch, too few held edits, a card already
    /// shown, or a prompter that asks one call at a time.
    ///
    /// # Errors
    ///
    /// Returns the prompter's error, a freeze, or the audit trail's error.
    pub(super) async fn change_set(
        &self,
        context: &PromptContext,
        request: &ToolConfirmationRequest,
        policy: &HarnessPolicy,
        prompts: &mut usize,
        deny_all: &mut bool,
    ) -> Result<Option<ToolApproval>> {
        let Some((batch_id, calls)) =
            self.batches.lock().unwrap_or_else(|e| e.into_inner()).pending(request)
        else {
            return Ok(None);
        };
        let workspace = policy.workspace();
        let mut members = Vec::new();
        let mut under_rules = 0;
        let mut separate = 0;
        let mut outside_keys = HashSet::new();
        let keys_of = |call: &BatchedCall| -> Vec<String> {
            crate::edits::edit_paths(&call.name, &call.args)
                .unwrap_or_default()
                .iter()
                .filter_map(|path| workspace.record_key(path).ok())
                .collect()
        };
        for call in calls.iter().filter(|call| EDIT_TOOLS.contains(&call.name.as_str())) {
            match self.membership(context, policy, call).await {
                Membership::Card => members.push(call.clone()),
                Membership::UnderRule => {
                    under_rules += 1;
                    outside_keys.extend(keys_of(call));
                }
                Membership::Separate => {
                    separate += 1;
                    outside_keys.extend(keys_of(call));
                }
                Membership::Outside => outside_keys.extend(keys_of(call)),
            }
        }
        // An edit of a file a call outside the card also changes is asked about on its own:
        // the card's composition could not account for the other call's change.
        let (members, sharing): (Vec<_>, Vec<_>) = members
            .into_iter()
            .partition(|call| keys_of(call).iter().all(|key| !outside_keys.contains(key)));
        separate += sharing.len();
        let not_shown = |this: &Self| {
            this.batches
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .settle(batch_id, CardState::NotShown);
        };
        if members.len() < 2 || *prompts >= self.per_turn_cap {
            not_shown(self);
            return Ok(None);
        }

        let agent = super::RenderedCall::render(request, policy.isolation_status(), 0).agent;
        let mode = policy
            .evaluate_full(
                &context.policy_request(&request.tool_name, &request.args),
                request.origin.as_ref(),
                request.function_call_id.as_deref(),
                false,
            )
            .await
            .mode;
        let mut edits: Vec<(String, Value)> = Vec::new();
        let (change_set, answer) = loop {
            let change_set = ChangeSet::compose(workspace, args_of(&members, &edits)).await;
            let prompt = ChangeSetPrompt::new(
                change_set.clone(),
                mode,
                agent.clone(),
                under_rules,
                separate,
            );
            let rendered = prompt.rendered.clone();
            let answer = tokio::select! {
                answer = self.prompter.prompt_change_set(prompt) => answer?,
                error = frozen(&context.governance) => {
                    tracing::warn!("execution was frozen while a person was deciding a change set; its calls do not run");
                    return Err(error);
                }
            };
            match answer {
                None => {
                    not_shown(self);
                    return Ok(None);
                }
                Some(ChangeSetAnswer::Edit(edited)) => {
                    for (id, args) in edited {
                        edits.retain(|(seen, _)| *seen != id);
                        edits.push((id, args));
                    }
                }
                Some(answer @ (ChangeSetAnswer::ApproveAll | ChangeSetAnswer::ApproveFiles(_)))
                    if !rendered.can_approve() =>
                {
                    let _ = answer;
                    break (
                        change_set,
                        ChangeSetAnswer::DenyAll {
                            message: Some(
                                "the change set was approved without showing its whole diff".into(),
                            ),
                        },
                    );
                }
                Some(answer) => break (change_set, answer),
            }
        };
        *prompts += 1;

        let approved: HashSet<String> = match &answer {
            ChangeSetAnswer::ApproveAll => {
                change_set.files.iter().map(|file| file.path.clone()).collect()
            }
            ChangeSetAnswer::ApproveFiles(files) => files.iter().cloned().collect(),
            ChangeSetAnswer::DenyAll { .. } | ChangeSetAnswer::Edit(_) => HashSet::new(),
        };
        let runs: HashSet<&str> =
            change_set.calls_within(&approved).map(|call| call.function_call_id.as_str()).collect();
        let mut decisions = HashMap::new();
        let mut denied_by_person = false;
        for (call, original) in change_set.calls.iter().zip(&members) {
            let approval = match (&call.failure, runs.contains(call.function_call_id.as_str())) {
                (Some(failure), _) => super::prompt::refuse(format!("not run: {failure}")),
                (None, true) => match call.approved_args() {
                    Some(args) => ToolApproval::approve().with_edited_args(args),
                    None => super::prompt::refuse("not run"),
                },
                (None, false) => {
                    denied_by_person = true;
                    match &answer {
                        ChangeSetAnswer::DenyAll { message: Some(message) } => {
                            super::prompt::refuse(message.clone())
                        }
                        _ => super::prompt::refuse(
                            "the person did not approve this edit in the change set",
                        ),
                    }
                }
            };
            decisions.insert(call.function_call_id.clone(), (original.args.clone(), approval));
        }
        // After a person denies a call, the rest of the turn's calls are denied (I19).
        if denied_by_person {
            *deny_all = true;
        }
        if let Some(audit) = context.audit() {
            let label = match &answer {
                ChangeSetAnswer::ApproveAll => "approve_all",
                ChangeSetAnswer::ApproveFiles(_) => "approve_files",
                ChangeSetAnswer::DenyAll { .. } | ChangeSetAnswer::Edit(_) => "deny_all",
            };
            let files: Vec<Value> = change_set
                .files
                .iter()
                .map(|file| {
                    json!({
                        "path": file.path,
                        "beforeSha256": file.before_sha256,
                        "afterSha256": file.after_sha256,
                        "approved": approved.contains(&file.path),
                    })
                })
                .collect();
            let call_ids: Vec<&str> =
                change_set.calls.iter().map(|call| call.function_call_id.as_str()).collect();
            let mut approved_ids: Vec<&str> = runs.iter().copied().collect();
            approved_ids.sort_unstable();
            audit
                .record(
                    "harness.changeset.decided",
                    json!({
                        "digest": change_set.digest,
                        "files": files,
                        "callIds": call_ids,
                        "approvedCallIds": approved_ids,
                        "answer": label,
                        "edited": !edits.is_empty(),
                        "underRules": under_rules,
                        "askedSeparately": separate,
                    }),
                )
                .await?;
        }
        let mine = request.function_call_id.as_deref().and_then(|id| decisions.remove(id));
        self.batches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .settle(batch_id, CardState::Decided(decisions));
        Ok(mine.map(|(_, approval)| approval))
    }

    /// How the run's policy answers `call` now, as the handler's re-check sees it.
    async fn membership(
        &self,
        context: &PromptContext,
        policy: &HarnessPolicy,
        call: &BatchedCall,
    ) -> Membership {
        let request = context.policy_request(&call.name, &call.args);
        let evaluated =
            policy.evaluate_full(&request, call.origin.as_ref(), Some(&call.id), false).await;
        let peers = match &context.peers {
            Some(peers) => {
                let mut policy_call = PolicyCall::new().with_function_call_id(&call.id);
                if let Some(origin) = call.origin.as_ref() {
                    policy_call = policy_call.with_origin(origin);
                }
                Some(peers.evaluate_call(&request, &policy_call).await)
            }
            None => None,
        };
        let composed = match peers {
            Some(peers) => evaluated.decision.clone().stricter(peers),
            None => evaluated.decision.clone(),
        };
        match composed {
            PolicyDecision::RequireApproval { .. } if evaluated.base.is_non_waivable() => {
                Membership::Separate
            }
            PolicyDecision::RequireApproval { .. } => Membership::Card,
            PolicyDecision::Allow if evaluated.rule.is_some() => Membership::UnderRule,
            PolicyDecision::Allow | PolicyDecision::Deny { .. } => Membership::Outside,
        }
    }
}

/// Never resolves when the run has no kill switch; otherwise the freeze error once one
/// stops execution (I20).
pub(super) async fn frozen(governance: &[adk_core::GovernanceControl]) -> AdkError {
    if governance.is_empty() {
        return std::future::pending::<AdkError>().await;
    }
    loop {
        if let Some(error) = governance.iter().find_map(|control| control.check().err()) {
            return error;
        }
        tokio::time::sleep(crate::tools::GOVERNANCE_POLL_INTERVAL).await;
    }
}
