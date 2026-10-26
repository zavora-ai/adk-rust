//! The governed path a capability call enters from every binding (spec 3.5 §7.4, A-I7, A-I8;
//! AWP 2.0 §7.5).
//!
//! A [`CapabilityExecutor`] takes an [`Invocation`] — the capability, its input, the caller's
//! tier and requester id, its idempotency key and consent references — and answers an
//! [`InvocationOutcome`]: an output, a held call, or an error. Every binding (HTTP, A2A, MCP)
//! calls the same executor, so the outcome never depends on the binding.
//!
//! [`DirectExecutor`] is the built-in executor. In order it checks visibility by tier,
//! deprecation, the input schema, the idempotency key, consent, and the idempotency record;
//! then it holds the call for the owner when the approval hint says so, or runs the
//! capability. A runtime that routes calls through an agent's governed path supplies its own
//! executor.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::context::{Artifacts, CallbackContext, MemoryEntry, ReadonlyContext};
use adk_core::types::Content;
use adk_core::{EventActions, Tool, ToolContext, json_digest};
use async_trait::async_trait;
use awp_types::{ErrorBody, ErrorCode, HeldCall, HeldCallState, RequesterId, TrustTier};
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::capability::{
    CapabilityRegistry, CapabilityTool, META_CONSENT, META_REQUESTER, META_TIER, adk_error_to_body,
};
use super::store::{
    ConsentReceiptStore, HeldCallRecord, HeldCallStore, HeldCallTransition, IdempotencyBegin,
    IdempotencyStore, RecordedOutcome, StoreError, has_live_receipt,
};

/// The binding a call arrived over; it changes only the representation of the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Binding {
    /// `POST /awp/v2/capabilities/{name}`.
    Http,
    /// An A2A message.
    A2a,
    /// An MCP `tools/call`.
    Mcp,
}

/// One capability call as it enters the governed path (spec 3.5 §7.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    /// The capability's name.
    pub capability: String,
    /// The input object.
    pub input: Value,
    /// The caller's tier.
    pub tier: TrustTier,
    /// The caller's verified requester id, from `known` up.
    pub requester: Option<RequesterId>,
    /// The idempotency key the binding carried.
    pub idempotency_key: Option<String>,
    /// Consent receipt ids the call references.
    pub consent: Vec<Uuid>,
    /// The binding it arrived over.
    pub binding: Binding,
}

impl Invocation {
    /// A call over `binding` with no key, no consent references and a `public` caller.
    pub fn new(capability: impl Into<String>, input: Value, binding: Binding) -> Self {
        Self {
            capability: capability.into(),
            input,
            tier: TrustTier::Public,
            requester: None,
            idempotency_key: None,
            consent: Vec::new(),
            binding,
        }
    }
}

/// How a capability call ended (spec 3.5 §7.4).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum InvocationOutcome {
    /// The output object.
    Completed(Value),
    /// The call is held for the owner; HTTP answers `202` with its status URL.
    Held(HeldCall),
    /// The call failed or was refused.
    Failed(ErrorBody),
}

/// The governed path every binding calls (spec 3.5 §7.4, D-4).
#[async_trait]
pub trait CapabilityExecutor: Send + Sync {
    /// Executes, holds or refuses `invocation`.
    async fn invoke(&self, invocation: Invocation) -> InvocationOutcome;
}

/// The owner's decision on a held call (spec 3.5 §7.4).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum HeldDecision {
    /// Run the call.
    Approve,
    /// Refuse it, with a message for the requester.
    Reject {
        /// Why, with no internal detail.
        message: String,
    },
}

/// The built-in [`CapabilityExecutor`] (spec 3.5 §7.4, A-I7, A-I8).
///
/// # Example
///
/// ```
/// use std::sync::Arc;
///
/// use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry};
/// use adk_awp::v2::executor::{Binding, CapabilityExecutor, DirectExecutor, Invocation, InvocationOutcome};
/// use adk_awp::v2::store::MemoryEdgeStores;
/// use awp_types::{CapabilityEffect, CapabilityV2};
/// use serde_json::{Value, json};
///
/// struct Search;
/// #[async_trait::async_trait]
/// impl CapabilityHandler for Search {
///     async fn handle(&self, _call: CapabilityCall) -> Result<Value, CapabilityError> {
///         Ok(json!({ "results": [] }))
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let mut registry = CapabilityRegistry::new("example.com");
/// registry.declare(CapabilityV2::new("search_site", "Search", CapabilityEffect::ReadOnly), Arc::new(Search)).unwrap();
/// let executor = DirectExecutor::in_memory(Arc::new(registry), &MemoryEdgeStores::default());
/// let outcome = executor.invoke(Invocation::new("search_site", json!({}), Binding::Http)).await;
/// assert_eq!(outcome, InvocationOutcome::Completed(json!({ "results": [] })));
/// # }
/// ```
pub struct DirectExecutor {
    registry: Arc<CapabilityRegistry>,
    held_calls: Arc<dyn HeldCallStore>,
    idempotency: Arc<dyn IdempotencyStore>,
    consent: Arc<dyn ConsentReceiptStore>,
    hold_expiry: Option<chrono::Duration>,
}

impl std::fmt::Debug for DirectExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectExecutor").field("registry", &self.registry).finish_non_exhaustive()
    }
}

fn refused(code: ErrorCode, message: impl Into<String>) -> InvocationOutcome {
    InvocationOutcome::Failed(ErrorBody::new(code, message))
}

fn store_failed(error: &StoreError) -> InvocationOutcome {
    tracing::error!(error = %error, "awp edge store failed during a capability call");
    refused(ErrorCode::InternalError, "the site could not record the call")
}

impl DirectExecutor {
    /// Creates an executor over `registry` and the given stores.
    pub fn new(
        registry: Arc<CapabilityRegistry>,
        held_calls: Arc<dyn HeldCallStore>,
        idempotency: Arc<dyn IdempotencyStore>,
        consent: Arc<dyn ConsentReceiptStore>,
    ) -> Self {
        Self { registry, held_calls, idempotency, consent, hold_expiry: None }
    }

    /// Gives every held call an `expiresAt` this long after it is held; an undecided call
    /// then expires through [`Self::expire_due`]. Without it a call waits as long as the owner
    /// takes.
    pub fn with_hold_expiry(mut self, expiry: chrono::Duration) -> Self {
        self.hold_expiry = Some(expiry);
        self
    }

    /// Creates an executor over `registry` and the in-memory `stores`, sharing their records.
    pub fn in_memory(
        registry: Arc<CapabilityRegistry>,
        stores: &super::store::MemoryEdgeStores,
    ) -> Self {
        Self::new(
            registry,
            Arc::new(stores.held_calls.clone()),
            Arc::new(stores.idempotency.clone()),
            Arc::new(stores.consent.clone()),
        )
    }

    /// The scope of an idempotency key: the capability and, from `known` up, the requester.
    fn idempotency_scope(invocation: &Invocation, key: &str) -> String {
        let requester = invocation.requester.as_ref().map_or("", RequesterId::as_str);
        format!("{}\u{1f}{requester}\u{1f}{key}", invocation.capability)
    }

    async fn replay(&self, outcome: RecordedOutcome) -> InvocationOutcome {
        match outcome {
            RecordedOutcome::Completed { output } => InvocationOutcome::Completed(output),
            RecordedOutcome::Failed { error } => InvocationOutcome::Failed(error),
            RecordedOutcome::Held { call_id } => match self.held_calls.get(&call_id).await {
                Ok(Some(record)) => InvocationOutcome::Held(record.call),
                Ok(None) => refused(ErrorCode::InternalError, "the held call was not found"),
                Err(error) => store_failed(&error),
            },
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
        };
        if let Err(error) = self.idempotency.complete(scope, recorded).await {
            tracing::error!(error = %error, "awp idempotency outcome was not recorded");
        }
    }

    async fn run(
        tool: &Arc<CapabilityTool>,
        invocation: &Invocation,
        input: Value,
        call_id: &str,
    ) -> InvocationOutcome {
        let context = Arc::new(EdgeToolContext::new(tool, invocation, call_id));
        match Arc::clone(tool).execute(context, input).await {
            Ok(output) => InvocationOutcome::Completed(output),
            Err(error) => InvocationOutcome::Failed(adk_error_to_body(&error)),
        }
    }

    /// Resolves held call `id` with the owner's `decision`: an approved call runs and
    /// completes with its output or error; a rejected call ends `rejected` (spec 3.5 §7.4).
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] for an unknown call; [`StoreError::Conflict`] for a call
    /// that is not pending.
    pub async fn decide(&self, id: &str, decision: HeldDecision) -> Result<HeldCall, StoreError> {
        let record = self
            .held_calls
            .get(id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("held call {id}")))?;
        match decision {
            HeldDecision::Reject { message } => {
                let rejected = self
                    .held_calls
                    .transition(
                        id,
                        HeldCallTransition {
                            to: HeldCallState::Rejected,
                            result: None,
                            error: Some(ErrorBody::new(ErrorCode::Forbidden, message)),
                        },
                    )
                    .await?;
                Ok(rejected.call)
            }
            HeldDecision::Approve => {
                self.held_calls
                    .transition(
                        id,
                        HeldCallTransition {
                            to: HeldCallState::Approved,
                            result: None,
                            error: None,
                        },
                    )
                    .await?;
                let Some(tool) = self.registry.get(&record.call.capability) else {
                    return Err(StoreError::Conflict(format!(
                        "capability {} is no longer declared",
                        record.call.capability
                    )));
                };
                let mut invocation = Invocation::new(
                    record.call.capability.clone(),
                    record.input.clone(),
                    Binding::Http,
                );
                invocation.requester = record.requester.clone();
                invocation.tier = record.tier;
                invocation.consent = record.consent.clone();
                invocation.idempotency_key = record.idempotency_key.clone();
                let transition = match Self::run(tool, &invocation, record.input, id).await {
                    InvocationOutcome::Completed(output) => HeldCallTransition {
                        to: HeldCallState::Completed,
                        result: Some(output),
                        error: None,
                    },
                    InvocationOutcome::Failed(error) => HeldCallTransition {
                        to: HeldCallState::Completed,
                        result: None,
                        error: Some(error),
                    },
                    InvocationOutcome::Held(_) => HeldCallTransition {
                        to: HeldCallState::Completed,
                        result: None,
                        error: Some(ErrorBody::new(
                            ErrorCode::InternalError,
                            "the call was held again",
                        )),
                    },
                };
                Ok(self.held_calls.transition(id, transition).await?.call)
            }
        }
    }

    /// Expires every pending call whose `expiresAt` is at or before `now`; returns their ids.
    ///
    /// # Errors
    ///
    /// As the held-call store.
    pub async fn expire_due(&self, now: DateTime<Utc>) -> Result<Vec<String>, StoreError> {
        let mut expired = Vec::new();
        for record in self.held_calls.pending().await? {
            if record.call.expires_at.is_some_and(|expires_at| expires_at <= now) {
                self.held_calls
                    .transition(
                        &record.call.id,
                        HeldCallTransition {
                            to: HeldCallState::Expired,
                            result: None,
                            error: Some(ErrorBody::new(
                                ErrorCode::Forbidden,
                                "the owner did not decide in time",
                            )),
                        },
                    )
                    .await?;
                expired.push(record.call.id);
            }
        }
        Ok(expired)
    }
}

#[async_trait]
impl CapabilityExecutor for DirectExecutor {
    async fn invoke(&self, invocation: Invocation) -> InvocationOutcome {
        let now = Utc::now();
        // A capability above the caller's tier, or past its sunset, is reported as absent.
        let Some(tool) = self.registry.visible(&invocation.capability, invocation.tier) else {
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
            match has_live_receipt(self.consent.as_ref(), &invocation.consent, requirement, now)
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
            match self.idempotency.begin(scope, &json_digest(&invocation.input), now).await {
                Ok(IdempotencyBegin::Started) => {}
                Ok(IdempotencyBegin::Recorded(outcome)) => return self.replay(outcome).await,
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
                Err(error) => return store_failed(&error),
            }
        }

        let call_id = format!("call_{}", Uuid::new_v4().simple());
        let outcome = if declaration.approval.holds(tool.amount(&invocation.input).await) {
            let mut call = HeldCall::pending(call_id, declaration.name.clone(), now);
            call.expires_at = self.hold_expiry.map(|expiry| now + expiry);
            let record = HeldCallRecord {
                call,
                requester: invocation.requester.clone(),
                tier: invocation.tier,
                input: invocation.input.clone(),
                idempotency_key: invocation.idempotency_key.clone(),
                consent: invocation.consent.clone(),
            };
            match self.held_calls.create(record.clone()).await {
                Ok(()) => InvocationOutcome::Held(record.call),
                Err(error) => store_failed(&error),
            }
        } else {
            Self::run(tool, &invocation, invocation.input.clone(), &call_id).await
        };
        self.record(scope.as_deref(), &outcome).await;
        outcome
    }
}

/// The tool context of a capability call at the edge: the call's identity, its idempotency
/// key and the caller's tier, requester and consent references as request metadata.
pub struct EdgeToolContext {
    app_name: String,
    user_id: String,
    call_id: String,
    idempotency_key: String,
    metadata: HashMap<String, Value>,
    user_content: Content,
    actions: Mutex<EventActions>,
}

impl EdgeToolContext {
    /// The context of call `call_id` of `tool` for `invocation`.
    pub fn new(tool: &CapabilityTool, invocation: &Invocation, call_id: &str) -> Self {
        let mut metadata = HashMap::new();
        if let Some(requester) = &invocation.requester {
            metadata.insert(META_REQUESTER.to_owned(), Value::String(requester.to_string()));
        }
        metadata.insert(
            META_TIER.to_owned(),
            serde_json::to_value(invocation.tier).unwrap_or(Value::Null),
        );
        metadata.insert(
            META_CONSENT.to_owned(),
            serde_json::to_value(&invocation.consent).unwrap_or(Value::Null),
        );
        let idempotency_key =
            invocation.idempotency_key.clone().unwrap_or_else(|| call_id.to_owned());
        Self {
            app_name: tool.origin().to_owned(),
            user_id: invocation
                .requester
                .as_ref()
                .map_or_else(|| "public".to_owned(), ToString::to_string),
            call_id: call_id.to_owned(),
            idempotency_key,
            metadata,
            user_content: Content::new("user"),
            actions: Mutex::new(EventActions::default()),
        }
    }
}

#[async_trait]
impl ReadonlyContext for EdgeToolContext {
    fn invocation_id(&self) -> &str {
        &self.call_id
    }

    fn agent_name(&self) -> &str {
        "awp-edge"
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn session_id(&self) -> &str {
        ""
    }

    fn branch(&self) -> &str {
        ""
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for EdgeToolContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for EdgeToolContext {
    fn function_call_id(&self) -> &str {
        &self.call_id
    }

    fn idempotency_key(&self) -> String {
        self.idempotency_key.clone()
    }

    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = actions;
    }

    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    fn request_metadata(&self) -> HashMap<String, Value> {
        self.metadata.clone()
    }
}
