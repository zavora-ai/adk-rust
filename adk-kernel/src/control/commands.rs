//! The harness REPL's slash commands and "Always allow" over the control plane (spec 3.3
//! §5.2, §7.2, §8.2).
//!
//! The kernel runs no command itself and names no harness type: a [`CommandHost`] is what the
//! harness gives the kernel, as a [`MemoryBook`](super::MemoryBook) is, so a terminal attached
//! to the kernel offers the same commands as the REPL, run by the same code over the kernel's
//! harness, and answers "Always allow" with the standing rule the harness makes in process.
//! `adk-rust up` builds one over its harness.
//!
//! | Route (under `/api`) | Caller | Does |
//! |---|---|---|
//! | `GET /kernel/commands` | the person, no device | The commands the host offers, in `/help` order ([`CommandsResponse`]) |
//! | `POST /kernel/commands/run` | the person, no device | Runs one in the caller's session ([`RunCommandRequest`] → [`RunCommandResponse`]); one that changes something is refused while everything is stopped |
//! | `GET /kernel/rules/always-allow/{id}` | the person, no device | What "Always allow" creates for the caller's waiting decision `id` ([`AlwaysAllowResponse`]); `404` when no standing rule applies |
//! | `POST /kernel/rules/always-allow/{id}/prepare` | the person, no device | Prepares that rule, before the decision is answered |
//! | `POST /kernel/rules/always-allow/{id}/grant` | the person, no device | Grants the prepared rule, once the caller's own yes was recorded |
//! | `GET /kernel/sessions/{session}/mode` | the person, no device | The mode of the caller's session ([`SessionModeResponse`]), read without a record; `404` when the host keeps no modes |
//! | `POST /kernel/surface/records` | the person, no device | Writes one record of a surface engine attached to the kernel ([`SurfaceRecord`]) on the kernel's signed log |
//!
//! "Always allow" takes two steps because the rule is made from the held call, which the
//! answer clears, and is granted only once the answer was recorded: an answer that loses to
//! one given elsewhere grants nothing, as in process. The routes never make a standing rule
//! of a high-risk call, and grant one only when the decision's last answer is the caller's
//! own "Allow once": an edit, or someone else's yes, grants nothing (spec 3.3 §7.2).
//!
//! While everything is stopped, a command that lets the agents do more is refused with
//! `409 kernel.frozen`; one that only makes them do less — the host says which
//! ([`CommandHost::tightens`]) — still runs, so a rule can be revoked or a fact forgotten
//! before Resume all (S-I10).
//!
//! A surface engine in another process, such as a terminal attached to the kernel, writes
//! its records — every answer with the surface and how it was given, Stop all, Resume all,
//! a mode change, a message sent — through `POST /kernel/surface/records`, so they reach the
//! same signed log as the home's (spec 3.3 §11). The kernel names the caller as the record's
//! actor and the transport it came over, whatever the record says; only `surface.*` actions
//! are written, and an answer's record only for the caller's own decision.
//!
//! A paired device's token is refused with `403`: commands and standing rules change what
//! the agents may do, which only the person at the kernel's socket, pipe or loopback, or
//! signed in to the home, may (S-I7). Each run is recorded as `kernel.control` with action
//! `commands.run`, the command's name as its target and the session, arguments, whether it
//! changes something and how the caller reached the kernel ([`CommandReach`]) as its details;
//! a granted rule as `rules.always_allow`, the decision as its target. Both are recorded
//! before they act. The host gets each run as a [`CommandCall`]: the caller, their session,
//! the command and how the caller reached the kernel, so a command can decide what it allows
//! by it.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalAction, ApprovalRequest, ApprovalRisk, ApprovalState, ErrorCategory,
    ErrorComponent,
};
use async_trait::async_trait;
use axum::Json;
use axum::Router;
use axum::extract::{Extension, Path as UrlPath, State};
use axum::http::Method;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::client::encode;
use super::routes::{Caller, ControlBackend, record};
use super::server::PeerProcess;
use super::settings::require_owner;
use super::{ControlTransport, paths};
use crate::error::{
    CONFIG_CODE, FROZEN_CODE, INVALID_STATE_CODE, KernelError, NOT_FOUND_CODE, Result,
};

/// How much of a command's arguments its record keeps.
const RECORDED_ARGS: usize = 200;

/// A slash command the kernel's host offers (spec 3.3 §5.2).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::CommandInfo;
///
/// let todos = CommandInfo::new("todos", "Show the session's todo list");
/// assert_eq!(serde_json::to_value(&todos).unwrap()["name"], "todos");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CommandInfo {
    /// The name typed after the slash: `todos` for `/todos`.
    pub name: String,
    /// One line on what it does.
    pub description: String,
}

impl CommandInfo {
    /// The command `name`, described as `description`.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into() }
    }
}

/// How the caller of a command reached the kernel (spec 3.3 §5.2): the transport, the paired
/// device whose token admitted it, and the calling process where the transport says. A
/// command reads it to decide what it allows; the run's record names it.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::{CommandReach, ControlTransport};
///
/// let local = CommandReach::new(ControlTransport::Socket).with_peer_pid(4242);
/// assert!(local.is_local() && local.device.is_none());
/// let phone = CommandReach::new(ControlTransport::Http).with_device("dev_1");
/// assert!(!phone.is_local());
/// assert_eq!(serde_json::to_value(&local).unwrap()["peerPid"], 4242);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CommandReach {
    /// The kernel's socket, its named pipe, or HTTP.
    pub transport: ControlTransport,
    /// The paired device whose token admitted the call; `None` for the person at the kernel
    /// or signed in to the home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The process that called, when the transport says: the socket's peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_pid: Option<u32>,
}

impl CommandReach {
    /// A call that came over `transport`, from no device and no known process.
    pub fn new(transport: ControlTransport) -> Self {
        Self { transport, device: None, peer_pid: None }
    }

    /// Sets the paired device whose token admitted the call.
    #[must_use]
    pub fn with_device(mut self, device: impl Into<String>) -> Self {
        self.device = Some(device.into());
        self
    }

    /// Sets the process that called.
    #[must_use]
    pub fn with_peer_pid(mut self, pid: u32) -> Self {
        self.peer_pid = Some(pid);
        self
    }

    /// Whether the caller is at the kernel's own machine as the OS user that runs it: the
    /// socket or the named pipe, which admit no one else.
    pub fn is_local(&self) -> bool {
        matches!(self.transport, ControlTransport::Socket | ControlTransport::Pipe)
    }
}

/// One run of a command, as the host gets it (spec 3.3 §5.2).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::{CommandCall, CommandReach, ControlTransport};
///
/// let call = CommandCall::new("ana", "s1", "todos", "", CommandReach::new(ControlTransport::Socket));
/// assert_eq!((call.user.as_str(), call.name.as_str()), ("ana", "todos"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommandCall {
    /// The caller; the host runs the command for this person only.
    pub user: String,
    /// The caller's session it runs in: the thread the terminal shows.
    pub session: String,
    /// The command's name, without the slash.
    pub name: String,
    /// The text after its name, trimmed.
    pub args: String,
    /// How the caller reached the kernel.
    pub reach: CommandReach,
}

impl CommandCall {
    /// `/name args` run by `user` in `session`, reached as `reach`.
    pub fn new(
        user: impl Into<String>,
        session: impl Into<String>,
        name: impl Into<String>,
        args: impl Into<String>,
        reach: CommandReach,
    ) -> Self {
        Self {
            user: user.into(),
            session: session.into(),
            name: name.into(),
            args: args.into(),
            reach,
        }
    }
}

/// `GET /kernel/commands` answer (spec 3.3 §5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandsResponse {
    /// The commands, in `/help` order.
    pub commands: Vec<CommandInfo>,
}

/// `POST /kernel/commands/run` body (spec 3.3 §5.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCommandRequest {
    /// The command's name, without the slash.
    pub name: String,
    /// The text after its name.
    #[serde(default)]
    pub args: String,
    /// The caller's session the command runs in: the thread the terminal shows.
    pub session_id: String,
}

/// `POST /kernel/commands/run` answer (spec 3.3 §5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCommandResponse {
    /// What the command returned, as the REPL prints it. Escape it before display (S-I9).
    pub text: String,
}

/// The answer of the "Always allow" routes: the words of the rule (spec 3.3 §8.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlwaysAllowResponse {
    /// The answer's words, such as "Always allow for this folder".
    pub label: String,
}

/// `GET /kernel/sessions/{session}/mode` answer: the mode of the caller's session, so a
/// terminal attached to the kernel shows the mode in force and knows whether a change
/// loosens it (spec 3.3 §5.2, §8.1).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::SessionModeResponse;
///
/// let answer: SessionModeResponse = serde_json::from_value(serde_json::json!({ "mode": "ask" })).unwrap();
/// assert_eq!(answer.mode, "ask");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModeResponse {
    /// The mode, as the harness names it: `read_only`, `plan`, `ask` or `auto`.
    pub mode: String,
}

/// `POST /kernel/surface/records` body: one record of a surface engine attached to the
/// kernel — an answer with the surface and how it was given, Stop all, Resume all, a mode
/// change, a message sent — for the kernel's signed log (spec 3.3 §11).
///
/// The kernel writes it with the caller as its actor and the transport it came over as
/// `transport`; whatever the record says of either is dropped.
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::AuditRecordKind;
/// use adk_kernel::control::SurfaceRecord;
///
/// let record = SurfaceRecord::new(AuditRecordKind::Governance, "surface.stop_all")
///     .with_ext("surface", serde_json::json!("terminal"))
///     .with_ext("origin", serde_json::json!({ "kind": "keyboard" }));
/// assert_eq!(serde_json::to_value(&record).unwrap()["kind"], "governance");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SurfaceRecord {
    /// What the record is about.
    pub kind: AuditRecordKind,
    /// The action: `surface.decide`, `surface.stop_all`, …
    pub action: String,
    /// The decision an answer's record carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<AuditDecision>,
    /// Its fields: the surface, how it was given, the decision's id, the outcome.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ext: BTreeMap<String, serde_json::Value>,
}

impl SurfaceRecord {
    /// A record of `action`, of `kind`, with no fields yet.
    pub fn new(kind: AuditRecordKind, action: impl Into<String>) -> Self {
        Self { kind, action: action.into(), decision: None, ext: BTreeMap::new() }
    }

    /// Sets the decision.
    #[must_use]
    pub fn with_decision(mut self, decision: AuditDecision) -> Self {
        self.decision = Some(decision);
        self
    }

    /// Adds the field `key`.
    #[must_use]
    pub fn with_ext(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.ext.insert(key.into(), value);
        self
    }
}

/// The prefix of the actions a surface engine writes (spec 3.3 §11).
const SURFACE_ACTIONS: &str = "surface.";

/// The fields of a surface's record the kernel sets itself: who the caller is and how it came.
const KERNEL_FIELDS: &[&str] = &["transport", "device", "deviceName", "presence"];

/// What the harness gives the kernel so a surface attached to it runs the REPL's commands
/// and answers "Always allow" (spec 3.3 §5.2, §7.2). A run's [`CommandCall`] names the
/// caller; the host runs a command in that person's session only.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::{CommandCall, CommandHost, CommandInfo};
///
/// struct Uptime;
///
/// #[async_trait::async_trait]
/// impl CommandHost for Uptime {
///     fn commands(&self) -> Vec<CommandInfo> {
///         vec![CommandInfo::new("uptime", "How long the kernel has run")]
///     }
///
///     fn changes(&self, _: &str, _: &str) -> bool {
///         false
///     }
///
///     async fn run(&self, call: &CommandCall) -> adk_kernel::Result<String> {
///         let place = if call.reach.is_local() { "here" } else { "over HTTP" };
///         Ok(format!("Up 3 hours; you called {place}"))
///     }
/// }
/// ```
#[async_trait]
pub trait CommandHost: Send + Sync {
    /// The commands, in `/help` order.
    fn commands(&self) -> Vec<CommandInfo>;

    /// Whether `/name args` changes something — a rule, the memory, the mode — rather than
    /// reads: while everything is stopped, it is refused unless it
    /// [`tightens`](Self::tightens).
    fn changes(&self, name: &str, args: &str) -> bool;

    /// Whether `call`, which changes something, only makes the agents do less — revokes a
    /// rule, forgets a fact, turns the sandbox on, sets a stricter mode — so it runs while
    /// everything is stopped, and a person can clean up before Resume all (spec 3.3 §5.2,
    /// S-I10). By default nothing does, and every change is refused until Resume all.
    ///
    /// # Errors
    ///
    /// The host's error, such as a store it reads the session's mode from.
    async fn tightens(&self, call: &CommandCall) -> Result<bool> {
        let _ = call;
        Ok(false)
    }

    /// The mode of `user`'s session `session`, as the harness names it (`read_only`,
    /// `plan`, `ask`, `auto`); `None` when the host keeps no modes, and by default.
    ///
    /// # Errors
    ///
    /// The host's error.
    async fn session_mode(&self, user: &str, session: &str) -> Result<Option<String>> {
        let _ = (user, session);
        Ok(None)
    }

    /// Runs `call`: `/name args` for its user in their session; returns what to show.
    ///
    /// # Errors
    ///
    /// A `NotFound` error for a command it does not offer, or the command's error.
    async fn run(&self, call: &CommandCall) -> Result<String>;

    /// The words of "Always allow" for the waiting decision `request`, when the harness can
    /// make a standing rule of its call; `None` otherwise, and by default.
    ///
    /// # Errors
    ///
    /// The host's error.
    async fn always_allow_offer(&self, request: &ApprovalRequest) -> Result<Option<String>> {
        let _ = request;
        Ok(None)
    }

    /// Makes the standing rule "Always allow" creates for the waiting decision `request`, as
    /// `by`, and keeps it until [`grant_always_allow`](Self::grant_always_allow); returns its
    /// words.
    ///
    /// # Errors
    ///
    /// An `InvalidInput` error when no standing rule applies to the call; by default, a
    /// `Config` error.
    async fn prepare_always_allow(
        &self,
        request: &ApprovalRequest,
        by: &SubjectRef,
    ) -> Result<String> {
        let _ = (request, by);
        Err(no_rules())
    }

    /// Grants the rule prepared for `request`, which was allowed.
    ///
    /// # Errors
    ///
    /// A `NotFound` error when no rule was prepared for it; by default, a `Config` error.
    async fn grant_always_allow(&self, request: &ApprovalRequest) -> Result<()> {
        let _ = request;
        Err(no_rules())
    }
}

fn no_rules() -> KernelError {
    KernelError::Core(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::InvalidInput,
        CONFIG_CODE,
        "this kernel makes no standing rules from a decision: allow it once",
    ))
}

type Backend = State<Arc<dyn ControlBackend>>;

/// The routes, relative to `/api`, over the backend in their state.
pub(super) fn routes() -> Router<Arc<dyn ControlBackend>> {
    Router::new()
        .route(paths::COMMANDS, get(list))
        .route(paths::COMMAND_RUN, post(run))
        .route(paths::ALWAYS_ALLOW, get(offer))
        .route(paths::ALWAYS_ALLOW_PREPARE, post(prepare))
        .route(paths::ALWAYS_ALLOW_GRANT, post(grant))
        .route(paths::SESSION_MODE, get(session_mode))
        .route(paths::SURFACE_RECORDS, post(surface_record))
}

/// The command host of `backend`, or a `Config` error naming why there is none.
fn host_of(backend: &dyn ControlBackend) -> Result<Arc<dyn CommandHost>> {
    backend.kernel().and_then(|kernel| kernel.config().control.commands.clone()).ok_or_else(|| {
        KernelError::Config {
            reason: "this kernel offers no commands: set ControlPlaneConfig::with_commands, as \
                     `adk-rust up` does"
                .into(),
        }
    })
}

async fn list(State(backend): Backend, caller: Caller) -> Result<Json<CommandsResponse>> {
    require_owner(&caller, "use the REPL's commands")?;
    Ok(Json(CommandsResponse { commands: host_of(backend.as_ref())?.commands() }))
}

async fn run(
    State(backend): Backend,
    caller: Caller,
    peer: Option<Extension<PeerProcess>>,
    Json(request): Json<RunCommandRequest>,
) -> Result<Json<RunCommandResponse>> {
    require_owner(&caller, "run the REPL's commands")?;
    let host = host_of(backend.as_ref())?;
    let RunCommandRequest { name, args, session_id } = request;
    let args = args.trim().to_string();
    if !host.commands().iter().any(|command| command.name == name) {
        return Err(KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::NotFound,
            NOT_FOUND_CODE,
            format!("no command /{name} on this kernel; /help lists them"),
        )));
    }
    let changes = host.changes(&name, &args);
    let mut reach = CommandReach::new(caller.transport);
    if let Some(device) = caller.context.metadata.get(crate::pairing::DEVICE_ID_METADATA) {
        reach = reach.with_device(device.clone());
    }
    if let Some(Extension(PeerProcess(pid))) = peer {
        reach = reach.with_peer_pid(pid);
    }
    let recorded: String = args.chars().take(RECORDED_ARGS).collect();
    let call = CommandCall::new(caller.context.user_id.clone(), &session_id, &name, args, reach);
    // Stopped, the agents may still be made to do less: cleaning up never waits (S-I10).
    let governance = backend.governance();
    if changes && governance.is_frozen() && !host.tightens(&call).await? {
        let reason = governance.reason().unwrap_or_default();
        return Err(KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Conflict,
            FROZEN_CODE,
            format!(
                "everything is stopped ({reason}), so /{name} cannot let the agents do more \
                 until you resume all; what makes them do less still runs"
            ),
        )));
    }
    let extra = [
        ("session", json!(session_id)),
        ("args", json!(recorded)),
        ("changes", json!(changes)),
        ("reach", json!(call.reach)),
    ];
    record(backend.as_ref(), &caller, "commands.run", Some(&name), &extra).await?;
    let text = host.run(&call).await?;
    Ok(Json(RunCommandResponse { text }))
}

/// The caller's decision `id`, from the kernel's approvals inbox (S-I8).
async fn owned_request(
    backend: &dyn ControlBackend,
    caller: &Caller,
    id: &str,
) -> Result<ApprovalRequest> {
    let approvals = backend.approvals().ok_or_else(|| KernelError::Config {
        reason: "this kernel has no approvals inbox; start it with an approval service".into(),
    })?;
    approvals
        .get(id)
        .await?
        .filter(|request| request.scope.user_id == caller.context.user_id)
        .ok_or_else(|| {
            KernelError::Core(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::NotFound,
                NOT_FOUND_CODE,
                format!("no decision {id} of yours"),
            ))
        })
}

/// The caller's waiting decision `id`.
async fn waiting(
    backend: &dyn ControlBackend,
    caller: &Caller,
    id: &str,
) -> Result<ApprovalRequest> {
    let request = owned_request(backend, caller, id).await?;
    if request.state != ApprovalState::Pending {
        return Err(KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Conflict,
            INVALID_STATE_CODE,
            format!("decision {id} is no longer waiting"),
        )));
    }
    Ok(request)
}

/// Whether `request` is high risk, as it says or as the kernel rates its tool: "Always allow"
/// is never offered for it, on any surface (spec 3.3 §7.2).
fn high_risk(backend: &dyn ControlBackend, request: &ApprovalRequest) -> bool {
    request.risk >= ApprovalRisk::High || super::pairing::rated_high(backend.kernel(), request)
}

/// The answer of a call no standing rule may be made of.
fn no_rule_applies() -> KernelError {
    KernelError::Core(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::NotFound,
        NOT_FOUND_CODE,
        "no standing rule applies to this call; allow it once",
    ))
}

async fn offer(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<AlwaysAllowResponse>> {
    require_owner(&caller, "see what Always allow creates")?;
    let host = host_of(backend.as_ref())?;
    let request = waiting(backend.as_ref(), &caller, &id).await?;
    if high_risk(backend.as_ref(), &request) {
        return Err(no_rule_applies());
    }
    match host.always_allow_offer(&request).await? {
        Some(label) => Ok(Json(AlwaysAllowResponse { label })),
        None => Err(no_rule_applies()),
    }
}

async fn prepare(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<AlwaysAllowResponse>> {
    require_owner(&caller, "create a standing rule")?;
    let host = host_of(backend.as_ref())?;
    let request = waiting(backend.as_ref(), &caller, &id).await?;
    if high_risk(backend.as_ref(), &request) {
        return Err(crate::pairing::forbidden(format!(
            "decision {id} is high risk: no standing rule is ever made of it; allow it once"
        )));
    }
    let label = host.prepare_always_allow(&request, &caller.subject()).await?;
    Ok(Json(AlwaysAllowResponse { label }))
}

async fn grant(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<serde_json::Value>> {
    require_owner(&caller, "create a standing rule")?;
    let host = host_of(backend.as_ref())?;
    let request = owned_request(backend.as_ref(), &caller, &id).await?;
    // The rule follows the caller's own yes, recorded first: an answer that lost, an edit, or
    // someone else's yes grants nothing.
    let own_yes = request.decisions.last().is_some_and(|decision| {
        decision.kind == ApprovalAction::Approve && decision.approver.id == caller.context.user_id
    });
    if request.state != ApprovalState::Approved || !own_yes || high_risk(backend.as_ref(), &request)
    {
        return Err(KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Conflict,
            INVALID_STATE_CODE,
            format!("decision {id} was not allowed by your yes, so no rule is made from it"),
        )));
    }
    record(backend.as_ref(), &caller, "rules.always_allow", Some(&id), &[]).await?;
    host.grant_always_allow(&request).await?;
    Ok(Json(json!({ "granted": id })))
}

async fn session_mode(
    State(backend): Backend,
    caller: Caller,
    UrlPath(session): UrlPath<String>,
) -> Result<Json<SessionModeResponse>> {
    require_owner(&caller, "read a session's mode")?;
    let host = host_of(backend.as_ref())?;
    match host.session_mode(&caller.context.user_id, &session).await? {
        Some(mode) => Ok(Json(SessionModeResponse { mode })),
        None => Err(KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::NotFound,
            NOT_FOUND_CODE,
            "this kernel keeps no mode for its sessions",
        ))),
    }
}

async fn surface_record(
    State(backend): Backend,
    caller: Caller,
    Json(record): Json<SurfaceRecord>,
) -> Result<Json<serde_json::Value>> {
    require_owner(&caller, "write a surface's records")?;
    let SurfaceRecord { kind, action, decision, ext } = record;
    let kinds = [
        AuditRecordKind::Approval,
        AuditRecordKind::Governance,
        AuditRecordKind::Configuration,
        AuditRecordKind::Other("surface".into()),
    ];
    if !action.starts_with(SURFACE_ACTIONS) || !kinds.contains(&kind) {
        return Err(KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::InvalidInput,
            CONFIG_CODE,
            format!("only a surface's records (`surface.…`) are written here, not `{action}`"),
        )));
    }
    // An answer is on record for the caller's own decision only (S-I8).
    if let Some(id) = ext.get("approvalId") {
        let id = id.as_str().unwrap_or_default();
        owned_request(backend.as_ref(), &caller, id).await?;
    }
    let Some(trail) = backend.audit_trail() else {
        return Ok(Json(json!({ "recorded": false })));
    };
    let mut draft =
        AuditRecordDraft::new(kind, action).actor(AuditActor::subject(caller.subject()));
    if let Some(decision) = decision {
        draft = draft.decision(decision);
    }
    for (key, value) in ext {
        if !KERNEL_FIELDS.contains(&key.as_str()) {
            draft = draft.ext(key, value);
        }
    }
    draft = draft.ext("transport", json!(caller.transport));
    trail.record(draft).await?;
    Ok(Json(json!({ "recorded": true })))
}

impl super::ControlClient {
    /// `GET /kernel/commands`: the REPL's commands the kernel offers, in `/help` order (spec
    /// 3.3 §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the error
    /// the kernel answered: `403` for a device token, `400` when the kernel offers none.
    pub async fn commands(&self) -> Result<Vec<CommandInfo>> {
        let answer: CommandsResponse =
            self.call(Method::GET, paths::COMMANDS.to_string(), None::<&()>).await?;
        Ok(answer.commands)
    }

    /// `POST /kernel/commands/run`: runs `/name args` in the caller's session `session`, by
    /// the REPL's code on the kernel's harness; returns what it printed (spec 3.3 §5.2).
    ///
    /// # Errors
    ///
    /// As [`commands`](Self::commands), and `404` for a command the kernel does not offer,
    /// `409` for one that changes something while everything is stopped, or the command's
    /// error.
    pub async fn run_command(&self, name: &str, args: &str, session: &str) -> Result<String> {
        let body = RunCommandRequest {
            name: name.to_string(),
            args: args.to_string(),
            session_id: session.to_string(),
        };
        let answer: RunCommandResponse =
            self.call(Method::POST, paths::COMMAND_RUN.to_string(), Some(&body)).await?;
        Ok(answer.text)
    }

    /// `GET /kernel/rules/always-allow/{id}`: the words of "Always allow" for the caller's
    /// waiting decision `id`, or `None` when no standing rule applies (spec 3.3 §8.2).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the error
    /// the kernel answered: `403` for a device token, `409` for a decision no longer waiting.
    pub async fn always_allow_offer(&self, id: &str) -> Result<Option<String>> {
        let path = paths::ALWAYS_ALLOW.replace("{id}", &encode(id));
        match self.call::<AlwaysAllowResponse>(Method::GET, path, None::<&()>).await {
            Ok(answer) => Ok(Some(answer.label)),
            Err(error) if error.category() == ErrorCategory::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// `POST /kernel/rules/always-allow/{id}/prepare`: makes the standing rule "Always
    /// allow" creates for the waiting decision `id`, before it is answered; returns its
    /// words (spec 3.3 §8.2).
    ///
    /// # Errors
    ///
    /// As [`always_allow_offer`](Self::always_allow_offer), and `400` when no rule applies.
    pub async fn prepare_always_allow(&self, id: &str) -> Result<String> {
        let path = paths::ALWAYS_ALLOW_PREPARE.replace("{id}", &encode(id));
        let answer: AlwaysAllowResponse = self.call(Method::POST, path, Some(&())).await?;
        Ok(answer.label)
    }

    /// `POST /kernel/rules/always-allow/{id}/grant`: grants the rule prepared for the
    /// decision `id` once it was allowed (spec 3.3 §8.2).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the error
    /// the kernel answered: `403` for a device token, `409` for a decision not allowed,
    /// `404` when no rule was prepared for it.
    pub async fn grant_always_allow(&self, id: &str) -> Result<()> {
        let path = paths::ALWAYS_ALLOW_GRANT.replace("{id}", &encode(id));
        let _: serde_json::Value = self.call(Method::POST, path, Some(&())).await?;
        Ok(())
    }

    /// `GET /kernel/sessions/{session}/mode`: the mode of the caller's session `session`, as
    /// the harness names it, or `None` when the kernel keeps no modes (spec 3.3 §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the error
    /// the kernel answered: `403` for a device token, `400` when the kernel offers no
    /// commands.
    pub async fn session_mode(&self, session: &str) -> Result<Option<String>> {
        let path = paths::SESSION_MODE.replace("{session}", &encode(session));
        match self.call::<SessionModeResponse>(Method::GET, path, None::<&()>).await {
            Ok(answer) => Ok(Some(answer.mode)),
            Err(error) if error.category() == ErrorCategory::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// `POST /kernel/surface/records`: writes `record`, one record of a surface engine in this
    /// process, on the kernel's signed log, with the caller as its actor (spec 3.3 §11).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the error
    /// the kernel answered: `403` for a device token, `400` for an action that is not a
    /// surface's, `404` for an answer to a decision that is not the caller's, or the log's
    /// error.
    pub async fn record_surface(&self, record: &SurfaceRecord) -> Result<()> {
        let path = paths::SURFACE_RECORDS.to_string();
        let _: serde_json::Value = self.call(Method::POST, path, Some(record)).await?;
        Ok(())
    }
}
