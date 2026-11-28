//! The executor hook every `tools/call` goes through (spec 3.5 §7.4, §7.9).

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use adk_core::{
    AdkError, ApprovalScope, ErrorCategory, ErrorComponent, GovernedCall, PolicyDecision, Result,
    RunConfig, Tool, ToolAuthorization, ToolGate, ToolPolicy, ToolPolicyRequest,
};
use async_trait::async_trait;
use rmcp::model::{InputRequests, InputResponses, Task};
use serde_json::Value;

use super::{McpCaller, McpRequestPeer};
use crate::SimpleToolContext;

/// The approval-scope session of a call that arrived outside an MCP session, such as over stdio
/// or on a stateless endpoint.
const SESSIONLESS: &str = "mcp";

/// One `tools/call` the server is about to run: the tool, its arguments, the caller and the
/// request that carried it.
///
/// The arguments are a JSON object that already matched the tool's parameters schema.
#[derive(Clone)]
#[non_exhaustive]
pub struct McpToolCall {
    /// The tool the call names.
    pub tool: Arc<dyn Tool>,
    /// The call's arguments, a JSON object.
    pub arguments: Value,
    /// The caller the server's [`McpCallerResolver`](super::McpCallerResolver) resolved.
    pub caller: McpCaller,
    /// The JSON-RPC request id, as a string; unique within its session, not across them.
    pub request_id: String,
    /// The MCP session the request belongs to, for stateful sessions.
    pub session_id: Option<String>,
    /// Whether the request declared the tasks extension (SEP-2663), so the executor may
    /// answer with [`McpToolOutcome::Task`].
    pub accepts_task: bool,
    /// Whether the client negotiated MCP `2026-07-28` or later, so the executor may answer
    /// with [`McpToolOutcome::InputRequired`].
    pub accepts_input_required: bool,
    /// The answers to the input requests an earlier [`McpToolOutcome::InputRequired`] asked,
    /// sent with the retried call (SEP-2322).
    pub input_responses: Option<InputResponses>,
    /// The `requestState` the client echoed back with the retried call. The client may have
    /// changed it, so an executor treats it as untrusted.
    pub request_state: Option<String>,
    /// The client end of the request, for progress notifications and, before `2026-07-28`,
    /// elicitation on the live connection; `None` for a call made outside a server.
    pub peer: Option<McpRequestPeer>,
}

impl McpToolCall {
    /// Creates a call, outside a session, for exercising an executor directly.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use adk_tool::FunctionTool;
    /// use adk_tool::mcp::server::{McpCaller, McpToolCall};
    /// use serde_json::json;
    ///
    /// let tool = Arc::new(FunctionTool::new("ping", "Answers pong", |_ctx, _args| async {
    ///     Ok(json!("pong"))
    /// }));
    /// let call = McpToolCall::new(tool, json!({}), McpCaller::anonymous(), "1")
    ///     .with_session_id("session-1");
    /// assert_eq!(call.session_id.as_deref(), Some("session-1"));
    /// ```
    pub fn new(
        tool: Arc<dyn Tool>,
        arguments: Value,
        caller: McpCaller,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            tool,
            arguments,
            caller,
            request_id: request_id.into(),
            session_id: None,
            accepts_task: false,
            accepts_input_required: false,
            input_responses: None,
            request_state: None,
            peer: None,
        }
    }

    /// Sets the MCP session the call belongs to.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Marks the call as made by a client that takes a task in answer (SEP-2663).
    pub fn with_task_support(mut self) -> Self {
        self.accepts_task = true;
        self
    }

    /// Marks the call as a retry carrying `responses` to an earlier `input_required` result
    /// and the `request_state` it echoed back (SEP-2322); the client takes `input_required`.
    pub fn with_input_responses(
        mut self,
        responses: InputResponses,
        request_state: Option<String>,
    ) -> Self {
        self.accepts_input_required = true;
        self.input_responses = Some(responses);
        self.request_state = request_state;
        self
    }
}

impl std::fmt::Debug for McpToolCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolCall")
            .field("tool", &self.tool.name())
            .field("caller", &self.caller.id())
            .field("request_id", &self.request_id)
            .field("session_id", &self.session_id)
            .field("accepts_task", &self.accepts_task)
            .field("accepts_input_required", &self.accepts_input_required)
            .field("request_state", &self.request_state.is_some())
            .finish_non_exhaustive()
    }
}

/// How a call that ran ended.
#[derive(Debug)]
#[non_exhaustive]
pub enum McpToolOutcome {
    /// The tool returned this value. An object becomes the result's `structuredContent`; every
    /// value is also sent as text.
    Completed(Value),
    /// The tool ran and failed. The result carries `isError: true` and the error as
    /// `adk_core::tool_error_response` describes it: a rejection's message, otherwise only its
    /// category and code.
    Failed(AdkError),
    /// The call continues as this task (SEP-2663), which the client follows through
    /// `tasks/get`, `tasks/update` and `tasks/cancel` on the server's
    /// [`McpTaskHost`](super::McpTaskHost). Only for a call that
    /// [accepts a task](McpToolCall::accepts_task); otherwise the call is refused.
    Task(Task),
    /// The call needs answers to `input_requests` before it can finish (SEP-2322): the client
    /// calls again with the answers and `request_state`. Only for a call that
    /// [accepts it](McpToolCall::accepts_input_required); otherwise the call is refused.
    InputRequired {
        /// The questions, keyed as the answers must be.
        input_requests: InputRequests,
        /// Echoed back by the client with the answers.
        request_state: Option<String>,
    },
}

/// Runs, or refuses, each call the server receives (spec 3.5 §7.9).
///
/// The server hands every `tools/call` to a listed, visible tool whose arguments match its
/// schema to [`execute`](Self::execute), so an executor is the one place to route calls
/// through `adk_core::authorize_tool_call` and the action ledger, as [`GovernedExecutor`]
/// does. An `Err` refuses the call: the client receives a JSON-RPC error whose message keeps
/// the error's details back unless the error is a rejection
/// ([`ErrorCategory::is_rejection`]).
///
/// # Example
///
/// ```rust
/// use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolEffect};
/// use adk_tool::mcp::server::{
///     GovernedExecutor, McpCallExecutor, McpCaller, McpToolCall, McpToolOutcome,
/// };
///
/// /// Anonymous callers may only read; every call still goes through the governed path.
/// struct ReadOnlyForAnonymous(GovernedExecutor);
///
/// #[async_trait::async_trait]
/// impl McpCallExecutor for ReadOnlyForAnonymous {
///     async fn execute(&self, call: McpToolCall) -> Result<McpToolOutcome> {
///         if !call.caller.is_authenticated() && call.tool.effect() != ToolEffect::ReadOnly {
///             return Err(AdkError::new(
///                 ErrorComponent::Tool,
///                 ErrorCategory::Forbidden,
///                 "mcp.caller.read_only",
///                 "sign in to make changes",
///             ));
///         }
///         self.0.execute(call).await
///     }
///
///     fn is_visible(&self, tool: &dyn Tool, caller: &McpCaller) -> bool {
///         caller.is_authenticated() || tool.effect() == ToolEffect::ReadOnly
///     }
/// }
/// ```
#[async_trait]
pub trait McpCallExecutor: Send + Sync {
    /// Runs `call`, or refuses it with an error.
    ///
    /// # Errors
    ///
    /// Returns the error the call is refused with; the client receives it as a JSON-RPC error.
    async fn execute(&self, call: McpToolCall) -> Result<McpToolOutcome>;

    /// Whether `caller` may see and call `tool`.
    ///
    /// `tools/list` lists only the tools this allows, and a call to any other tool is answered
    /// as a call to an unknown tool. The default shows every tool.
    fn is_visible(&self, tool: &dyn Tool, caller: &McpCaller) -> bool {
        let _ = (tool, caller);
        true
    }
}

/// The default executor: every call goes through [`adk_core::authorize_tool_call`] under a
/// [`RunConfig`], then the action ledger, before [`Tool::execute`].
///
/// | Step | From the config | A call it stops |
/// |------|-----------------|-----------------|
/// | Kill switch | `governance` | Refused with `governance.frozen` (or `governance.stale`), before and after the ledger write |
/// | Consent, egress, policy | `consent_gate`, `egress_policy`, `tool_policy` | Refused with `mcp.server.call_refused` and the reason |
/// | Confirmation | `tool_approvals`, `approval_store`, `tool_reviewer`, `tool_confirmation_handler` | Held with `mcp.server.call_held` when the config has an approval store, else refused with `mcp.server.no_approver` |
/// | Action ledger | `action_ledger`, for `NonIdempotent` tools | A replayed request is answered from its record and not run again |
/// | Audit | `audit` | Each decision is recorded before it takes effect |
///
/// A config without a `tool_policy` refuses every call: a server's callers are remote, so a
/// server with no rule runs no tool. The MCP client never decides a confirmation: only the
/// server's own handler, reviewer, standing approvals or approval store do, and a held call
/// runs when the client calls it again, with the same arguments in the same session, after an
/// approver decided it in the approval store.
///
/// A call is evaluated as the agent `app_name` of the app `app_name`, for the user the
/// caller's id — which the server's [`McpCallerResolver`](super::McpCallerResolver) or
/// [`local_caller`](super::McpServerBuilder::local_caller) sets, never the client — in the MCP
/// session (`mcp` for a call outside one), with function call id `mcp-{request id}`; a
/// ledgered call is keyed by the app, caller, session and request id. A confirmation
/// handler's wait counts against the server's call timeout.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::{DeclarativePolicy, GovernanceControl, RunConfig};
/// use adk_tool::mcp::server::GovernedExecutor;
///
/// let control = GovernanceControl::new();
/// let executor = GovernedExecutor::new(
///     "com.example/shop",
///     RunConfig::builder()
///         .tool_policy(Arc::new(DeclarativePolicy::builder().allow_read_only().build()))
///         .governance(control.clone())
///         .build(),
/// );
/// assert_eq!(executor.app_name(), "com.example/shop");
/// ```
#[derive(Debug, Clone)]
pub struct GovernedExecutor {
    app_name: String,
    config: Arc<RunConfig>,
}

impl GovernedExecutor {
    /// Creates an executor that governs each call of the app `app_name` under `config`.
    ///
    /// A `config` without a `tool_policy` gets one that refuses every call. The config's
    /// decisions keyed by call ID (`tool_confirmation_decisions`) are dropped: the client
    /// chooses the request id a call ID is made from, so it could claim one.
    pub fn new(app_name: impl Into<String>, mut config: RunConfig) -> Self {
        config.tool_policy.get_or_insert_with(|| Arc::new(NoToolPolicy));
        config.tool_confirmation_decisions.clear();
        config.tool_confirmation_fingerprints.clear();
        Self { app_name: app_name.into(), config: Arc::new(config) }
    }

    /// The app, and acting agent, name calls are evaluated under.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    /// The config every call is governed under.
    pub fn run_config(&self) -> &RunConfig {
        &self.config
    }
}

#[async_trait]
impl McpCallExecutor for GovernedExecutor {
    async fn execute(&self, call: McpToolCall) -> Result<McpToolOutcome> {
        let config = self.config.as_ref();
        let name = call.tool.name().to_string();
        let function_call_id = format!("mcp-{}", call.request_id);
        let session = call.session_id.as_deref().unwrap_or(SESSIONLESS);
        let scope = ApprovalScope::new(&self.app_name, call.caller.id(), session);
        let gate = ToolGate::new(config, &self.app_name, &function_call_id, scope);
        let governed =
            GovernedCall::for_tool(call.tool.as_ref(), &function_call_id, call.arguments);
        let args = match adk_core::authorize_tool_call(&gate, governed).await? {
            ToolAuthorization::Execute { args, .. } => args,
            ToolAuthorization::Refuse { reason, .. } => {
                return Err(refused("mcp.server.call_refused", reason));
            }
            ToolAuthorization::Pending { reason, .. } if config.approval_store.is_some() => {
                return Err(refused(
                    "mcp.server.call_held",
                    format!(
                        "{reason}. The call is held for an approver; call it again with the same \
                         arguments once it is approved"
                    ),
                ));
            }
            ToolAuthorization::Pending { reason, .. } => {
                return Err(refused(
                    "mcp.server.no_approver",
                    format!(
                        "{reason}. This server has no approver to decide it: give its governance \
                         config a `tool_confirmation_handler` or an `approval_store`, or permit \
                         the call in its `tool_policy`"
                    ),
                ));
            }
        };

        let mut context = SimpleToolContext::new(self.app_name.clone())
            .with_function_call_id(&function_call_id)
            .with_acting_context(gate.acting_context().clone());
        if let Some(session) = &call.session_id {
            context = context.with_session_id(session);
        }
        let context = Arc::new(context);
        let key = match &call.session_id {
            Some(session) => {
                format!("mcp/{}/{}/{session}/{}", self.app_name, call.caller.id(), call.request_id)
            }
            // Outside a session a request id repeats across requests, so it identifies nothing.
            None => format!("mcp/{}/{}/{}", self.app_name, call.caller.id(), uuid::Uuid::new_v4()),
        };
        let ledgered = match adk_core::begin_ledgered_call(
            config,
            context.as_ref(),
            call.tool.effect(),
            key,
            &name,
            &args,
        )
        .await?
        {
            Ok(ledgered) => ledgered,
            Err(answer) => {
                return match answer.cause {
                    Some(cause) => Err(cause),
                    None => Ok(McpToolOutcome::Completed(answer.response)),
                };
            }
        };
        // The ledger write can take long enough for a freeze to arrive: a call refused now
        // never starts, and its begun record is completed as not run (spec 3.1 I20).
        if let Err(refusal) = config.check_governance() {
            let text = adk_core::not_run_text(&refusal);
            if let Some(trail) = &config.audit {
                let draft = AuditRecordDraft::new(AuditRecordKind::ToolCall, &name)
                    .actor(AuditActor::new(gate.acting_context(), &function_call_id))
                    .args_digest(trail.digest(&args))
                    .outcome(AuditRecordOutcome::NotRun)
                    .ext("reason", Value::String(text.clone()))
                    .ext("sessionId", Value::String(session.to_string()));
                trail.record(draft).await?;
            }
            if let Some(ledgered) = ledgered {
                ledgered.complete(&name, None, &text).await;
            }
            return Err(refusal);
        }

        let result = call.tool.execute(context, args).await;
        if let Some(ledgered) = ledgered {
            // The record keeps the text the client is shown, never the error's own message.
            let shown = match &result {
                Ok(_) => Value::Null,
                Err(error) => adk_core::tool_error_response(&name, error, false)["error"].clone(),
            };
            ledgered.complete(&name, result.as_ref().ok(), shown.as_str().unwrap_or("")).await;
        }
        Ok(match result {
            Ok(value) => McpToolOutcome::Completed(value),
            Err(error) => McpToolOutcome::Failed(error),
        })
    }
}

/// The policy of a [`GovernedExecutor`] whose config sets none: no rule permits any call.
#[derive(Debug)]
struct NoToolPolicy;

#[async_trait]
impl ToolPolicy for NoToolPolicy {
    async fn evaluate(&self, _request: &ToolPolicyRequest) -> PolicyDecision {
        PolicyDecision::deny(
            "this MCP server has no tool policy, so it runs no tool; permit tools with a \
             `tool_policy` in the config passed to `McpServerBuilder::tool_governance`",
        )
    }

    fn policy_ref(&self) -> Option<String> {
        Some("mcp.server.no_policy".to_string())
    }
}

fn refused(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, ErrorCategory::Forbidden, code, message)
}

/// Calls [`Tool::execute`] with a [`SimpleToolContext`] and nothing else: no tool policy, kill
/// switch, confirmation, action ledger or audit record.
///
/// **Important:** with this executor any caller that reaches the server runs any tool it can
/// see. It is the explicit opt-out from [`GovernedExecutor`], for an executor that decides
/// each call itself before delegating to it, or a test fixture; it never fronts a reachable
/// server on its own.
///
/// The context's app and agent name are the executor's `app_name`, its session is the MCP
/// session, and its function call id is `mcp-{request id}`. A tool error is reported as
/// [`McpToolOutcome::Failed`]; nothing is refused.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_tool::mcp::server::{McpServer, UngovernedExecutor};
///
/// let server = McpServer::builder("com.example/fixture", "1.0.0")
///     .description("A local fixture whose every call runs")
///     .executor(Arc::new(UngovernedExecutor::new("com.example/fixture")))
///     .build()?;
/// assert_eq!(server.name(), "com.example/fixture");
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Debug, Clone)]
pub struct UngovernedExecutor {
    app_name: String,
}

impl UngovernedExecutor {
    /// Creates an executor whose tool contexts carry `app_name`.
    pub fn new(app_name: impl Into<String>) -> Self {
        Self { app_name: app_name.into() }
    }

    /// The app name the tool contexts carry.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }
}

#[async_trait]
impl McpCallExecutor for UngovernedExecutor {
    async fn execute(&self, call: McpToolCall) -> Result<McpToolOutcome> {
        let mut context = SimpleToolContext::new(self.app_name.clone())
            .with_function_call_id(format!("mcp-{}", call.request_id));
        if let Some(session) = call.session_id {
            context = context.with_session_id(session);
        }
        Ok(match call.tool.execute(Arc::new(context), call.arguments).await {
            Ok(value) => McpToolOutcome::Completed(value),
            Err(error) => McpToolOutcome::Failed(error),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use adk_core::audit_log::{AuditDecision, AuditTrail, InMemoryAuditRecorder};
    use adk_core::{
        ActionLedger, ApprovalStore, DeclarativePolicy, GOVERNANCE_FROZEN_CODE, GovernanceControl,
        InMemoryActionLedger, InMemoryApprovalStore, ToolApproval, ToolEffect,
    };
    use serde_json::json;

    use super::*;
    use crate::FunctionTool;
    use crate::mcp::server::McpServer;

    const APP: &str = "com.example/shop";

    /// A tool of `effect` that counts its runs.
    fn counted(name: &str, effect: ToolEffect) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&runs);
        let tool = FunctionTool::new(name, "a tool", move |_ctx, _args| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(json!({ "booked": true }))
            }
        })
        .with_effect(effect);
        (Arc::new(tool), runs)
    }

    fn call(tool: &Arc<dyn Tool>, request_id: &str) -> McpToolCall {
        McpToolCall::new(
            Arc::clone(tool),
            json!({ "date": "2026-12-01" }),
            McpCaller::authenticated("partner:acme"),
            request_id,
        )
        .with_session_id("s1")
    }

    fn policy(policy: DeclarativePolicy) -> RunConfig {
        RunConfig::builder().tool_policy(Arc::new(policy)).build()
    }

    fn completed(outcome: McpToolOutcome) -> Value {
        match outcome {
            McpToolOutcome::Completed(value) => value,
            other => panic!("the call completes: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_server_without_governance_refuses_every_call() {
        let (tool, runs) = counted("get_quote", ToolEffect::ReadOnly);
        let server = McpServer::builder(APP, "1.0.0")
            .description("Shop")
            .tool(Arc::clone(&tool))
            .build()
            .unwrap();
        let error = server.inner.executor.execute(call(&tool, "1")).await.unwrap_err();
        assert_eq!(error.code, "mcp.server.call_refused");
        assert_eq!(error.category, ErrorCategory::Forbidden);
        assert!(error.message.contains("no tool policy"), "{}", error.message);
        assert!(error.message.contains("tool_governance"), "names the fix: {}", error.message);
        assert_eq!(runs.load(Ordering::SeqCst), 0, "a refused call never runs");
    }

    #[tokio::test]
    async fn a_call_no_rule_permits_is_refused() {
        let (tool, runs) = counted("book_install", ToolEffect::NonIdempotent);
        let executor = GovernedExecutor::new(
            APP,
            policy(DeclarativePolicy::builder().allow("get_quote").build()),
        );
        let error = executor.execute(call(&tool, "1")).await.unwrap_err();
        assert_eq!(error.code, "mcp.server.call_refused");
        assert!(error.message.contains("no policy rule permits"), "{}", error.message);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_frozen_control_refuses_the_call_until_unfrozen() {
        let (tool, runs) = counted("book_install", ToolEffect::Idempotent);
        let control = GovernanceControl::new();
        let config = RunConfig::builder()
            .tool_policy(Arc::new(DeclarativePolicy::builder().allow("*").build()))
            .governance(control.clone())
            .build();
        let executor = GovernedExecutor::new(APP, config);

        control.freeze("incident 4012");
        let error = executor.execute(call(&tool, "1")).await.unwrap_err();
        assert_eq!(error.code, GOVERNANCE_FROZEN_CODE);
        assert!(error.message.contains("incident 4012"), "{}", error.message);
        assert_eq!(runs.load(Ordering::SeqCst), 0);

        control.unfreeze();
        completed(executor.execute(call(&tool, "2")).await.unwrap());
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_approval_required_call_with_no_approver_is_refused() {
        let (tool, runs) = counted("book_install", ToolEffect::NonIdempotent);
        let executor = GovernedExecutor::new(
            APP,
            policy(
                DeclarativePolicy::builder()
                    .require_approval("book_install", "a person confirms each booking")
                    .build(),
            ),
        );
        let error = executor.execute(call(&tool, "1")).await.unwrap_err();
        assert_eq!(error.code, "mcp.server.no_approver");
        assert_eq!(error.category, ErrorCategory::Forbidden);
        assert!(error.message.contains("a person confirms each booking"), "{}", error.message);
        assert!(error.message.contains("approval_store"), "names the fix: {}", error.message);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_client_cannot_claim_a_decision_keyed_by_call_id() {
        let (tool, runs) = counted("book_install", ToolEffect::NonIdempotent);
        let mut config = policy(
            DeclarativePolicy::builder()
                .require_approval("book_install", "a person confirms each booking")
                .build(),
        );
        config
            .tool_confirmation_decisions
            .insert("mcp-1".to_string(), adk_core::ToolConfirmationDecision::Approve);
        let executor = GovernedExecutor::new(APP, config);
        let error = executor.execute(call(&tool, "1")).await.unwrap_err();
        assert_eq!(error.code, "mcp.server.no_approver");
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_held_call_runs_once_an_approver_decides_it() {
        let (tool, runs) = counted("book_install", ToolEffect::NonIdempotent);
        let store = Arc::new(InMemoryApprovalStore::new());
        let config = RunConfig::builder()
            .tool_policy(Arc::new(
                DeclarativePolicy::builder()
                    .require_approval("book_install", "a person confirms each booking")
                    .build(),
            ))
            .approval_store(store.clone())
            .build();
        let executor = GovernedExecutor::new(APP, config);

        let held = executor.execute(call(&tool, "1")).await.unwrap_err();
        assert_eq!(held.code, "mcp.server.call_held");
        assert_eq!(runs.load(Ordering::SeqCst), 0);

        let scope = ApprovalScope::new(APP, "partner:acme", "s1");
        let pending = store.pending(&scope).await.unwrap();
        assert_eq!(pending.len(), 1, "the call waits in the store");
        store.decide(&scope, &pending[0].fingerprint, ToolApproval::approve()).await.unwrap();

        completed(executor.execute(call(&tool, "2")).await.unwrap());
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the decided call runs on its retry");
    }

    #[tokio::test]
    async fn an_allowed_call_runs_and_is_recorded() {
        let (tool, runs) = counted("book_install", ToolEffect::NonIdempotent);
        let recorder = InMemoryAuditRecorder::new();
        let ledger = Arc::new(InMemoryActionLedger::new());
        let config = RunConfig::builder()
            .tool_policy(Arc::new(DeclarativePolicy::builder().allow("book_install").build()))
            .action_ledger(ledger.clone())
            .audit(AuditTrail::new(Arc::new(recorder.clone())))
            .build();
        let executor = GovernedExecutor::new(APP, config);

        let value = completed(executor.execute(call(&tool, "7")).await.unwrap());
        assert_eq!(value, json!({ "booked": true }));
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        let decisions: Vec<(String, Option<AuditDecision>)> =
            recorder.records().into_iter().map(|record| (record.action, record.decision)).collect();
        assert_eq!(decisions, vec![("book_install".to_string(), Some(AuditDecision::Allowed))]);
        let record = ledger.get("mcp/com.example/shop/partner:acme/s1/7").await.unwrap();
        assert!(record.is_some_and(|record| record.is_completed()), "the call is ledgered");

        let replay = completed(executor.execute(call(&tool, "7")).await.unwrap());
        assert_eq!(replay["status"], "already_succeeded");
        assert_eq!(runs.load(Ordering::SeqCst), 1, "a replayed request is not run again");
    }

    #[tokio::test]
    async fn the_ungoverned_executor_runs_every_call() {
        let (tool, runs) = counted("book_install", ToolEffect::NonIdempotent);
        let executor = UngovernedExecutor::new(APP);
        let value = completed(executor.execute(call(&tool, "1")).await.unwrap());
        assert_eq!(value, json!({ "booked": true }));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_executor_and_a_governance_config_are_exclusive() {
        let error = McpServer::builder(APP, "1.0.0")
            .description("Shop")
            .executor(Arc::new(UngovernedExecutor::new(APP)))
            .tool_governance(RunConfig::default())
            .build()
            .unwrap_err();
        assert_eq!(error.code, "mcp.server.executor_and_governance");
    }
}
