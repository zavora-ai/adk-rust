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
//! record or a log line. A take over is a live exchange: the call waits in flight for the
//! harness's confirmation timeout and is declined when it passes, because a one-time code
//! does not outlive a night of quiet time.
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
    ApprovalRisk, ApprovalScope, ApprovalService, ApprovalState, ApproverSelector,
    CredentialBroker, CredentialSecret, ErrorCategory, ErrorComponent, LlmRequest, ReadonlyContext,
    Result, RevocationScope, ScopedToken, Tool, ToolConfirmationRequest, ToolContext, ToolEffect,
    Toolset, Url,
};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::audit::HarnessAudit;

/// The tool name of every take-over inbox entry (spec 3.3 §10 H2). No tool can carry it: a
/// provider's function names allow no `:`.
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

/// How long an entry marked approved may wait for its secret before the call gives up: it
/// was answered with a plain `decide` instead of [`TakeOver::provide`].
const DELIVERY_GRACE: Duration = Duration::from_secs(5);

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

    /// Asks for a one-time code for `resource`.
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
    pub fn from_request(request: &ApprovalRequest) -> Option<Self> {
        if request.tool_name != TAKE_OVER_TOOL {
            return None;
        }
        let args = &request.args_redacted;
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

/// A call waiting for its secret.
struct Waiter {
    sender: Option<oneshot::Sender<Delivered>>,
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
    secrets: Arc<Mutex<Vec<Zeroizing<String>>>>,
}

tokio::task_local! {
    static CALL: CallScope;
}

/// Removes the waiter, and cancels a still-pending entry, when the waiting call ends or is
/// dropped.
struct WaitGuard {
    inner: Arc<Inner>,
    id: String,
    service: Arc<dyn ApprovalService>,
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
            let (service, id) = (self.service.clone(), self.id.clone());
            handle.spawn(async move {
                let _ = service.cancel(&id, Some("the tool call ended".to_string())).await;
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
    /// waits until they type it, decline, or the harness's confirmation timeout passes.
    ///
    /// The returned token's secret is for this call only: present it with
    /// [`ScopedToken::authorize_for`] to the resource asked about. Every occurrence of it in
    /// the call's result or error is replaced by [`TAKE_OVER_PLACEHOLDER`] before the model,
    /// the session or the action ledger see them.
    ///
    /// # Errors
    ///
    /// | Code | When |
    /// |---|---|
    /// | `harness.take_over.unavailable` | Called outside a call the harness runs, without `HarnessAgentBuilder::take_over`, or without an approval inbox |
    /// | `harness.take_over.declined` | The person declined, the entry was cancelled or expired, the timeout passed, or the call was cancelled |
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
        let timeout = ask.timeout.map_or(binding.timeout, |timeout| timeout.min(binding.timeout));
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

        let (sender, mut receiver) = oneshot::channel();
        self.inner
            .waiting
            .lock()
            .map_err(|_| unavailable("the take-over table is poisoned"))?
            .insert(id.clone(), Waiter { sender: Some(sender) });
        let mut guard = WaitGuard {
            inner: self.inner.clone(),
            id: id.clone(),
            service: service.clone(),
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
        tracing::info!(
            approval.id = %id,
            tool.name = %scope.tool,
            secret.kind = ?ask.kind,
            "take over requested"
        );

        let ended = self.wait(ctx, &service, &id, &mut receiver, timeout).await;
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
                let token = ScopedToken::new(
                    acting.agent.id.clone(),
                    delivered.secret.as_str(),
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
                        "the person did not type the secret within {} s",
                        timeout.as_secs()
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
    /// call's cancellation.
    async fn wait(
        &self,
        ctx: &dyn ToolContext,
        service: &Arc<dyn ApprovalService>,
        id: &str,
        receiver: &mut oneshot::Receiver<Delivered>,
        timeout: Duration,
    ) -> Ended {
        let deadline = Instant::now() + timeout;
        let mut approved_since: Option<Instant> = None;
        let mut poll = tokio::time::interval(crate::tools::GOVERNANCE_POLL_INTERVAL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                delivered = &mut *receiver => {
                    return match delivered {
                        Ok(delivered) => Ended::Provided(delivered),
                        Err(_) => Ended::Gone,
                    };
                }
                _ = poll.tick() => {
                    if let Some(config) = ctx.run_config()
                        && let Err(frozen) = config.check_governance()
                    {
                        return Ended::Stopped(frozen);
                    }
                    if ctx.is_cancelled() {
                        return Ended::Gone;
                    }
                    match service.get(id).await {
                        Ok(Some(request)) => match request.state {
                            ApprovalState::Pending => {}
                            // `provide` sends right after its decision lands.
                            ApprovalState::Approved => {
                                let since = *approved_since.get_or_insert_with(Instant::now);
                                if since.elapsed() >= DELIVERY_GRACE {
                                    return Ended::Declined(
                                        "it was marked done without a secret".to_string(),
                                    );
                                }
                                continue;
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
                    if Instant::now() >= deadline {
                        return Ended::TimedOut;
                    }
                }
            }
        }
    }

    /// Delivers the secret a person typed for the take-over entry `request_id` (spec 3.3
    /// §8.2 "Take over, then Done"), deciding it at `version` — the version the surface
    /// showed — as approved by `answer.by` on `answer.surface`.
    ///
    /// The decision is recorded in the inbox and the audit log without the value; the value
    /// goes to the waiting call only.
    ///
    /// # Errors
    ///
    /// | Code | When |
    /// |---|---|
    /// | `harness.take_over.invalid_secret` | The secret is shorter than [`TAKE_OVER_MIN_SECRET_CHARS`], longer than [`TAKE_OVER_MAX_SECRET_CHARS`], or holds control characters |
    /// | `harness.take_over.not_waiting` | No call in this process waits for the entry: it ended, or the harness restarted |
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
        let decided = service
            .decide(
                request_id,
                version,
                ApprovalDecision::approve(answer.by.clone())
                    .with_reason(format!("taken over on the {}", answer.surface)),
            )
            .await?;
        if decided.state != ApprovalState::Approved {
            return Ok(decided);
        }
        let sender = self
            .inner
            .waiting
            .lock()
            .ok()
            .and_then(|mut waiting| waiting.get_mut(request_id).and_then(|w| w.sender.take()));
        let delivered = Delivered {
            by: answer.by,
            surface: answer.surface,
            secret: Zeroizing::new(secret.to_string()),
        };
        match sender.map(|sender| sender.send(delivered).is_ok()) {
            Some(true) => Ok(decided),
            _ => Err(error(
                ErrorCategory::NotFound,
                TAKE_OVER_NOT_WAITING_CODE,
                format!(
                    "the call waiting for take over {request_id} ended before the secret arrived"
                ),
            )),
        }
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
/// harness runs may ask, for the agent running it.
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
fn scrub_text(text: &str, secrets: &[Zeroizing<String>]) -> Option<String> {
    let mut out: Option<String> = None;
    for secret in secrets {
        let encoded = Zeroizing::new(percent_encode(secret));
        for form in [secret.as_str(), encoded.as_str()] {
            let current = out.as_deref().unwrap_or(text);
            if current.contains(form) {
                out = Some(current.replace(form, TAKE_OVER_PLACEHOLDER));
            }
        }
    }
    out
}

/// RFC 3986 percent-encoding of everything but the unreserved characters.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `value` with the secrets replaced in every string and key.
fn scrub_value(value: Value, secrets: &[Zeroizing<String>]) -> Value {
    match value {
        Value::String(text) => Value::String(scrub_text(&text, secrets).unwrap_or(text)),
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
        other => other,
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
}

impl ScopedTool {
    pub(crate) fn wrap(tool: Arc<dyn Tool>) -> Arc<dyn Tool> {
        Arc::new(Self { inner: tool })
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
        let scope = CallScope {
            ctx: ctx.clone(),
            tool: self.inner.name().to_string(),
            secrets: secrets.clone(),
        };
        let result = CALL.scope(scope, self.inner.execute(ctx, args)).await;
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
}

impl ScopedToolset {
    pub(crate) fn wrap(toolset: Arc<dyn Toolset>) -> Arc<dyn Toolset> {
        Arc::new(Self { inner: toolset })
    }
}

#[async_trait]
impl Toolset for ScopedToolset {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(self.inner.tools(ctx).await?.into_iter().map(ScopedTool::wrap).collect())
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
