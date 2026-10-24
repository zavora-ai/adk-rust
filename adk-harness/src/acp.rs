//! An editor's "Always allow" over ACP as a standing rule (spec 3.3 §10 A1).

use adk_acp::server::{StandingOffer, StandingPermissions};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalScope, ErrorCategory, ErrorComponent, Result, ToolConfirmationDecision,
    ToolConfirmationRequest, async_trait,
};

use crate::{
    HarnessAgent, RuleOwner, RulePredicate, RuleSuggestion, StandingDecision, StandingRule,
};

/// The narrowest rule offered for `request`: a command's two-token prefix, a file's own
/// folder, a site, or the tool from its exact publisher.
fn narrowest(request: &ToolConfirmationRequest) -> Option<RuleSuggestion> {
    let reach = |suggestion: &RuleSuggestion| -> usize {
        suggestion
            .predicates
            .iter()
            .map(|predicate| match predicate {
                RulePredicate::ShellPrefix { tokens, .. } => tokens.len(),
                RulePredicate::PathWithin { dir, .. } if dir == "." => 1,
                RulePredicate::PathWithin { dir, .. } => 2 + dir.matches('/').count(),
                RulePredicate::Arg(_) => 1,
            })
            .sum()
    };
    StandingRule::suggestions(request).into_iter().max_by_key(reach)
}

/// What a rule from `suggestion` covers, completing "Always allow …" (spec 3.3 §8.2).
fn covers(suggestion: &RuleSuggestion) -> String {
    let what = match suggestion.predicates.first() {
        Some(RulePredicate::ShellPrefix { .. }) => "for this command",
        Some(RulePredicate::PathWithin { dir, .. }) if dir == "." => "for this workspace",
        Some(RulePredicate::PathWithin { .. }) => "for this folder",
        Some(RulePredicate::Arg(_)) => "for this site",
        None => "for this person",
    };
    format!("{what} ({})", suggestion.label)
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
/// action, a protected edit, an unsandboxed retry — is offered allow once and deny only.
/// A call another policy layer or the agent's confirmation policy holds is still asked about
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
        let suggestion = narrowest(request)?;
        self.rule_for_held_call(
            request,
            suggestion.clone(),
            StandingDecision::Allow,
            &owner(scope, request),
        )
        .ok()?;
        Some(StandingOffer::new(covers(&suggestion)))
    }

    async fn remember(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
        decision: ToolConfirmationDecision,
    ) -> Result<()> {
        let suggestion = narrowest(request).ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                "harness.decision.invalid",
                "no standing rule applies to this call; decide it once",
            )
        })?;
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
