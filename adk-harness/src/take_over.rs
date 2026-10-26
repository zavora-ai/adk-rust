//! Take over (spec 3.3 §8.2, §10 H2): a tool asks the person for a secret — a password, a
//! one-time code — and the person types it on a surface. The secret reaches that one tool
//! call and nothing else.
//!
//! | Step | What happens | What is kept |
//! |---|---|---|
//! | Ask | A tool calls [`TakeOver::ask`] (or a [`CredentialBroker`] call reaches [`TakeOver`]) inside a call the harness runs | An inbox entry of kind take over ([`TakeOverRequest`]): the heading, the tool, the resource; a `harness.take_over.requested` audit record |
//! | Take over, then Done | A surface calls [`TakeOver::provide`] with the version it saw | The entry decided as approved by the person, with the surface; a `harness.take_over.provided` audit record naming who and where |
//! | Use | The tool receives a [`ScopedToken`] bound to the resource it asked about | Nothing: the value lives in memory for that call only |
//! | Answer | The call's result and error reach the model with every occurrence of the secret replaced by [`TAKE_OVER_PLACEHOLDER`] | The scrubbed result, as any other |
//!
//! The value never enters the approval inbox, a session event, the action ledger, an audit
//! record or a log line, and never reaches the harness's shell, whose commands the model
//! writes. A take over is a live exchange: the call waits in flight for the harness's
//! confirmation timeout, or until shortly before its own tool timeout, and is declined when
//! that passes, because a one-time code does not outlive a night of quiet time.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_harness::take_over::{TakeOver, TakeOverAsk};
//!
//! struct SignIn { take_over: TakeOver }
//!
//! #[async_trait]
//! impl Tool for SignIn {
//!     async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
//!         let bank = adk_core::resource_indicator("https://bank.example.com/login")?;
//!         let token = self
//!             .take_over
//!             .ask(ctx.as_ref(), TakeOverAsk::one_time_code("Type the code your bank sent", bank.clone()))
//!             .await?;
//!         let code = token.authorize_for(&bank)?;
//!         self.client.sign_in(code.expose()).await?;
//!         Ok(json!({ "signedIn": true }))
//!     }
//!     // name, description, ...
//! }
//!
//! let take_over = TakeOver::new();
//! let harness = HarnessAgent::builder("assistant")
//!     .tool(Arc::new(SignIn { take_over: take_over.clone() }))
//!     .take_over(take_over.clone())
//!     .approval_service(inbox)
//!     .build()?;
//! // A surface, once the person has typed the code:
//! take_over.provide(&entry.id, entry.version, TakeOverAnswer::new(person, TakeOverSurface::Phone, typed)).await?;
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use adk_core::principal::SubjectRef;
use adk_core::{
    ActingContext, AdkError, ApprovalAction, ApprovalDecision, ApprovalPolicy, ApprovalRequest,
    ApprovalRisk, ApprovalScope, ApprovalService, ApprovalState, ApproverSelector, Artifacts,
    CallbackContext, Content, CredentialBroker, CredentialSecret, ErrorCategory, ErrorComponent,
    Event, EventActions, LlmRequest, Memory, MemoryEntry, ReadonlyContext, Result, RevocationScope,
    RunConfig, ScopedToken, Session, SharedState, State, Tool, ToolConfirmationRequest,
    ToolContext, ToolEffect, ToolOutcome, Toolset, Url,
};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use crate::audit::HarnessAudit;

/// The tool name of every take-over inbox entry (spec 3.3 §10 H2). It is reserved: the
/// harness's policy denies a call to any tool that carries it, so such a call never reaches
/// the inbox, and [`TakeOverRequest::from_request`] also checks the entry's shape.
pub const TAKE_OVER_TOOL: &str = "harness:take-over";

/// What the model sees wherever the secret would appear in a call's result or error
/// (spec 3.3 §10 H2).
pub const TAKE_OVER_PLACEHOLDER: &str = "[secret typed by the person]";

/// Audit action: a tool asked the person for a secret.
pub const TAKE_OVER_REQUESTED: &str = "harness.take_over.requested";

/// Audit action: the person typed the secret; the record names who and on which surface,
/// never the value.
pub const TAKE_OVER_PROVIDED: &str = "harness.take_over.provided";

/// Audit action: the take over ended without a secret — declined, timed out, stopped or
/// cancelled.
pub const TAKE_OVER_ENDED: &str = "harness.take_over.ended";

/// Error code: take over cannot run here — outside a call the harness runs, without an
/// approval inbox, or without [`HarnessAgentBuilder::take_over`](crate::HarnessAgentBuilder::take_over).
pub const TAKE_OVER_UNAVAILABLE_CODE: &str = "harness.take_over.unavailable";

/// Error code: the person did not type the secret.
pub const TAKE_OVER_DECLINED_CODE: &str = "harness.take_over.declined";

/// Error code: no call in this process waits for the take over a surface answered.
pub const TAKE_OVER_NOT_WAITING_CODE: &str = "harness.take_over.not_waiting";

/// Error code: the typed secret is empty, too short, too long, or holds control characters.
pub const TAKE_OVER_INVALID_SECRET_CODE: &str = "harness.take_over.invalid_secret";

/// The shortest secret take over accepts; shorter ones could not be told apart from the
/// rest of a result when it is scrubbed.
pub const TAKE_OVER_MIN_SECRET_CHARS: usize = 4;

/// The longest secret take over accepts.
pub const TAKE_OVER_MAX_SECRET_CHARS: usize = 4096;

/// How long the [`ScopedToken`] a take over returns stays live.
pub const TAKE_OVER_TOKEN_LIFETIME: Duration = Duration::from_secs(600);

fn error(category: ErrorCategory, code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, category, code, message)
}

fn unavailable(message: impl Into<String>) -> AdkError {
    error(ErrorCategory::Unavailable, TAKE_OVER_UNAVAILABLE_CODE, message)
}

// A refusal, so the model is told why in plain words.
fn declined(message: impl Into<String>) -> AdkError {
    error(ErrorCategory::Forbidden, TAKE_OVER_DECLINED_CODE, message)
}

/// What kind of secret a tool asks for, which sets the entry's risk.
///
/// # Example
///
/// ```rust
/// use adk_harness::take_over::SecretKind;
///
/// assert_eq!(serde_json::to_value(SecretKind::OneTimeCode).unwrap(), "oneTimeCode");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SecretKind {
    /// A password: high risk.
    Password,
    /// A one-time code: medium risk.
    OneTimeCode,
    /// Any other secret: high risk.
    Other,
}

impl SecretKind {
    fn risk(self) -> ApprovalRisk {
        match self {
            Self::OneTimeCode => ApprovalRisk::Medium,
            Self::Password | Self::Other => ApprovalRisk::High,
        }
    }
}

/// What a tool asks the person to type (spec 3.3 §10 H2): a heading saying what happens, the
/// resource the secret is for, and its kind.
///
/// # Example
///
/// ```rust
/// use adk_harness::take_over::{SecretKind, TakeOverAsk};
///
/// let bank = adk_core::resource_indicator("https://bank.example.com/login").unwrap();
/// let ask = TakeOverAsk::one_time_code("Type the code your bank sent", bank);
/// assert_eq!(ask.kind(), SecretKind::OneTimeCode);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TakeOverAsk {
    heading: String,
    resource: Url,
    kind: SecretKind,
    timeout: Option<Duration>,
}

impl TakeOverAsk {
    /// Asks for a secret of kind [`Other`](SecretKind::Other) for `resource`.
    pub fn new(heading: impl Into<String>, resource: Url) -> Self {
        Self { heading: heading.into(), resource, kind: SecretKind::Other, timeout: None }
    }

    /// Asks for a one-time code for `resource`. The code reaches the tool without the spaces
    /// or dashes the person may type it with ("731 905" as `731905`).
    pub fn one_time_code(heading: impl Into<String>, resource: Url) -> Self {
        Self::new(heading, resource).with_kind(SecretKind::OneTimeCode)
    }

    /// Asks for a password for `resource`.
    pub fn password(heading: impl Into<String>, resource: Url) -> Self {
        Self::new(heading, resource).with_kind(SecretKind::Password)
    }

    /// Sets the kind.
    #[must_use]
    pub fn with_kind(mut self, kind: SecretKind) -> Self {
        self.kind = kind;
        self
    }

    /// Waits at most `timeout`, when it is shorter than the harness's confirmation timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// The kind of secret.
    pub fn kind(&self) -> SecretKind {
        self.kind
    }

    /// The resource as surfaces show it: scheme, host, port and path, without a query.
    fn resource_label(&self) -> String {
        let mut shown = self.resource.clone();
        shown.set_query(None);
        shown.set_fragment(None);
        shown.to_string()
    }
}

/// The surfaces a person may take over from (spec 3.3 §8.2: phone and web). Chat, voice and
/// the tray never carry a secret.
///
/// # Example
///
/// ```rust
/// use adk_harness::take_over::TakeOverSurface;
///
/// assert_eq!(TakeOverSurface::Phone.to_string(), "phone");
/// assert_eq!(serde_json::to_value(TakeOverSurface::Web).unwrap(), "web");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum TakeOverSurface {
    /// The web home.
    Web,
    /// The phone.
    Phone,
}

impl std::fmt::Display for TakeOverSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Web => "web",
            Self::Phone => "phone",
        })
    }
}

/// What a person typed, where, for one take over (spec 3.3 §10 H2). Its `Debug` output
/// redacts the secret.
///
/// # Example
///
/// ```rust
/// use adk_core::CredentialSecret;
/// use adk_core::principal::SubjectRef;
/// use adk_harness::take_over::{TakeOverAnswer, TakeOverSurface};
///
/// let answer = TakeOverAnswer::new(
///     SubjectRef::human("alice"),
///     TakeOverSurface::Phone,
///     CredentialSecret::new("731905"),
/// );
/// assert!(!format!("{answer:?}").contains("731905"));
/// ```
#[derive(Clone, Debug)]
pub struct TakeOverAnswer {
    /// Who typed it.
    pub by: SubjectRef,
    /// Where.
    pub surface: TakeOverSurface,
    /// The secret.
    pub secret: CredentialSecret,
}

impl TakeOverAnswer {
    /// The secret `by` typed on `surface`.
    pub fn new(by: SubjectRef, surface: TakeOverSurface, secret: CredentialSecret) -> Self {
        Self { by, surface, secret }
    }
}

/// A take-over inbox entry as a surface draws it (spec 3.3 §7.1, §8.2): the heading, the
/// tool, the resource and the kind of secret. It carries no secret.
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalPolicy, ApprovalRequest, ApprovalScope, ToolConfirmationRequest};
/// use adk_harness::take_over::TakeOverRequest;
/// use serde_json::json;
///
/// let call = ToolConfirmationRequest::new("send_email", json!({}));
/// let request = ApprovalRequest::new("r1", ApprovalScope::new("a", "u", "s"), &call, ApprovalPolicy::new([]));
/// assert!(TakeOverRequest::from_request(&request).is_none(), "an ordinary call");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TakeOverRequest {
    /// The inbox entry.
    pub request_id: String,
    /// The version a surface decides with.
    pub version: u64,
    /// What will happen, in plain words.
    pub heading: String,
    /// The tool that asked.
    pub tool: String,
    /// The kind of secret.
    pub secret: SecretKind,
    /// The resource the secret is for, without a query.
    pub resource: String,
    /// The call that waits for it.
    pub function_call_id: Option<String>,
}

impl TakeOverRequest {
    /// The take over `request` is, or `None` for any other inbox entry.
    ///
    /// A take over carries [`TAKE_OVER_TOOL`], the `takeOver` marker, the person as its
    /// requester and only Approve and Reject; a held call is never read as one, whatever
    /// its name and arguments.
    pub fn from_request(request: &ApprovalRequest) -> Option<Self> {
        let only_take_over_actions = request.allowed_actions.len() == 2
            && request.allowed_actions.contains(&ApprovalAction::Approve)
            && request.allowed_actions.contains(&ApprovalAction::Reject);
        let args = &request.args_redacted;
        if request.tool_name != TAKE_OVER_TOOL
            || args.get("takeOver") != Some(&Value::Bool(true))
            || !only_take_over_actions
            || request.requester.as_ref() != Some(&SubjectRef::human(&request.scope.user_id))
        {
            return None;
        }
        let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_string);
        Some(Self {
            request_id: request.id.clone(),
            version: request.version,
            heading: request.summary.clone(),
            tool: text("tool")?,
            secret: serde_json::from_value(args.get("secret")?.clone()).ok()?,
            resource: text("resource")?,
            function_call_id: text("functionCallId"),
        })
    }
}

/// The secret delivered to the waiting call.
struct Delivered {
    by: SubjectRef,
    surface: TakeOverSurface,
    secret: Zeroizing<String>,
}

/// A typed secret on its way to the waiting call, which decides the entry at `version` and
/// answers [`TakeOver::provide`] through `answer`: the call alone settles its entry, so a
/// secret is accepted only by a call that takes it (spec 3.3 §8.2).
struct Delivery {
    delivered: Delivered,
    version: u64,
    answer: oneshot::Sender<Result<ApprovalRequest>>,
}

/// A call waiting for its secret.
struct Waiter {
    sender: mpsc::UnboundedSender<Delivery>,
}

/// The answer to a [`TakeOver::provide`] whose call ended before it took the secret.
fn ended_before(request_id: &str) -> AdkError {
    error(
        ErrorCategory::NotFound,
        TAKE_OVER_NOT_WAITING_CODE,
        format!("the call waiting for take over {request_id} ended before the secret arrived"),
    )
}

#[derive(Clone)]
struct Binding {
    service: Option<Arc<dyn ApprovalService>>,
    timeout: Duration,
}

#[derive(Default)]
struct Inner {
    binding: RwLock<Option<Binding>>,
    waiting: Mutex<HashMap<String, Waiter>>,
}

/// Take over (spec 3.3 §8.2, §10 H2): the tool side ([`ask`](Self::ask)), the surface side
/// ([`provide`](Self::provide), [`decline`](Self::decline)), and the redaction that keeps the
/// secret to one call. See the [module documentation](self).
///
/// One `TakeOver` serves one harness: [`HarnessAgentBuilder::take_over`](crate::HarnessAgentBuilder::take_over)
/// binds it to the harness's approval inbox and confirmation timeout and scopes every tool
/// call, so a secret can reach a call and is removed from what the call returns. It is also a
/// [`CredentialBroker`] whose tokens the person types.
///
/// # Example
///
/// ```rust
/// let take_over = adk_harness::take_over::TakeOver::new();
/// assert!(!take_over.is_waiting("apr_1"));
/// ```
#[derive(Clone, Default)]
pub struct TakeOver {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TakeOver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let waiting = self.inner.waiting.lock().map_or(0, |waiting| waiting.len());
        f.debug_struct("TakeOver").field("waiting", &waiting).finish_non_exhaustive()
    }
}

/// The tool call a take over belongs to, set by [`ScopedTool`] around each call.
#[derive(Clone)]
struct CallScope {
    ctx: Arc<dyn ToolContext>,
    tool: String,
    /// The harness's shell, which never receives a typed secret: the command is the model's.
    shell: bool,
    /// When the call's own timeout drops it; a take over ends, recorded, before then.
    deadline: Option<Instant>,
    secrets: Arc<Mutex<Vec<Zeroizing<String>>>>,
}

tokio::task_local! {
    static CALL: CallScope;
}

/// How long before the call's own timeout a take over gives up, so it ends declined and
/// recorded rather than dropped mid-wait.
const CALL_DEADLINE_MARGIN: Duration = Duration::from_secs(2);

/// Removes the waiter when the waiting call ends or is dropped; a call dropped mid-wait
/// cancels its still-pending entry and records that the take over ended.
struct WaitGuard {
    inner: Arc<Inner>,
    id: String,
    service: Arc<dyn ApprovalService>,
    /// The audit trail and the record's fields, for a call dropped mid-wait.
    ended: Option<(HarnessAudit, Value)>,
    settled: bool,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        if let Ok(mut waiting) = self.inner.waiting.lock() {
            waiting.remove(&self.id);
        }
        if !self.settled
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let (service, id, ended) = (self.service.clone(), self.id.clone(), self.ended.take());
            handle.spawn(async move {
                let _ = service.cancel(&id, Some("the tool call ended".to_string())).await;
                if let Some((audit, mut fields)) = ended {
                    fields["reason"] = json!("the tool call ended");
                    if let Err(error) = audit.record(TAKE_OVER_ENDED, fields).await {
                        tracing::warn!(error = %error, approval.id = %id, "take over end not recorded");
                    }
                }
            });
        }
    }
}

/// How the wait for a secret ended.
enum Ended {
    Provided(Delivered),
    Declined(String),
    TimedOut,
    Stopped(AdkError),
    Gone,
}

impl TakeOver {
    /// A take over bound to no harness yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds the harness's inbox and confirmation timeout; the latest build wins.
    pub(crate) fn bind(&self, service: Option<Arc<dyn ApprovalService>>, timeout: Duration) {
        if let Ok(mut binding) = self.inner.binding.write() {
            *binding = Some(Binding { service, timeout });
        }
    }

    /// Whether a call in this process waits for the take-over entry `request_id`, so a
    /// surface can offer Take over rather than only Deny.
    pub fn is_waiting(&self, request_id: &str) -> bool {
        self.inner.waiting.lock().is_ok_and(|waiting| waiting.contains_key(request_id))
    }

    /// Asks the person for a secret on behalf of the tool call `ctx` (spec 3.3 §10 H2), and
    /// waits until they type it, decline, or the wait ends: at the harness's confirmation
    /// timeout, or shortly before the call's own tool timeout when that comes first, so an
    /// unanswered take over always ends declined and recorded. Raise the tool timeout
    /// ([`HarnessAgentBuilder::tool_timeout`](crate::HarnessAgentBuilder::tool_timeout) or
    /// the tool's `timeout_override`) to give the person longer.
    ///
    /// The returned token's secret is for this call only: present it with
    /// [`ScopedToken::authorize_for`] to the resource asked about. Every occurrence of it in
    /// the call's result or error — as typed or as a tool normalizes it (trimmed, without
    /// spaces or dashes, in either case, or a code's value without its leading zeros);
    /// percent- or form-encoded, JSON-escaped, or as a number — is replaced by
    /// [`TAKE_OVER_PLACEHOLDER`] before the model, the session or the action ledger see them,
    /// and progress or events the call streams once the secret has arrived are withheld.
    ///
    /// # Errors
    ///
    /// | Code | When |
    /// |---|---|
    /// | `harness.take_over.unavailable` | Called outside a call the harness runs, from the harness's shell, without `HarnessAgentBuilder::take_over`, or without an approval inbox |
    /// | `harness.take_over.declined` | The person declined, the entry was cancelled or expired, the wait ended, or the call was cancelled |
    /// | `governance.frozen` | Stop all froze the harness while the call waited |
    pub async fn ask(&self, ctx: &dyn ToolContext, ask: TakeOverAsk) -> Result<ScopedToken> {
        let scope = CALL.try_with(Clone::clone).map_err(|_| {
            unavailable(
                "take over runs only inside a tool call of a harness built with \
                 HarnessAgentBuilder::take_over",
            )
        })?;
        if scope.ctx.session_id() != ctx.session_id()
            || scope.ctx.function_call_id() != ctx.function_call_id()
        {
            return Err(unavailable("take over was asked for a call other than the running one"));
        }
        self.ask_in(&scope, ask).await
    }

    async fn ask_in(&self, scope: &CallScope, ask: TakeOverAsk) -> Result<ScopedToken> {
        // The shell runs a command the model wrote, which could print or transform the
        // secret past any scrub (spec 3.3 §10 H2).
        if scope.shell {
            return Err(unavailable(
                "take over never hands a secret to the shell: the model writes its commands; \
                 give the shell a credential broker that issues short-lived tokens instead",
            ));
        }
        let binding = self
            .inner
            .binding
            .read()
            .ok()
            .and_then(|binding| binding.clone())
            .ok_or_else(|| {
                unavailable("this take over is not bound to a harness: pass it to HarnessAgentBuilder::take_over")
            })?;
        let service = binding.service.ok_or_else(|| {
            unavailable(
                "take over needs the approval inbox, where a surface lets the person type the \
                 secret: give the harness an approval service, as `adk-rust up` does",
            )
        })?;
        let ctx = scope.ctx.as_ref();
        let mut timeout =
            ask.timeout.map_or(binding.timeout, |timeout| timeout.min(binding.timeout));
        if let Some(deadline) = scope.deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            let margin = CALL_DEADLINE_MARGIN.min(left / 4);
            timeout = timeout.min(left.saturating_sub(margin));
        }
        let acting = ctx.acting_context();
        let resource = ask.resource_label();
        let shown = json!({
            "takeOver": true,
            "tool": scope.tool,
            "secret": ask.kind,
            "resource": resource,
            "functionCallId": ctx.function_call_id(),
        });
        let mut call_args = shown.clone();
        call_args["nonce"] = Value::String(uuid::Uuid::new_v4().simple().to_string());
        let call = ToolConfirmationRequest::new(TAKE_OVER_TOOL, call_args)
            .with_function_call_id(ctx.function_call_id())
            .with_acting_context(acting.clone());
        let approval_scope = ApprovalScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id());
        let id = ApprovalRequest::id_for_call(&approval_scope, &call.fingerprint(), Utc::now());
        let person = SubjectRef::human(ctx.user_id());
        let mut policy =
            ApprovalPolicy::new([ApproverSelector::Subject(person.clone())]).allow_self_approval();
        policy.expires_after = Some(timeout);
        let request = ApprovalRequest::new(id.clone(), approval_scope, &call, policy)
            .with_summary(ask.heading.clone())
            .with_risk(ask.kind.risk())
            .with_redacted_args(shown)
            .with_allowed_actions([ApprovalAction::Approve, ApprovalAction::Reject])
            .with_requester(person)
            .with_acting(acting.clone())
            .with_expires_at(
                Utc::now()
                    + chrono::Duration::from_std(timeout).unwrap_or(chrono::Duration::seconds(300)),
            );

        let (sender, mut receiver) = mpsc::unbounded_channel();
        self.inner
            .waiting
            .lock()
            .map_err(|_| unavailable("the take-over table is poisoned"))?
            .insert(id.clone(), Waiter { sender });
        let mut guard = WaitGuard {
            inner: self.inner.clone(),
            id: id.clone(),
            service: service.clone(),
            ended: None,
            settled: false,
        };
        if let Err(submit) = service.submit(request).await {
            guard.settled = true;
            return Err(submit);
        }
        let audit = HarnessAudit::for_tool(ctx);
        let fields = |extra: Value| {
            let mut ext = json!({
                "tool": scope.tool,
                "functionCallId": ctx.function_call_id(),
                "approvalRequestId": id,
                "secretKind": ask.kind,
                "resourceHost": ask.resource.host_str(),
            });
            if let (Value::Object(ext), Value::Object(extra)) = (&mut ext, extra) {
                ext.extend(extra);
            }
            ext
        };
        if let Some(audit) = &audit
            && let Err(record) = audit.record(TAKE_OVER_REQUESTED, fields(json!({}))).await
        {
            return Err(record);
        }
        guard.ended = audit.clone().map(|audit| (audit, fields(json!({}))));
        tracing::info!(
            approval.id = %id,
            tool.name = %scope.tool,
            secret.kind = ?ask.kind,
            "take over requested"
        );

        let ended = self.wait(ctx, &service, &id, &mut receiver, timeout).await;
        // However the wait ended, no later secret is taken: the call leaves the table and a
        // secret already on its way is answered as too late, before anything else is awaited.
        if let Ok(mut waiting) = self.inner.waiting.lock() {
            waiting.remove(&id);
        }
        receiver.close();
        while let Ok(late) = receiver.try_recv() {
            let _ = late.answer.send(Err(ended_before(&id)));
        }
        let (reason, result) = match ended {
            Ended::Provided(delivered) => {
                guard.settled = true;
                if let Some(audit) = &audit {
                    audit
                        .record(
                            TAKE_OVER_PROVIDED,
                            fields(json!({ "by": delivered.by.id, "surface": delivered.surface })),
                        )
                        .await?;
                }
                tracing::info!(approval.id = %id, surface = %delivered.surface, "take over provided");
                if let Ok(mut secrets) = scope.secrets.lock() {
                    secrets.push(delivered.secret.clone());
                }
                // A code shown as "731 905" reaches the tool as the code; a password as typed.
                let value = match ask.kind {
                    SecretKind::OneTimeCode => compact(&delivered.secret),
                    SecretKind::Password | SecretKind::Other => delivered.secret.clone(),
                };
                let token = ScopedToken::new(
                    acting.agent.id.clone(),
                    value.as_str(),
                    ask.resource.clone(),
                    Vec::new(),
                )
                .expires_at(
                    Utc::now()
                        + chrono::Duration::from_std(TAKE_OVER_TOKEN_LIFETIME)
                            .unwrap_or(chrono::Duration::seconds(600)),
                );
                return Ok(token);
            }
            Ended::Declined(state) => {
                guard.settled = true;
                (
                    state.clone(),
                    Err(declined(format!("the person did not type the secret: {state}"))),
                )
            }
            Ended::TimedOut => {
                let _ = service.cancel(&id, Some("the take over timed out".to_string())).await;
                guard.settled = true;
                (
                    "timed out".to_string(),
                    Err(declined(format!(
                        "the person did not type the secret within {:?}",
                        Duration::from_millis(
                            u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
                        )
                    ))),
                )
            }
            Ended::Stopped(stopped) => {
                let _ = service.cancel(&id, Some("stopped".to_string())).await;
                guard.settled = true;
                ("stopped".to_string(), Err(stopped))
            }
            Ended::Gone => {
                let _ = service.cancel(&id, Some("the call was cancelled".to_string())).await;
                guard.settled = true;
                ("cancelled".to_string(), Err(declined("the take over was cancelled")))
            }
        };
        if let Some(audit) = &audit {
            audit.record(TAKE_OVER_ENDED, fields(json!({ "reason": reason }))).await?;
        }
        tracing::info!(approval.id = %id, reason = %reason, "take over ended without a secret");
        result
    }

    /// Waits for the secret, the entry's resolution elsewhere, the timeout, a freeze or the
    /// call's cancellation. A secret that arrives is taken only while none of the others has
    /// happened: the call decides its entry with it, and answers the surface that sent it.
    async fn wait(
        &self,
        ctx: &dyn ToolContext,
        service: &Arc<dyn ApprovalService>,
        id: &str,
        receiver: &mut mpsc::UnboundedReceiver<Delivery>,
        timeout: Duration,
    ) -> Ended {
        let deadline = Instant::now() + timeout;
        let mut poll = tokio::time::interval(crate::tools::GOVERNANCE_POLL_INTERVAL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let stopped = || match ctx.run_config().map(adk_core::RunConfig::check_governance) {
            Some(Err(frozen)) => Some(Ended::Stopped(frozen)),
            _ if ctx.is_cancelled() => Some(Ended::Gone),
            _ if Instant::now() >= deadline => Some(Ended::TimedOut),
            _ => None,
        };
        loop {
            tokio::select! {
                delivery = receiver.recv() => {
                    let Some(delivery) = delivery else { return Ended::Gone };
                    if let Some(ended) = stopped() {
                        let _ = delivery.answer.send(Err(ended_before(id)));
                        return ended;
                    }
                    let decision = ApprovalDecision::approve(delivery.delivered.by.clone())
                        .with_reason(format!("taken over on the {}", delivery.delivered.surface));
                    match service.decide(id, delivery.version, decision).await {
                        Ok(decided) if decided.state == ApprovalState::Approved => {
                            let _ = delivery.answer.send(Ok(decided));
                            return Ended::Provided(delivery.delivered);
                        }
                        // Refused, or decided without settling: the call waits on.
                        answered => {
                            let _ = delivery.answer.send(answered);
                        }
                    }
                }
                () = tokio::time::sleep_until(deadline.into()) => return Ended::TimedOut,
                _ = poll.tick() => {
                    if let Some(ended) = stopped() {
                        return ended;
                    }
                    match service.get(id).await {
                        Ok(Some(request)) => match request.state {
                            ApprovalState::Pending => {}
                            // Only this call approves its entry with the secret.
                            ApprovalState::Approved => {
                                return Ended::Declined(
                                    "it was marked done without a secret".to_string(),
                                );
                            }
                            state => return Ended::Declined(match state {
                                ApprovalState::Rejected => "the person declined".to_string(),
                                ApprovalState::Expired => "the request expired".to_string(),
                                _ => format!("the request was {}", state.as_str()),
                            }),
                        },
                        Ok(None) => return Ended::Declined("the request is gone".to_string()),
                        Err(read) => tracing::warn!(error = %read, approval.id = %id, "approval inbox unavailable while a take over waited"),
                    }
                }
            }
        }
    }

    /// Delivers the secret a person typed for the take-over entry `request_id` (spec 3.3
    /// §8.2 "Take over, then Done"), deciding it at `version` — the version the surface
    /// showed — as approved by `answer.by` on `answer.surface`.
    ///
    /// The waiting call records the decision, in the inbox and the audit log without the
    /// value, and takes the value; a call that stopped waiting first takes nothing and
    /// leaves the entry undecided.
    ///
    /// # Errors
    ///
    /// | Code | When |
    /// |---|---|
    /// | `harness.take_over.invalid_secret` | The secret is shorter than [`TAKE_OVER_MIN_SECRET_CHARS`], longer than [`TAKE_OVER_MAX_SECRET_CHARS`], or holds control characters |
    /// | `harness.take_over.not_waiting` | No call in this process waits for the entry: it ended, it stopped waiting — Stop all, its timeout, a cancel — before it took the secret, or the harness restarted |
    /// | `approval.version_conflict`, `approval.not_an_approver`, `approval.already_resolved` | The inbox refused the decision, as for any other; nothing was delivered |
    pub async fn provide(
        &self,
        request_id: &str,
        version: u64,
        answer: TakeOverAnswer,
    ) -> Result<ApprovalRequest> {
        let secret = answer.secret.expose();
        let chars = secret.chars().count();
        if !(TAKE_OVER_MIN_SECRET_CHARS..=TAKE_OVER_MAX_SECRET_CHARS).contains(&chars)
            || secret.chars().any(char::is_control)
        {
            return Err(error(
                ErrorCategory::InvalidInput,
                TAKE_OVER_INVALID_SECRET_CODE,
                format!(
                    "a secret has {TAKE_OVER_MIN_SECRET_CHARS} to {TAKE_OVER_MAX_SECRET_CHARS} \
                     characters and no line breaks or control characters"
                ),
            ));
        }
        let service = self.service()?;
        if !self.is_waiting(request_id) {
            // An entry already decided answers as any second decision does: a version
            // conflict or already resolved, shown as "already decided by …" (spec 3.3 S-I2).
            if let Some(entry) = service.get(request_id).await?
                && entry.state != ApprovalState::Pending
            {
                return service
                    .decide(request_id, version, ApprovalDecision::approve(answer.by.clone()))
                    .await;
            }
            return Err(error(
                ErrorCategory::NotFound,
                TAKE_OVER_NOT_WAITING_CODE,
                format!(
                    "no tool call in this process waits for take over {request_id}: it ended, or \
                     the harness restarted; the agent can ask again"
                ),
            ));
        }
        let sender = self
            .inner
            .waiting
            .lock()
            .ok()
            .and_then(|waiting| waiting.get(request_id).map(|waiter| waiter.sender.clone()));
        let (answer_to, answered) = oneshot::channel();
        let delivery = Delivery {
            delivered: Delivered {
                by: answer.by,
                surface: answer.surface,
                secret: Zeroizing::new(secret.to_string()),
            },
            version,
            answer: answer_to,
        };
        if sender.is_none_or(|sender| sender.send(delivery).is_err()) {
            return Err(ended_before(request_id));
        }
        answered.await.unwrap_or_else(|_| Err(ended_before(request_id)))
    }

    /// Declines the take-over entry `request_id` at `version` for `by` on `surface`: the
    /// waiting call ends without a secret.
    ///
    /// # Errors
    ///
    /// Returns the inbox's errors, as [`ApprovalService::decide`] does.
    pub async fn decline(
        &self,
        request_id: &str,
        version: u64,
        by: SubjectRef,
        surface: TakeOverSurface,
    ) -> Result<ApprovalRequest> {
        self.service()?
            .decide(
                request_id,
                version,
                ApprovalDecision::reject(by, format!("declined on the {surface}")),
            )
            .await
    }

    fn service(&self) -> Result<Arc<dyn ApprovalService>> {
        self.inner
            .binding
            .read()
            .ok()
            .and_then(|binding| binding.as_ref().and_then(|binding| binding.service.clone()))
            .ok_or_else(|| unavailable("this take over has no approval inbox"))
    }
}

/// The person is the issuer: a token is what they type (spec 3.3 §10 H2). Only a call the
/// harness runs may ask, for the agent running it, and never the harness's shell: as the
/// broker of [`HarnessAgentBuilder::credentials`](crate::HarnessAgentBuilder::credentials)
/// it refuses, and `bash` runs without a token.
#[async_trait]
impl CredentialBroker for TakeOver {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        _scopes: &[String],
    ) -> Result<ScopedToken> {
        let scope = CALL.try_with(Clone::clone).map_err(|_| {
            unavailable("take over issues tokens only inside a tool call the harness runs")
        })?;
        if scope.ctx.acting_context().agent.id != acting.agent.id {
            return Err(unavailable("take over issues tokens only to the agent running the call"));
        }
        let host = resource.host_str().unwrap_or("the service").to_string();
        self.ask_in(
            &scope,
            TakeOverAsk::new(format!("Type the secret for {host}"), resource.clone()),
        )
        .await
    }

    /// A typed secret is never stored, so there is nothing to withdraw.
    async fn revoke(&self, _scope: &RevocationScope) -> Result<u64> {
        Ok(0)
    }
}

// ── Keeping the secret to its call ───────────────────────────────────────────────────

/// `text` with every form of every secret replaced by the placeholder, or `None` when none
/// occurs.
///
/// A form spells each character of the secret as typed (in either ASCII case),
/// percent-encoded in either case, as `+` for a space (form encoding) or JSON-escaped, so the
/// secret is found however a URL, a form body or a serialized string carries it. The forms
/// are those of [`forms`]: the secret as typed and as a tool may normalize it.
fn scrub_text(text: &str, secrets: &[Zeroizing<String>]) -> Option<String> {
    let mut out: Option<String> = None;
    for form in forms(secrets) {
        let current = out.as_deref().unwrap_or(text);
        let mut scrubbed = String::new();
        let (mut copied, mut at, mut found) = (0, 0, false);
        while at < current.len() {
            if current[at..].starts_with(TAKE_OVER_PLACEHOLDER) {
                at += TAKE_OVER_PLACEHOLDER.len();
                continue;
            }
            match spelled_at(current, at, &form.text).filter(|end| {
                // A code's value is found only as a whole number: `7123` in "code 7123",
                // never in "17123".
                let alone = |ch: Option<char>| !ch.is_some_and(char::is_alphanumeric);
                !form.whole
                    || (alone(current[..at].chars().next_back())
                        && alone(current[*end..].chars().next()))
            }) {
                Some(end) => {
                    scrubbed.push_str(&current[copied..at]);
                    scrubbed.push_str(TAKE_OVER_PLACEHOLDER);
                    (copied, at, found) = (end, end, true);
                }
                None => at += current[at..].chars().next().map_or(1, char::len_utf8),
            }
        }
        if found {
            scrubbed.push_str(&current[copied..]);
            out = Some(scrubbed);
        }
    }
    out
}

/// A form of a secret the scrub looks for.
struct Form {
    text: Zeroizing<String>,
    /// Found only where no letter or digit adjoins it.
    whole: bool,
}

/// `secret` without whitespace or dashes: a one-time code as the tool receives it.
fn compact(secret: &str) -> Zeroizing<String> {
    Zeroizing::new(secret.chars().filter(|ch| !ch.is_whitespace() && *ch != '-').collect())
}

/// Every form of `secrets`, longest first: each as typed, trimmed, and without whitespace or
/// dashes, as a tool normalizes "731 905"; and for a code of digits, its value, which drops
/// leading zeros ("007123" read and printed as "7123"), as a whole number.
fn forms(secrets: &[Zeroizing<String>]) -> Vec<Form> {
    let mut forms: Vec<Form> = Vec::new();
    let mut add = |text: &str, whole: bool| {
        if !text.is_empty() && !forms.iter().any(|form| form.text.as_str() == text) {
            forms.push(Form { text: Zeroizing::new(text.to_string()), whole });
        }
    };
    for secret in secrets {
        add(secret, false);
        add(secret.trim(), false);
        let compact = compact(secret);
        add(&compact, false);
        if compact.bytes().all(|byte| byte.is_ascii_digit()) {
            let value = match compact.trim_start_matches('0') {
                "" => "0",
                value => value,
            };
            add(value, true);
        }
    }
    forms.sort_by_key(|form| std::cmp::Reverse(form.text.len()));
    forms
}

/// Where `secret` ends when `text` spells it, in any of its forms, from byte `start`.
///
/// A character can be spelled more than one way (`\\` is a backslash as typed, or the
/// start of its JSON escape), so every spelling is tried; each (secret, text) position is
/// tried once.
fn spelled_at(text: &str, start: usize, secret: &str) -> Option<usize> {
    // Most positions fail on the first character, before anything is allocated.
    spellings(&text[start..], secret.chars().next()?).next()?;
    let mut tried = std::collections::HashSet::new();
    let mut stack = vec![(0, start)];
    while let Some((done, at)) = stack.pop() {
        let Some(ch) = secret[done..].chars().next() else {
            return Some(at);
        };
        if !tried.insert((done, at)) {
            continue;
        }
        let Some(rest) = text.get(at..) else {
            continue;
        };
        let next: Vec<usize> = spellings(rest, ch).collect();
        // Reversed, so the plainest spelling is tried first.
        stack.extend(next.into_iter().rev().map(|len| (done + ch.len_utf8(), at + len)));
    }
    None
}

/// The lengths of every spelling of `ch` at the start of `rest`: as typed in either ASCII
/// case, `+` for a space, percent-encoded in either case, or JSON-escaped.
fn spellings(rest: &str, ch: char) -> impl Iterator<Item = usize> {
    let mut utf8 = [0u8; 4];
    let typed = ch.encode_utf8(&mut utf8);
    let raw = rest.as_bytes();
    // `prefix` then hex digits, read as a number; no sign, unlike `from_str_radix`.
    let escape = |at: usize, prefix: &str, digits: usize| -> Option<u32> {
        let start = at + prefix.len();
        let hex = raw.get(start..start + digits)?;
        (raw.get(at..start) == Some(prefix.as_bytes()))
            .then(|| hex.iter().try_fold(0, |n, d| Some(n * 16 + char::from(*d).to_digit(16)?)))
            .flatten()
    };
    let as_typed =
        rest.chars().next().filter(|first| first.eq_ignore_ascii_case(&ch)).map(char::len_utf8);
    let plus = (ch == ' ' && rest.starts_with('+')).then_some(1);
    let percent = typed
        .bytes()
        .enumerate()
        .all(|(i, byte)| escape(i * 3, "%", 2) == Some(u32::from(byte)))
        .then_some(typed.len() * 3);
    let short = match ch {
        '"' => Some("\\\""),
        '\\' => Some("\\\\"),
        '/' => Some("\\/"),
        _ => None,
    }
    .filter(|short| rest.starts_with(short))
    .map(str::len);
    let mut units = [0u16; 2];
    let units = ch.encode_utf16(&mut units);
    let unicode = units
        .iter()
        .enumerate()
        .all(|(i, unit)| escape(i * 6, "\\u", 4) == Some(u32::from(*unit)))
        .then_some(units.len() * 6);
    [as_typed, plus, percent, short, unicode].into_iter().flatten()
}

/// Whether a number spells a secret in any of its forms: "731905" holds "731 905", and 7123
/// is the code "007123" read as a number.
fn number_holds_secret(number: &serde_json::Number, secrets: &[Zeroizing<String>]) -> bool {
    scrub_text(&number.to_string(), secrets).is_some()
}

/// `value` with the secrets replaced in every string, key and number; a number that holds
/// one becomes the placeholder.
fn scrub_value(value: Value, secrets: &[Zeroizing<String>]) -> Value {
    match value {
        Value::String(text) => Value::String(scrub_text(&text, secrets).unwrap_or(text)),
        Value::Number(number) if number_holds_secret(&number, secrets) => {
            Value::String(TAKE_OVER_PLACEHOLDER.to_string())
        }
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|item| scrub_value(item, secrets)).collect())
        }
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| {
                    (scrub_text(&key, secrets).unwrap_or(key), scrub_value(value, secrets))
                })
                .collect(),
        ),
        other @ (Value::Null | Value::Bool(_) | Value::Number(_)) => other,
    }
}

/// `error` with the secrets replaced in its message and details; a source that carries one is
/// dropped.
fn scrub_error(mut error: AdkError, secrets: &[Zeroizing<String>]) -> AdkError {
    let source_leaks = std::error::Error::source(&error)
        .is_some_and(|source| scrub_text(&source.to_string(), secrets).is_some());
    if let Some(message) = scrub_text(&error.message, secrets) {
        error.message = message;
    }
    for value in [&mut error.details.request_id, &mut error.details.provider].into_iter().flatten()
    {
        if let Some(text) = scrub_text(value, secrets) {
            *value = text;
        }
    }
    let metadata = std::mem::take(&mut error.details.metadata);
    error.details.metadata = metadata
        .into_iter()
        .map(|(key, value)| (scrub_text(&key, secrets).unwrap_or(key), scrub_value(value, secrets)))
        .collect();
    if source_leaks {
        let mut rebuilt = AdkError::new(error.component, error.category, error.code, error.message);
        rebuilt.retry = error.retry;
        rebuilt.details = error.details;
        return rebuilt;
    }
    error
}

/// A tool whose calls may take over, with the secret scrubbed from what each returns.
pub(crate) struct ScopedTool {
    inner: Arc<dyn Tool>,
    /// The harness's tool timeout, which the agent applies unless the tool overrides it.
    tool_timeout: Duration,
    shell: bool,
}

impl ScopedTool {
    pub(crate) fn wrap(tool: Arc<dyn Tool>, tool_timeout: Duration) -> Arc<dyn Tool> {
        Arc::new(Self { inner: tool, tool_timeout, shell: false })
    }

    /// The harness's shell: its calls can never take over.
    pub(crate) fn wrap_shell(tool: Arc<dyn Tool>, tool_timeout: Duration) -> Arc<dyn Tool> {
        Arc::new(Self { inner: tool, tool_timeout, shell: true })
    }
}

/// The context a scoped call runs with: progress and events it streams once a secret has
/// reached it are withheld, since a scrub of a transformed copy cannot be guaranteed.
struct ScopedContext {
    inner: Arc<dyn ToolContext>,
    secrets: Arc<Mutex<Vec<Zeroizing<String>>>>,
    tool: String,
}

impl ScopedContext {
    fn holds_secret(&self) -> bool {
        let holds = self.secrets.lock().map_or(true, |secrets| !secrets.is_empty());
        if holds {
            tracing::debug!(tool.name = %self.tool, "streamed output withheld: a typed secret is in the call");
        }
        holds
    }
}

impl ReadonlyContext for ScopedContext {
    fn invocation_id(&self) -> &str {
        self.inner.invocation_id()
    }
    fn agent_name(&self) -> &str {
        self.inner.agent_name()
    }
    fn user_id(&self) -> &str {
        self.inner.user_id()
    }
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }
    fn session_id(&self) -> &str {
        self.inner.session_id()
    }
    fn branch(&self) -> &str {
        self.inner.branch()
    }
    fn user_content(&self) -> &Content {
        self.inner.user_content()
    }
    fn acting_context(&self) -> ActingContext {
        self.inner.acting_context()
    }
    fn is_authenticated(&self) -> bool {
        self.inner.is_authenticated()
    }
    fn state(&self) -> Option<&dyn State> {
        ReadonlyContext::state(self.inner.as_ref())
    }
}

#[async_trait]
impl CallbackContext for ScopedContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.inner.artifacts()
    }
    fn tool_outcome(&self) -> Option<ToolOutcome> {
        self.inner.tool_outcome()
    }
    fn tool_name(&self) -> Option<&str> {
        self.inner.tool_name()
    }
    fn tool_input(&self) -> Option<&Value> {
        self.inner.tool_input()
    }
    fn shared_state(&self) -> Option<Arc<SharedState>> {
        self.inner.shared_state()
    }
}

#[async_trait]
impl ToolContext for ScopedContext {
    fn function_call_id(&self) -> &str {
        self.inner.function_call_id()
    }
    fn idempotency_key(&self) -> String {
        self.inner.idempotency_key()
    }
    fn actions(&self) -> EventActions {
        self.inner.actions()
    }
    fn set_actions(&self, actions: EventActions) {
        self.inner.set_actions(actions);
    }
    async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>> {
        self.inner.search_memory(query).await
    }
    fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.inner.memory()
    }
    fn session(&self) -> Option<&dyn Session> {
        self.inner.session()
    }
    fn run_config(&self) -> Option<&RunConfig> {
        self.inner.run_config()
    }
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
    fn request_metadata(&self) -> HashMap<String, Value> {
        self.inner.request_metadata()
    }
    fn delegation_depth(&self) -> u32 {
        self.inner.delegation_depth()
    }
    fn max_delegation_depth(&self) -> Option<u32> {
        self.inner.max_delegation_depth()
    }
    fn orchestration_root_invocation_id(&self) -> &str {
        self.inner.orchestration_root_invocation_id()
    }
    fn orchestration_edge_id(&self) -> Option<&str> {
        self.inner.orchestration_edge_id()
    }
    async fn emit_event(&self, event: Event) {
        if !self.holds_secret() {
            self.inner.emit_event(event).await;
        }
    }
    async fn emit_progress(&self, stream: &str, chunk: &str) {
        if !self.holds_secret() {
            self.inner.emit_progress(stream, chunk).await;
        }
    }
    fn user_scopes(&self) -> Vec<String> {
        self.inner.user_scopes()
    }
    async fn get_secret(&self, name: &str) -> Result<Option<String>> {
        self.inner.get_secret(name).await
    }
    async fn get_secret_for_purpose(&self, name: &str, purpose: &str) -> Result<Option<String>> {
        self.inner.get_secret_for_purpose(name, purpose).await
    }
}

#[async_trait]
impl Tool for ScopedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self) -> &str {
        self.inner.description()
    }
    fn declaration(&self) -> Value {
        self.inner.declaration()
    }
    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }
    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }
    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }
    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }
    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }
    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }
    fn origin(&self) -> &str {
        self.inner.origin()
    }
    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }
    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }
    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }
    fn timeout_override(&self) -> Option<Option<Duration>> {
        self.inner.timeout_override()
    }
    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }
    fn is_agent_delegation(&self) -> bool {
        self.inner.is_agent_delegation()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let secrets = Arc::new(Mutex::new(Vec::new()));
        // The agent's timeout for this call started just before it reached this wrapper.
        let limit = self.inner.timeout_override().unwrap_or(Some(self.tool_timeout));
        let scope = CallScope {
            ctx: ctx.clone(),
            tool: self.inner.name().to_string(),
            shell: self.shell,
            deadline: limit.map(|limit| Instant::now() + limit),
            secrets: secrets.clone(),
        };
        let scoped: Arc<dyn ToolContext> = Arc::new(ScopedContext {
            inner: ctx,
            secrets: secrets.clone(),
            tool: scope.tool.clone(),
        });
        let result = CALL.scope(scope, self.inner.execute(scoped, args)).await;
        let secrets = std::mem::take(&mut *secrets.lock().unwrap_or_else(|e| e.into_inner()));
        if secrets.is_empty() {
            return result;
        }
        match result {
            Ok(value) => Ok(scrub_value(value, &secrets)),
            Err(failed) => Err(scrub_error(failed, &secrets)),
        }
    }
}

/// A toolset whose tools are [`ScopedTool`]s.
pub(crate) struct ScopedToolset {
    inner: Arc<dyn Toolset>,
    tool_timeout: Duration,
}

impl ScopedToolset {
    pub(crate) fn wrap(toolset: Arc<dyn Toolset>, tool_timeout: Duration) -> Arc<dyn Toolset> {
        Arc::new(Self { inner: toolset, tool_timeout })
    }
}

#[async_trait]
impl Toolset for ScopedToolset {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let tool_timeout = self.tool_timeout;
        Ok(self
            .inner
            .tools(ctx)
            .await?
            .into_iter()
            .map(|tool| ScopedTool::wrap(tool, tool_timeout))
            .collect())
    }

    async fn process_llm_request(
        &self,
        ctx: Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> Result<()> {
        self.inner.process_llm_request(ctx, request).await
    }

    async fn close(&self) {
        self.inner.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets(values: &[&str]) -> Vec<Zeroizing<String>> {
        values.iter().map(|value| Zeroizing::new((*value).to_string())).collect()
    }

    #[test]
    fn every_string_and_key_is_scrubbed() {
        let value = json!({
            "message": "signed in with 731905",
            "731905": ["code 731905", 7, { "url": "https://x/?c=p%40ss%20w" }],
        });
        let scrubbed = scrub_value(value, &secrets(&["731905", "p@ss w"]));
        let text = scrubbed.to_string();
        assert!(!text.contains("731905"), "{text}");
        assert!(!text.contains("p%40ss%20w"), "{text}");
        assert_eq!(scrubbed["message"], format!("signed in with {TAKE_OVER_PLACEHOLDER}"));
    }

    #[test]
    fn an_error_keeps_its_code_and_loses_the_secret() {
        let failed = AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Unauthorized,
            "bank.refused",
            "code 731905 refused",
        )
        .with_provider("bank 731905");
        let scrubbed = scrub_error(failed, &secrets(&["731905"]));
        assert_eq!(scrubbed.code, "bank.refused");
        assert_eq!(scrubbed.category, ErrorCategory::Unauthorized);
        assert!(!format!("{scrubbed:?}").contains("731905"), "{scrubbed:?}");
    }

    /// Review finding 1: a number whose text is the secret, contains it, or equals its value
    /// is scrubbed; other numbers stay.
    #[test]
    fn a_number_equal_to_the_secret_is_scrubbed() {
        let value = json!({ "otp": 731905, "lead": 7123, "zeros": 0, "count": 7, "big": 1731905 });
        let scrubbed = scrub_value(value, &secrets(&["731905", "007123", "0000"]));
        assert_eq!(scrubbed["otp"], TAKE_OVER_PLACEHOLDER);
        assert_eq!(scrubbed["lead"], TAKE_OVER_PLACEHOLDER, "leading zeros dropped by a parse");
        assert_eq!(scrubbed["zeros"], TAKE_OVER_PLACEHOLDER, "a code of zeros reads as 0");
        assert_eq!(scrubbed["big"], TAKE_OVER_PLACEHOLDER, "the digits appear in it");
        assert_eq!(scrubbed["count"], 7);
        let mut failed =
            AdkError::new(ErrorComponent::Tool, ErrorCategory::InvalidInput, "x", "refused");
        failed.details.metadata.insert("otp".into(), json!(731905));
        assert!(!format!("{:?}", scrub_error(failed, &secrets(&["731905"]))).contains("731905"));
    }

    /// Review finding 7: the form-urlencoded, lowercase-hex and JSON-escaped forms are
    /// scrubbed too.
    #[test]
    fn encoded_forms_of_the_secret_are_scrubbed() {
        let typed = secrets(&["correct horse~battery*", "pa\"ss\\word@1"]);
        for text in [
            "password=correct+horse%7Ebattery*",
            "password=correct%20horse~battery%2A",
            "password=correct%20horse%7ebattery%2a",
            r#"{"password":"pa\"ss\\word@1"}"#,
            "p=pa%22ss%5cword%401",
        ] {
            let scrubbed = scrub_text(text, &typed);
            assert!(
                scrubbed.as_deref().is_some_and(|s| s.contains(TAKE_OVER_PLACEHOLDER)),
                "{text}: {scrubbed:?}"
            );
        }
    }

    /// Review of #1045, findings 1, 2 and 7: the secret as a tool normalizes it — without
    /// separators, trimmed, upper-cased, or a code read as a number — is scrubbed in text and
    /// numbers; a code's value only as a whole number.
    #[test]
    fn a_normalized_secret_is_scrubbed() {
        for (typed, text) in [
            ("731 905", "code 731905 accepted"),
            ("731-905", "code 731905 accepted"),
            ("hunter22 ", "password hunter22, accepted"),
            ("ab12-cd34", "token AB12CD34"),
            ("007123", "the bank refused code 7123"),
            ("007123", "otp=7123&next=1"),
        ] {
            let scrubbed = scrub_text(text, &secrets(&[typed]));
            assert!(
                scrubbed.as_deref().is_some_and(|s| s.contains(TAKE_OVER_PLACEHOLDER)),
                "{typed:?} in {text}: {scrubbed:?}"
            );
        }
        assert_eq!(scrub_text("order 17123 and 71234", &secrets(&["007123"])), None);
        let value = json!({ "otp": 731905, "lead": 7123, "text": "7123", "other": 17123 });
        let scrubbed = scrub_value(value, &secrets(&["731 905", "007123"]));
        assert_eq!(
            scrubbed,
            json!({
                "otp": TAKE_OVER_PLACEHOLDER,
                "lead": TAKE_OVER_PLACEHOLDER,
                "text": TAKE_OVER_PLACEHOLDER,
                "other": 17123,
            })
        );
    }

    /// Review finding 6: an inbox entry the harness did not raise is never read as a take
    /// over, whatever its tool name and arguments.
    #[test]
    fn a_held_call_named_like_take_over_is_not_one() {
        let forged = json!({
            "takeOver": true,
            "tool": "bank_sign_in",
            "secret": "password",
            "resource": "https://bank.example.com/login",
        });
        let call = ToolConfirmationRequest::new(TAKE_OVER_TOOL, forged);
        let request = ApprovalRequest::new(
            "apr_1",
            ApprovalScope::new("a", "u", "s"),
            &call,
            ApprovalPolicy::new([ApproverSelector::human("u")]),
        );
        assert!(TakeOverRequest::from_request(&request).is_none(), "every action allowed");
    }

    #[test]
    fn a_take_over_entry_reads_back() {
        let call = ToolConfirmationRequest::new(TAKE_OVER_TOOL, json!({}));
        let request = ApprovalRequest::new(
            "apr_1",
            ApprovalScope::new("a", "u", "s"),
            &call,
            ApprovalPolicy::new([ApproverSelector::human("u")]),
        )
        .with_summary("Type the code your bank sent")
        .with_allowed_actions([ApprovalAction::Approve, ApprovalAction::Reject])
        .with_requester(SubjectRef::human("u"))
        .with_redacted_args(json!({
            "takeOver": true,
            "tool": "bank_sign_in",
            "secret": "oneTimeCode",
            "resource": "https://bank.example.com/login",
            "functionCallId": "call-1",
        }));
        let entry = TakeOverRequest::from_request(&request).unwrap();
        assert_eq!(entry.heading, "Type the code your bank sent");
        assert_eq!(entry.secret, SecretKind::OneTimeCode);
        assert_eq!(entry.function_call_id.as_deref(), Some("call-1"));
    }
}
