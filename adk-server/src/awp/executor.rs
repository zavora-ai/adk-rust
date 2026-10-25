//! The governed path every binding enters (spec 3.5 §7.4, D-4, A-I7, A-I8; AWP 2.0 §7.5).
//!
//! [`GovernedExecutor`] answers a capability call the same way whether it arrived over HTTP,
//! A2A, MCP or a public agent's conversation. In order:
//!
//! 1. Visibility by tier and deprecation — a hidden or sunset capability is `capability_not_found`.
//! 2. Read-only mode and the kill switch — a non-`read_only` capability answers `read_only`.
//! 3. The caller's daily call limit — `rate_limit_exceeded`.
//! 4. The input schema, the idempotency key a `non_idempotent` capability requires, and a live
//!    consent receipt for a consent-bound one.
//! 5. The idempotency record: a repeat with the same key and input returns the recorded
//!    outcome — output, error or held call — without executing; the same key with another
//!    input is `idempotency_conflict`.
//! 6. `adk_core::authorize_tool_call` under [`CapabilityPolicy`], the owner's
//!    `ApprovalService`, the action ledger and the audit trail: the call executes, is refused,
//!    or is held for the owner (`202` with `Location: /awp/v2/calls/{id}`).
//!
//! A held call waits in the approval store for as long as the owner takes — spec 3.3's park on
//! timeout — until its decision arrives; [`GovernedExecutor::resolve`] then runs it through the
//! same path, or ends it `rejected` or `expired`.

use std::sync::Arc;

use adk_awp::v2::capability::{CapabilityRegistry, CapabilityTool, adk_error_to_body};
use adk_awp::v2::executor::{
    Binding, CapabilityExecutor, EdgeToolContext, Invocation, InvocationOutcome,
};
use adk_awp::v2::store::{
    ConsentReceiptStore, HeldCallRecord, HeldCallStore, HeldCallTransition, IdempotencyBegin,
    IdempotencyStore, RecordedOutcome, StoreError, has_live_receipt,
};
use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::{
    ActingContext, ActionLedger, ActionOutcome, ActionRecord, AgentPrincipal, ApprovalScope,
    ApprovalService, ApprovalState, ApprovalStore, GovernedCall, InboxQuery, RunConfig, Tool,
    ToolAuthorization, ToolConfirmationRequest, ToolEffect, ToolGate, ToolOrigin, json_digest,
    not_run_text,
};
use async_trait::async_trait;
use awp_types::{
    CapabilityEffect, ErrorBody, ErrorCode, HeldCall, HeldCallState, IdempotencySource,
    RequesterId, TrustTier,
};
use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use super::control::EdgeControl;
use super::limits::EdgeLimits;
use super::policy::{ATTR_CONSENT, ATTR_TIER, CapabilityPolicy};
use super::tiers::EdgeCaller;

/// The name the edge's governed calls are recorded under.
pub const EDGE_AGENT_NAME: &str = "awp-edge";

tokio::task_local! {
    /// The caller of the request being served, for executors called without one.
    pub(crate) static CURRENT_CALLER: EdgeCaller;
}

/// Where a binding found the idempotency key of a call (AWP 2.0 §7.7, C7, C8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeySources {
    /// The explicit key: `Idempotency-Key`, the A2A metadata key, or the MCP `_meta` key.
    pub explicit: Option<String>,
    /// The binding's own id: the A2A `messageId`, or the MCP request id within its session.
    pub binding_id: Option<String>,
}

impl KeySources {
    /// The key `source` selects for a call over `binding` (AWP 2.0 §7.7): `idempotency_key`
    /// takes only the explicit key; every other source takes the binding's own source —
    /// `Idempotency-Key` on HTTP, the `messageId` on A2A, the `_meta` key else the request id
    /// on MCP.
    pub fn key_for(&self, source: IdempotencySource, binding: Binding) -> Option<String> {
        match (source, binding) {
            (IdempotencySource::IdempotencyKey, _) | (_, Binding::Http) => self.explicit.clone(),
            (_, Binding::A2a) => self.binding_id.clone().or_else(|| self.explicit.clone()),
            (_, Binding::Mcp) => self.explicit.clone().or_else(|| self.binding_id.clone()),
            _ => self.explicit.clone(),
        }
    }
}

/// The governed [`CapabilityExecutor`] of the edge (spec 3.5 §7.4).
///
/// Build it through [`AwpEdge`](super::AwpEdge); every binding shares one.
#[derive(Clone)]
pub struct GovernedExecutor {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for GovernedExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernedExecutor")
            .field("domain", &self.inner.registry.domain())
            .finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub(crate) registry: Arc<CapabilityRegistry>,
    pub(crate) held_calls: Arc<dyn HeldCallStore>,
    pub(crate) idempotency: Arc<dyn IdempotencyStore>,
    pub(crate) consent: Arc<dyn ConsentReceiptStore>,
    pub(crate) approval_service: Option<Arc<dyn ApprovalService>>,
    pub(crate) approval_store: Arc<dyn ApprovalStore>,
    pub(crate) ledger: Arc<dyn ActionLedger>,
    pub(crate) audit: Option<AuditTrail>,
    pub(crate) principal: AgentPrincipal,
    pub(crate) policy: Arc<CapabilityPolicy>,
    pub(crate) control: Arc<EdgeControl>,
    pub(crate) limits: Arc<EdgeLimits>,
    pub(crate) hold_expiry: Option<chrono::Duration>,
}

fn refused(code: ErrorCode, message: impl Into<String>) -> InvocationOutcome {
    InvocationOutcome::Failed(ErrorBody::new(code, message))
}

fn store_failed(error: &StoreError) -> InvocationOutcome {
    tracing::error!(error = %error, "awp edge store failed during a capability call");
    refused(ErrorCode::InternalError, "the site could not record the call")
}

fn binding_name(binding: Binding) -> &'static str {
    match binding {
        Binding::Http => "http",
        Binding::A2a => "a2a",
        Binding::Mcp => "mcp",
        _ => "other",
    }
}

fn outcome_name(outcome: &InvocationOutcome) -> String {
    match outcome {
        InvocationOutcome::Completed(_) => "completed".to_owned(),
        InvocationOutcome::Held(_) => "held".to_owned(),
        InvocationOutcome::Failed(error) => error.code.to_string(),
        _ => "other".to_owned(),
    }
}

impl GovernedExecutor {
    pub(crate) fn from_inner(inner: Inner) -> Self {
        Self { inner: Arc::new(inner) }
    }

    /// The capability registry.
    pub fn registry(&self) -> &Arc<CapabilityRegistry> {
        &self.inner.registry
    }

    /// The scope of an idempotency key: the capability and, from `known` up, the requester.
    fn idempotency_scope(invocation: &Invocation, key: &str) -> String {
        let requester = invocation.requester.as_ref().map_or("", RequesterId::as_str);
        format!("{}\u{1f}{requester}\u{1f}{key}", invocation.capability)
    }

    fn acting(
        &self,
        tier: TrustTier,
        requester: Option<&RequesterId>,
        consent: &[Uuid],
    ) -> ActingContext {
        let consent = consent.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
        let principal = self
            .inner
            .principal
            .clone()
            .with_attribute(ATTR_TIER, tier.as_str())
            .with_attribute(ATTR_CONSENT, consent);
        let acting = ActingContext::new(principal);
        match requester {
            // The verified requester is the call's counterparty (spec 3.5 §7.3).
            Some(requester) => acting.on_behalf_of(SubjectRef::service(requester.as_str())),
            None => acting,
        }
    }

    fn scope(&self, requester: Option<&RequesterId>, call_id: &str) -> ApprovalScope {
        ApprovalScope::new(
            CapabilityTool::origin_for(self.inner.registry.domain()),
            requester.map_or("public", RequesterId::as_str),
            call_id,
        )
    }

    fn run_config(&self, effect: CapabilityEffect) -> RunConfig {
        let inner = &self.inner;
        let mut builder = RunConfig::builder()
            .tool_policy(Arc::clone(&inner.policy) as Arc<dyn adk_core::ToolPolicy>)
            .action_ledger(Arc::clone(&inner.ledger))
            .approval_store(Arc::clone(&inner.approval_store));
        if let Some(service) = &inner.approval_service {
            builder = builder.approval_service(Arc::clone(service));
        }
        if let Some(audit) = &inner.audit {
            builder = builder.audit(audit.clone());
        }
        // Read-only capabilities keep answering under the kill switch (A-I12).
        if effect != CapabilityEffect::ReadOnly
            && let Some(control) = inner.control.governance()
        {
            builder = builder.governance(control.clone());
        }
        builder.build()
    }

    async fn audit(
        &self,
        action: &str,
        acting: &ActingContext,
        call_id: &str,
        invocation: &Invocation,
        outcome: &str,
    ) {
        let Some(trail) = &self.inner.audit else { return };
        let draft = AuditRecordDraft::new(AuditRecordKind::Other("awp".to_owned()), action)
            .actor(AuditActor::new(acting, call_id))
            .args_digest(trail.digest(&invocation.input))
            .ext("awp.capability", Value::String(invocation.capability.clone()))
            .ext("awp.binding", Value::String(binding_name(invocation.binding).to_owned()))
            .ext("awp.tier", Value::String(invocation.tier.as_str().to_owned()))
            .ext(
                "awp.requester",
                invocation
                    .requester
                    .as_ref()
                    .map_or(Value::Null, |requester| Value::String(requester.to_string())),
            )
            .ext("outcome", Value::String(outcome.to_owned()));
        if let Err(error) = trail.record(draft).await {
            tracing::warn!(audit.action = action, error = %error, "awp audit record not written");
        }
    }

    /// Executes an authorized call, ledgered when it is `non_idempotent` (A-I7).
    async fn execute(
        &self,
        tool: &Arc<CapabilityTool>,
        invocation: &Invocation,
        args: Value,
        call_id: &str,
    ) -> InvocationOutcome {
        let ledger = &self.inner.ledger;
        let effect = adk_core::Tool::effect(tool.as_ref());
        let ledger_key = format!(
            "awp\u{1f}{}",
            Self::idempotency_scope(
                invocation,
                invocation.idempotency_key.as_deref().unwrap_or(call_id)
            )
        );
        if effect == ToolEffect::NonIdempotent {
            match ledger.get(&ledger_key).await {
                Ok(None) => {}
                Ok(Some(record)) => {
                    let mut body = ErrorBody::new(
                        ErrorCode::IdempotencyConflict,
                        if record.is_completed() {
                            "this call already executed; its result is no longer retained"
                        } else {
                            "a call with this idempotency key is still executing"
                        },
                    );
                    body.retryable = !record.is_completed();
                    return InvocationOutcome::Failed(body);
                }
                Err(error) => {
                    tracing::error!(error = %error, "awp action ledger read failed");
                    return refused(ErrorCode::InternalError, "the site could not record the call");
                }
            }
            let record =
                ActionRecord::new(&ledger_key, tool.declaration().name.clone(), &args, effect);
            if let Err(error) = ledger.begin(&record).await {
                tracing::error!(error = %error, "awp action ledger begin failed");
                return refused(ErrorCode::InternalError, "the site could not record the call");
            }
            // The kill switch may engage between the hold and the execution.
            if let Some(control) = self.inner.control.governance()
                && let Err(frozen) = control.check()
            {
                let _ = ledger
                    .complete(&ledger_key, ActionOutcome::Failed { error: not_run_text(&frozen) })
                    .await;
                return refused(ErrorCode::ReadOnly, "the site is read-only");
            }
        }
        let context = Arc::new(EdgeToolContext::new(tool, invocation, call_id));
        let result = Arc::clone(tool).execute(context, args).await;
        if effect == ToolEffect::NonIdempotent {
            let outcome = match &result {
                Ok(output) => ActionOutcome::Succeeded { result_digest: json_digest(output) },
                Err(error) => ActionOutcome::Failed { error: error.message.clone() },
            };
            if let Err(error) = ledger.complete(&ledger_key, outcome).await {
                tracing::error!(error = %error, "awp action ledger complete failed");
            }
        }
        match result {
            Ok(output) => InvocationOutcome::Completed(output),
            Err(error) => InvocationOutcome::Failed(adk_error_to_body(&error)),
        }
    }

    async fn record(&self, scope: Option<&str>, outcome: &InvocationOutcome) {
        let Some(scope) = scope else { return };
        let recorded = match outcome {
            InvocationOutcome::Completed(output) => {
                RecordedOutcome::Completed { output: output.clone() }
            }
            InvocationOutcome::Held(call) => RecordedOutcome::Held { call_id: call.id.clone() },
            InvocationOutcome::Failed(error) => RecordedOutcome::Failed { error: error.clone() },
            _ => return,
        };
        if let Err(error) = self.inner.idempotency.complete(scope, recorded).await {
            tracing::error!(error = %error, "awp idempotency outcome was not recorded");
        }
    }

    async fn replay(&self, outcome: RecordedOutcome) -> InvocationOutcome {
        match outcome {
            RecordedOutcome::Completed { output } => InvocationOutcome::Completed(output),
            RecordedOutcome::Failed { error } => InvocationOutcome::Failed(error),
            RecordedOutcome::Held { call_id } => match self.resolve(&call_id).await {
                Ok(Some(call)) => InvocationOutcome::Held(call),
                Ok(None) => refused(ErrorCode::InternalError, "the held call was not found"),
                Err(error) => store_failed(&error),
            },
            _ => refused(ErrorCode::InternalError, "the recorded outcome could not be read"),
        }
    }

    /// Executes, holds or refuses `invocation` for `caller` (spec 3.5 §7.4).
    pub async fn invoke_as(
        &self,
        caller: &EdgeCaller,
        invocation: Invocation,
    ) -> InvocationOutcome {
        let span = tracing::info_span!(
            "awp.capability.call",
            awp.capability = %invocation.capability,
            awp.binding = binding_name(invocation.binding),
            awp.tier = %invocation.tier,
            awp.requester = invocation.requester.as_ref().map_or("", RequesterId::as_str),
            awp.outcome = tracing::field::Empty,
        );
        let outcome =
            tracing::Instrument::instrument(self.invoke_inner(caller, invocation), span.clone())
                .await;
        span.record("awp.outcome", outcome_name(&outcome).as_str());
        outcome
    }

    async fn invoke_inner(&self, caller: &EdgeCaller, invocation: Invocation) -> InvocationOutcome {
        let inner = &self.inner;
        let now = Utc::now();
        let Some(tool) = inner.registry.visible(&invocation.capability, invocation.tier) else {
            return refused(ErrorCode::CapabilityNotFound, "no such capability");
        };
        let declaration = tool.declaration();
        if declaration
            .deprecated
            .as_ref()
            .and_then(|deprecated| deprecated.sunset)
            .is_some_and(|sunset| sunset <= now)
        {
            return refused(ErrorCode::CapabilityNotFound, "no such capability");
        }
        if declaration.effect != CapabilityEffect::ReadOnly && inner.control.is_read_only() {
            let mut body = ErrorBody::new(
                ErrorCode::ReadOnly,
                "the site is read-only; only read-only capabilities answer",
            );
            body.retryable = true;
            return InvocationOutcome::Failed(body);
        }
        if let Err(limited) = inner.limits.check_call(caller, declaration.rate_class.as_deref()) {
            let mut body = ErrorBody::new(
                ErrorCode::RateLimitExceeded,
                format!("the caller passed its daily call limit of {}", limited.limit),
            );
            body.retry_after = Some(limited.retry_after);
            return InvocationOutcome::Failed(body);
        }
        if let Err(errors) = tool.validate_input(&invocation.input) {
            return refused(
                ErrorCode::SchemaValidationFailed,
                format!("the input does not match the input schema: {}", errors.join("; ")),
            );
        }
        if declaration.requires_idempotency() && invocation.idempotency_key.is_none() {
            return refused(
                ErrorCode::IdempotencyKeyRequired,
                "this capability needs an idempotency key on every call",
            );
        }
        if let Some(requirement) = &declaration.consent {
            match has_live_receipt(inner.consent.as_ref(), &invocation.consent, requirement, now)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    return refused(
                        ErrorCode::ConsentRequired,
                        format!(
                            "this capability needs a live consent receipt for {} under policy {}",
                            requirement.scope, requirement.policy_version
                        ),
                    );
                }
                Err(error) => return store_failed(&error),
            }
        }

        let scope = invocation
            .idempotency_key
            .as_deref()
            .map(|key| Self::idempotency_scope(&invocation, key));
        if let Some(scope) = &scope {
            match inner.idempotency.begin(scope, &json_digest(&invocation.input), now).await {
                Ok(IdempotencyBegin::Started) => {}
                Ok(IdempotencyBegin::Recorded(outcome)) => {
                    tracing::debug!("awp call answered from its idempotency record");
                    return self.replay(outcome).await;
                }
                Ok(IdempotencyBegin::InFlight) => {
                    let mut body = ErrorBody::new(
                        ErrorCode::IdempotencyConflict,
                        "a call with this idempotency key is still executing",
                    );
                    body.retryable = true;
                    body.retry_after = Some(1);
                    return InvocationOutcome::Failed(body);
                }
                Ok(IdempotencyBegin::Conflict) => {
                    return refused(
                        ErrorCode::IdempotencyConflict,
                        "the idempotency key was used with a different input",
                    );
                }
                Ok(_) => {
                    return refused(
                        ErrorCode::InternalError,
                        "the idempotency record is unreadable",
                    );
                }
                Err(error) => return store_failed(&error),
            }
        }

        // At least 128 bits of entropy (AWP 2.0 §7.6).
        let call_id = format!("call_{}", Uuid::new_v4().simple());
        let outcome = self.govern(tool, &invocation, &call_id).await;
        self.record(scope.as_deref(), &outcome).await;
        outcome
    }

    /// Runs the call through `adk_core::authorize_tool_call` and executes, refuses or holds it.
    async fn govern(
        &self,
        tool: &Arc<CapabilityTool>,
        invocation: &Invocation,
        call_id: &str,
    ) -> InvocationOutcome {
        let inner = &self.inner;
        let declaration = tool.declaration();
        let run_config = self.run_config(declaration.effect);
        let acting =
            self.acting(invocation.tier, invocation.requester.as_ref(), &invocation.consent);
        let gate = ToolGate::new(
            &run_config,
            EDGE_AGENT_NAME,
            call_id,
            self.scope(invocation.requester.as_ref(), call_id),
        )
        .with_acting_context(acting.clone());
        let call = GovernedCall::for_tool(tool.as_ref(), call_id, invocation.input.clone());
        let outcome = match adk_core::authorize_tool_call(&gate, call).await {
            Ok(ToolAuthorization::Execute { args, .. }) => {
                self.execute(tool, invocation, args, call_id).await
            }
            Ok(ToolAuthorization::Refuse { reason, .. }) => {
                tracing::info!(reason = %reason, "awp capability call refused by the governed path");
                refused(ErrorCode::Forbidden, "the owner's policy does not allow this call")
            }
            Ok(ToolAuthorization::Pending { .. }) => {
                let now = Utc::now();
                let mut held = HeldCall::pending(call_id, declaration.name.clone(), now);
                held.expires_at = inner.hold_expiry.map(|expiry| now + expiry);
                let record = HeldCallRecord {
                    call: held,
                    requester: invocation.requester.clone(),
                    tier: invocation.tier,
                    input: invocation.input.clone(),
                    idempotency_key: invocation.idempotency_key.clone(),
                    consent: invocation.consent.clone(),
                };
                match inner.held_calls.create(record.clone()).await {
                    Ok(()) => {
                        self.audit("awp.capability.held", &acting, call_id, invocation, "held")
                            .await;
                        InvocationOutcome::Held(record.call)
                    }
                    Err(error) => store_failed(&error),
                }
            }
            Err(error) if inner.control.frozen() => {
                tracing::info!(error = %error, "awp capability call stopped by the kill switch");
                let mut body = ErrorBody::new(ErrorCode::ReadOnly, "the site is read-only");
                body.retryable = true;
                InvocationOutcome::Failed(body)
            }
            Err(error) => {
                tracing::error!(error = %error, "awp governed path failed");
                refused(ErrorCode::InternalError, "the call could not be authorized")
            }
        };
        if !matches!(outcome, InvocationOutcome::Held(_)) {
            self.audit(
                "awp.capability.called",
                &acting,
                call_id,
                invocation,
                &outcome_name(&outcome),
            )
            .await;
        }
        outcome
    }

    async fn transition(
        &self,
        id: &str,
        to: HeldCallState,
        result: Option<Value>,
        error: Option<ErrorBody>,
    ) -> Result<HeldCall, StoreError> {
        Ok(self
            .inner
            .held_calls
            .transition(id, HeldCallTransition { to, result, error })
            .await?
            .call)
    }

    /// Whether the owner's approval request for `fingerprint` in `scope` expired.
    async fn approval_expired(&self, scope: &ApprovalScope, fingerprint: &str) -> bool {
        let Some(service) = &self.inner.approval_service else { return false };
        let query = InboxQuery::default().with_session(scope).with_states(ApprovalState::ALL);
        match service.inbox(query).await {
            Ok(page) => page.items.iter().any(|item| {
                item.fingerprint == fingerprint && item.state == ApprovalState::Expired
            }),
            Err(_) => false,
        }
    }

    /// Resolves held call `id` once the owner decided it (spec 3.5 §7.4): an approved call runs
    /// through the governed path again and completes with its output or error; a refused call
    /// ends `rejected`, or `expired` when its approval request expired; a call past its
    /// `expiresAt` expires. A call with no decision, or one decided while the kill switch is
    /// engaged, stays pending. Returns the held call, or `None` for an unknown id.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the held-call store fails.
    pub async fn resolve(&self, id: &str) -> Result<Option<HeldCall>, StoreError> {
        let inner = &self.inner;
        let Some(record) = inner.held_calls.get(id).await? else { return Ok(None) };
        if record.call.state != HeldCallState::Pending {
            return Ok(Some(record.call));
        }
        let now = Utc::now();
        if record.call.expires_at.is_some_and(|expires_at| expires_at <= now) {
            let error = ErrorBody::new(ErrorCode::Forbidden, "the owner did not decide in time");
            return self.transition(id, HeldCallState::Expired, None, Some(error)).await.map(Some);
        }
        if inner.control.frozen() {
            return Ok(Some(record.call));
        }
        let Some(tool) = inner.registry.get(&record.call.capability) else {
            let error =
                ErrorBody::new(ErrorCode::CapabilityNotFound, "the capability was withdrawn");
            return self.transition(id, HeldCallState::Rejected, None, Some(error)).await.map(Some);
        };
        let mut invocation =
            Invocation::new(record.call.capability.clone(), record.input.clone(), Binding::Http);
        invocation.tier = record.tier;
        invocation.requester = record.requester.clone();
        invocation.consent = record.consent.clone();
        invocation.idempotency_key = record.idempotency_key.clone();
        let run_config = self.run_config(tool.declaration().effect);
        let acting = self.acting(record.tier, record.requester.as_ref(), &record.consent);
        let scope = self.scope(record.requester.as_ref(), id);
        let gate = ToolGate::new(&run_config, EDGE_AGENT_NAME, id, scope.clone())
            .with_acting_context(acting.clone());
        let request = ToolConfirmationRequest {
            tool_name: tool.declaration().name.clone(),
            function_call_id: Some(id.to_owned()),
            args: record.input.clone(),
            acting: Some(Box::new(acting.clone())),
            origin: Some(ToolOrigin::of(tool.as_ref() as &dyn Tool)),
        };
        let fingerprint = request.fingerprint();
        match adk_core::confirmation_decision_available(&gate, &request).await {
            Ok(true) => {}
            Ok(false) => return Ok(Some(record.call)),
            Err(error) => {
                tracing::warn!(error = %error, "awp held call decision could not be read");
                return Ok(Some(record.call));
            }
        }
        let call =
            GovernedCall::for_tool(tool.as_ref(), id, record.input.clone()).resuming(request);
        let resolved = match adk_core::authorize_tool_call(&gate, call).await {
            Ok(ToolAuthorization::Execute { args, .. }) => {
                self.transition(id, HeldCallState::Approved, None, None).await?;
                match self.execute(tool, &invocation, args, id).await {
                    InvocationOutcome::Completed(output) => {
                        self.transition(id, HeldCallState::Completed, Some(output), None).await?
                    }
                    InvocationOutcome::Failed(error) => {
                        self.transition(id, HeldCallState::Completed, None, Some(error)).await?
                    }
                    _ => {
                        let error =
                            ErrorBody::new(ErrorCode::InternalError, "the call did not complete");
                        self.transition(id, HeldCallState::Completed, None, Some(error)).await?
                    }
                }
            }
            Ok(ToolAuthorization::Refuse { reason, .. }) => {
                tracing::info!(reason = %reason, "awp held call refused");
                if self.approval_expired(&scope, &fingerprint).await {
                    let error =
                        ErrorBody::new(ErrorCode::Forbidden, "the owner did not decide in time");
                    self.transition(id, HeldCallState::Expired, None, Some(error)).await?
                } else {
                    let error = ErrorBody::new(ErrorCode::Forbidden, "the owner declined the call");
                    self.transition(id, HeldCallState::Rejected, None, Some(error)).await?
                }
            }
            Ok(ToolAuthorization::Pending { .. }) => return Ok(Some(record.call)),
            Err(error) => {
                tracing::warn!(error = %error, "awp held call stays pending");
                return Ok(Some(record.call));
            }
        };
        let state = format!("{:?}", resolved.state).to_lowercase();
        self.audit("awp.capability.decided", &acting, id, &invocation, &state).await;
        Ok(Some(resolved))
    }

    /// Resolves every pending held call that has a decision or is past its expiry; returns
    /// the calls that left `pending`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the held-call store fails.
    pub async fn resolve_pending(&self) -> Result<Vec<HeldCall>, StoreError> {
        let mut resolved = Vec::new();
        for record in self.inner.held_calls.pending().await? {
            if let Some(call) = self.resolve(&record.call.id).await?
                && call.state != HeldCallState::Pending
            {
                resolved.push(call);
            }
        }
        Ok(resolved)
    }
}

#[async_trait]
impl CapabilityExecutor for GovernedExecutor {
    /// Runs `invocation` for the caller of the request being served.
    async fn invoke(&self, invocation: Invocation) -> InvocationOutcome {
        let caller = CURRENT_CALLER.try_with(Clone::clone).unwrap_or_else(|_| {
            let limit_key = invocation.requester.as_ref().map_or_else(
                || "ip:unknown".to_owned(),
                |requester| format!("id:{}", requester.as_str()),
            );
            EdgeCaller {
                tier: invocation.tier,
                requester: invocation.requester.clone(),
                ..EdgeCaller::public(limit_key)
            }
        });
        self.invoke_as(&caller, invocation).await
    }
}
