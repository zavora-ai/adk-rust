//! An editor's "Always allow" over ACP as a standing rule (spec 3.3 §10 A1).

use adk_acp::server::{StandingOffer, StandingPermissions};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalScope, ErrorCategory, ErrorComponent, PolicyCall, PolicyDecision, Result,
    ToolConfirmationDecision, ToolConfirmationRequest, async_trait,
};

use crate::{
    HarnessAgent, RuleOwner, RulePredicate, RuleSuggestion, StandingDecision, StandingRule,
    SuggestedScope,
};

/// The narrowest rule offered for `request`: a file write's folder rule, a command's
/// two-token prefix, a site, or the tool from its exact publisher. A file change no folder
/// rule covers — a delete or a move — gets none.
fn narrowest(harness: &HarnessAgent, request: &ToolConfirmationRequest) -> Option<RuleSuggestion> {
    if let Some(folder) = harness.folder_suggestion(request) {
        return Some(folder);
    }
    let reach = |suggestion: &RuleSuggestion| -> usize {
        suggestion
            .predicates
            .iter()
            .map(|predicate| match predicate {
                RulePredicate::ShellPrefix { tokens, .. } => tokens.len(),
                RulePredicate::Arg(_) => 1,
                RulePredicate::PathWithin { .. } | RulePredicate::WritesWithin { .. } => 0,
            })
            .sum()
    };
    StandingRule::suggestions(request)
        .into_iter()
        .filter(|suggestion| {
            !suggestion
                .predicates
                .iter()
                .any(|predicate| matches!(predicate, RulePredicate::PathWithin { .. }))
        })
        .max_by_key(reach)
}

/// What a rule from `suggestion` covers, completing "Always allow …" (spec 3.3 §8.2): where it
/// applies, and — as at the terminal prompt — that a rule over commands that run project
/// scripts allows whatever they do (spec 3.1 §7.5).
fn covers(suggestion: &RuleSuggestion) -> StandingOffer {
    let what = match suggestion.predicates.first() {
        Some(RulePredicate::ShellPrefix { .. }) => "for this command",
        Some(RulePredicate::PathWithin { dir, .. } | RulePredicate::WritesWithin { dir })
            if dir == "." =>
        {
            "for this workspace"
        }
        Some(RulePredicate::PathWithin { .. } | RulePredicate::WritesWithin { .. }) => {
            "for this folder"
        }
        Some(RulePredicate::Arg(_)) => "for this site",
        None => "for this person",
    };
    // A label states what the rule matches, never its scope (spec 3.1 §7.33 fix 1).
    let scope = match suggestion.scope {
        SuggestedScope::User => ", every session",
        SuggestedScope::Session => ", this session",
    };
    let offer = StandingOffer::new(format!("{what} ({}{scope})", suggestion.label));
    if suggestion.runs_scripts {
        offer.with_allow_warning("runs project scripts, so it allows whatever they do")
    } else {
        offer
    }
}

/// Whether the harness's standing rules apply to an ACP run. ACP runs carry no authenticated
/// identity, so under `require_authenticated_identity` no rule applies to them (spec 3.1
/// §9.2 I6), and an "always" answer would be stored but never used there.
fn rules_apply(harness: &HarnessAgent) -> bool {
    #[cfg(feature = "auth")]
    {
        !harness.runtime.identity.requires_authentication()
    }
    #[cfg(not(feature = "auth"))]
    {
        let _ = harness;
        true
    }
}

fn refused(message: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::InvalidInput,
        "harness.decision.invalid",
        message,
    )
}

/// The person `scope` names, as the rule's owner and creator.
fn owner(scope: &ApprovalScope, request: &ToolConfirmationRequest) -> RuleOwner {
    let created_by = request
        .acting
        .as_ref()
        .and_then(|acting| acting.on_behalf_of.clone())
        .unwrap_or_else(|| SubjectRef::human(scope.user_id.clone()));
    RuleOwner {
        app_name: scope.app_name.clone(),
        user_id: scope.user_id.clone(),
        subject_kind: created_by.kind,
        session_id: scope.session_id.clone(),
        created_by,
    }
}

/// Offers "always" for the calls a standing rule may cover and remembers the answer as the
/// narrowest such rule: bound to the tool's exact declaration and the session's person, and
/// recorded as `harness.rule.created` with surface `acp` (spec 3.1 §7.5, spec 3.3 §10 A1).
///
/// A call no rule covers — spending money, leaving plan mode, a memory write, a desktop
/// action, a protected edit, an unsandboxed retry — is offered allow once and deny only, and
/// so is every call of a harness built with `require_authenticated_identity`, whose rules do
/// not apply to unauthenticated ACP runs. A call a rule already covers is decided by it. A
/// call another policy layer or the agent's confirmation policy holds is still asked about
/// after its rule allows it, because a standing rule answers only the harness's approval.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::server::{AcpServer, AcpServerConfigBuilder};
///
/// let harness = Arc::new(HarnessAgent::builder("harness").model(model).workspace(workspace).build()?);
/// let config = AcpServerConfigBuilder::new()
///     .agent(harness.clone())
///     .session_service(sessions)
///     .standing_permissions(harness)
///     .build()?;
/// let server = AcpServer::run(config).await?;
/// ```
#[async_trait]
impl StandingPermissions for HarnessAgent {
    async fn offer(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> Option<StandingOffer> {
        if !rules_apply(self) {
            return None;
        }
        let suggestion = narrowest(self, request)?;
        self.rule_for_held_call(
            request,
            suggestion.clone(),
            StandingDecision::Allow,
            &owner(scope, request),
        )
        .ok()?;
        Some(covers(&suggestion))
    }

    async fn standing_decision(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> Option<ToolConfirmationDecision> {
        // The context the run's policy recorded when it held this call; without it, or for
        // another session's call of the same ID, the person decides.
        let context =
            self.runtime.policy.prompts().by_call_id(request.function_call_id.as_deref()?)?;
        if (context.app_name.as_str(), context.user_id.as_str(), context.session_id.as_str())
            != (scope.app_name.as_str(), scope.user_id.as_str(), scope.session_id.as_str())
        {
            return None;
        }
        // Re-checked as the terminal prompt re-checks a call of a batch (spec 3.1 §7.5): a
        // rule remembered for an earlier call answers the harness's own approval, never
        // another layer's, and never a non-waivable one.
        let policy = self.runtime.policy.with_audit(context.audit.clone());
        let policy_request = context.policy_request(&request.tool_name, &request.args);
        let evaluated = policy
            .evaluate_full(
                &policy_request,
                request.origin.as_ref(),
                request.function_call_id.as_deref(),
                false,
            )
            .await;
        let composed = match &context.peers {
            Some(peers) => {
                let mut call = PolicyCall::new();
                if let Some(origin) = request.origin.as_ref() {
                    call = call.with_origin(origin);
                }
                if let Some(call_id) = request.function_call_id.as_deref() {
                    call = call.with_function_call_id(call_id);
                }
                evaluated.decision.stricter(peers.evaluate_call(&policy_request, &call).await)
            }
            None => evaluated.decision,
        };
        match composed {
            PolicyDecision::Allow if context.harness_held => {
                Some(ToolConfirmationDecision::Approve)
            }
            PolicyDecision::Deny { .. } => Some(ToolConfirmationDecision::Deny),
            // Held by another layer or the agent's confirmation policy, or still held by the
            // harness: the person decides.
            PolicyDecision::Allow | PolicyDecision::RequireApproval { .. } => None,
        }
    }

    async fn remember(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
        decision: ToolConfirmationDecision,
    ) -> Result<()> {
        if !rules_apply(self) {
            return Err(refused(
                "standing rules do not apply to unauthenticated ACP runs of this harness; \
                 decide the call once",
            ));
        }
        let suggestion = narrowest(self, request)
            .ok_or_else(|| refused("no standing rule applies to this call; decide it once"))?;
        let decision = match decision {
            ToolConfirmationDecision::Approve => StandingDecision::Allow,
            ToolConfirmationDecision::Deny => {
                StandingDecision::Deny { reason: format!("denied by {}", scope.user_id) }
            }
        };
        let rule =
            self.rule_for_held_call(request, suggestion, decision, &owner(scope, request))?;
        self.grant_rule(rule, "acp", Some(request)).await?;
        Ok(())
    }
}
