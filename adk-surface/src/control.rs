//! The control-plane source (spec 3.3 §6): a kernel in another process or on another
//! machine, through `adk_kernel::control::ControlClient`.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, AuditRecorder, AuditTrail};
use adk_core::{
    ApprovalAction, ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ErrorCategory,
    Event, RunCheckpointRef, RunState,
};
use adk_harness::HarnessMode;
use adk_kernel::KernelEvent;
use adk_kernel::control::api::{ApprovalItem, AttachRequest, LedgerParams, SpawnRequest};
use adk_kernel::control::{ControlClient, SurfaceRecord};
use adk_kernel::proc::LedgerEntry;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde_json::json;
use tokio::sync::watch;

use crate::error::{Result, SurfaceError};
use crate::inbox::Inbox;
use crate::origin::Caller;
use crate::source::{AgentState, AlwaysAllowOffer, RuleCommit, Source, SpendToday};

/// How long the feed waits for a bus event before it reads the session again anyway, while a
/// process runs (its turn writes the session between bus events) or the bus is unreachable.
const QUIET_REREAD: Duration = Duration::from_secs(15);

/// How long the feed waits for a bus event while no process runs and the bus is connected:
/// nothing then changes a session without a bus event, and every session read is an attach
/// the kernel records (spec 3.2 §9.3), so an idle surface reads only this often.
const QUIET_IDLE: Duration = Duration::from_secs(300);

/// What the one subscription to the kernel's bus knows, shared by every feed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Bus {
    /// Moves on every bus event, and when the stream connects or drops.
    events: u64,
    /// The stream is connected.
    live: bool,
    /// Processes running or queued, as the bus last reported them.
    running: usize,
}

/// The engine's source over a kernel's control plane (spec 3.3 §6, spec 3.2 §7.11): a
/// terminal or a home attached to a kernel elsewhere, over its socket, named pipe or HTTP.
///
/// Sessions, processes, the ledger and the kill switch go through `ControlClient`, so the
/// caller is whoever the client authenticates as (the OS user on the socket, a bearer token
/// over HTTP), and the kernel records each session read as `kernel.control`. The feed re-reads
/// a session when the kernel's event bus reports a change; with no event, every 15 seconds
/// while a process runs or the bus is unreachable, and every 5 minutes otherwise.
///
/// Decisions come from the approvals inbox beside the kernel routes, read in full —
/// `GET /api/approvals?session=`, `GET /api/approvals/{id}`, `POST /api/approvals/{id}/decide`
/// — over the same socket or HTTP endpoint, so cards carry their risk, agent and earlier
/// decisions, "Edit" sends its arguments, and a decision answered elsewhere says who
/// answered. The inbox gets the client's bearer token
/// ([`ControlClient::bearer_token`]); [`with_bearer_token`](Self::with_bearer_token) replaces
/// it for the inbox alone. Every inbox call gives up after 30 seconds, as the client's calls
/// do. Over a named pipe (Windows) the source keeps to `ControlClient`'s
/// pending items, which carry none of these: every decision counts as high risk, "Edit" is
/// not offered, and a decision answered elsewhere is no longer found.
///
/// "Always allow" is offered where the kernel's command host makes a standing rule of the
/// call (`GET /api/kernel/rules/always-allow/{id}`, spec 3.3 §5.2): the rule is prepared
/// while the decision waits and granted once the yes is recorded, as in process. A paired
/// device's token reaches none of those routes, so a remote terminal offers Allow once, Edit
/// and Deny.
///
/// The session's mode is read from the kernel (`GET /api/kernel/sessions/{id}/mode`) and set
/// by the REPL's `/mode`, which the kernel runs over its harness (spec 3.3 §5.2), so the
/// Permissions keys and `/mode` work on a thread that has no message yet. The engine's
/// records reach the kernel's signed log through [`audit_trail`](Self::audit_trail).
///
/// # Example
///
/// ```rust,no_run
/// use adk_kernel::control::ControlClient;
/// use adk_surface::ControlSource;
///
/// # async fn example(home: &std::path::Path) -> adk_surface::Result<()> {
/// let client = ControlClient::connect_local(home).await?.expect("a kernel is running");
/// let source = ControlSource::new(client, "adk-rust", "adk-rust");
/// # let _ = source;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ControlSource {
    client: ControlClient,
    app: String,
    target: String,
    daily_limit: Option<u64>,
    changes: Arc<std::sync::OnceLock<watch::Receiver<Bus>>>,
    inbox: Option<Inbox>,
    /// "Always allow" as the kernel offers it: the engine asks every time it draws a card.
    offers: Arc<std::sync::Mutex<Offers>>,
}

/// "Always allow" as a kernel offers it, by decision: the version asked at, and the words.
type Offers = BTreeMap<String, (u64, Option<String>)>;

impl fmt::Debug for ControlSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlSource")
            .field("endpoint", &self.client.endpoint())
            .field("app", &self.app)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl ControlSource {
    /// A source over `client` for the sessions of `app_name`, starting turns as processes of
    /// `target`.
    pub fn new(
        client: ControlClient,
        app_name: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        let inbox =
            Inbox::for_endpoint(&client.endpoint(), client.bearer_token().map(str::to_owned));
        Self {
            client,
            app: app_name.into(),
            target: target.into(),
            daily_limit: None,
            changes: Arc::new(std::sync::OnceLock::new()),
            inbox,
            offers: Arc::default(),
        }
    }

    /// Sends `token` as a bearer token to the approvals inbox instead of the client's own
    /// token, which [`new`](Self::new) already passes on.
    #[must_use]
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.inbox = self.inbox.map(|inbox| inbox.with_token(token.into()));
        self
    }

    /// The daily spending limit in micro-USD, which the control plane does not report.
    #[must_use]
    pub fn with_daily_limit(mut self, micro_usd: u64) -> Self {
        self.daily_limit = Some(micro_usd);
        self
    }

    /// The kernel's signed log, for the engine over this source: each record the engine
    /// writes — every answer with the surface and how it was given, Stop all, Resume all, a
    /// mode change, a message sent — goes to `POST /api/kernel/surface/records`, and the kernel
    /// writes it with the caller as its actor (spec 3.3 §11). Only the person reaches that
    /// route: give it to the engine of a caller the kernel admits as the operator, never a
    /// paired device's, whose answers the kernel records itself.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use adk_kernel::control::ControlClient;
    /// use adk_surface::{ControlSource, SurfaceEngine};
    ///
    /// # async fn example(home: &std::path::Path) -> adk_surface::Result<()> {
    /// let client = ControlClient::connect_local(home).await?.expect("a kernel is running");
    /// let source = ControlSource::new(client, "adk-rust", "adk-rust");
    /// let engine = SurfaceEngine::builder(Arc::new(source.clone())).audit(source.audit_trail()).build();
    /// # let _ = engine;
    /// # Ok(())
    /// # }
    /// ```
    pub fn audit_trail(&self) -> AuditTrail {
        AuditTrail::new(Arc::new(KernelRecorder { client: self.client.clone() }))
    }

    /// What the kernel's bus reports, from one subscription shared by every feed.
    fn changes(&self) -> watch::Receiver<Bus> {
        self.changes
            .get_or_init(|| {
                let (sender, receiver) = watch::channel(Bus::default());
                let client = self.client.clone();
                let app = self.app.clone();
                tokio::spawn(async move {
                    let mut cursor = None;
                    loop {
                        match client.events(cursor).await {
                            Ok(mut events) => {
                                // The processes running now; the bus keeps the count from here.
                                let mut running: Option<HashMap<String, RunState>> = client
                                    .processes(&Default::default())
                                    .await
                                    .ok()
                                    .map(|processes| {
                                        processes
                                            .into_iter()
                                            .filter(|process| {
                                                process.run.app_name == app && runs(process.run.state)
                                            })
                                            .map(|process| (process.meta.pid, process.run.state))
                                            .collect()
                                    });
                                let count = |running: &Option<HashMap<String, RunState>>| {
                                    // Unknown counts as running: the feed keeps re-reading.
                                    running.as_ref().map_or(1, HashMap::len)
                                };
                                sender.send_modify(|bus| {
                                    bus.events += 1;
                                    bus.live = true;
                                    bus.running = count(&running);
                                });
                                while let Some(Ok(record)) = events.next().await {
                                    cursor = Some(record.cursor);
                                    if let (KernelEvent::Process { pid, to, .. }, Some(running)) =
                                        (&record.event, running.as_mut())
                                    {
                                        if runs(*to) {
                                            running.insert(pid.clone(), *to);
                                        } else {
                                            running.remove(pid);
                                        }
                                    }
                                    sender.send_modify(|bus| {
                                        bus.events += 1;
                                        bus.running = count(&running);
                                    });
                                    if sender.is_closed() {
                                        return;
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::debug!(error = %error, "the kernel's event stream is unavailable");
                            }
                        }
                        sender.send_modify(|bus| {
                            bus.events += 1;
                            bus.live = false;
                        });
                        if sender.is_closed() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                });
                receiver
            })
            .clone()
    }

    async fn processes(&self, user: &str) -> Result<Vec<adk_kernel::Process>> {
        let mut processes: Vec<_> = self
            .client
            .processes(&Default::default())
            .await?
            .into_iter()
            .filter(|process| process.run.user_id == user && process.run.app_name == self.app)
            .collect();
        processes.sort_by_key(|process| std::cmp::Reverse(process.run.updated_at));
        Ok(processes)
    }
}

/// Whether a process in `state` is running or about to.
fn runs(state: RunState) -> bool {
    matches!(state, RunState::Running | RunState::Queued)
}

/// Whether an error of `category` may pass: the kernel was out of reach, busy or failed, not
/// refusing.
fn passing(category: ErrorCategory) -> bool {
    matches!(
        category,
        ErrorCategory::Unavailable
            | ErrorCategory::Timeout
            | ErrorCategory::Internal
            | ErrorCategory::RateLimited
            | ErrorCategory::ResourceExhausted
            | ErrorCategory::Cancelled
    )
}

/// How many decisions' "Always allow" offers a source keeps.
const OFFERS: usize = 256;

/// The engine's records, written on the kernel's signed log over its control plane.
struct KernelRecorder {
    client: ControlClient,
}

#[async_trait]
impl AuditRecorder for KernelRecorder {
    fn digest(&self, value: &serde_json::Value) -> String {
        adk_core::json_digest(value)
    }

    async fn record(&self, draft: AuditRecordDraft) -> adk_core::Result<()> {
        // The kernel names the actor itself, as the caller it admitted.
        let AuditRecordDraft { kind, action, decision, ext, .. } = draft;
        let mut record = SurfaceRecord::new(kind, action);
        if let Some(decision) = decision {
            record = record.with_decision(decision);
        }
        for (key, value) in ext {
            record = record.with_ext(key, value);
        }
        Ok(self.client.record_surface(&record).await?)
    }
}

/// The standing rule the kernel prepared for a decision, granted once its yes is recorded.
struct KernelRule {
    client: ControlClient,
    decision: String,
}

#[async_trait]
impl RuleCommit for KernelRule {
    async fn commit(self: Box<Self>) -> Result<()> {
        Ok(self.client.grant_always_allow(&self.decision).await?)
    }
}

/// An inbox item, over a named pipe, as a request: risk high, as the item does not say.
fn request_of(item: ApprovalItem) -> Result<ApprovalRequest> {
    let policy = serde_json::to_value(ApprovalPolicy::new([]))
        .map_err(|error| SurfaceError::Invalid(error.to_string()))?;
    let value = json!({
        "id": item.id,
        "scope": item.scope,
        "fingerprint": "",
        "toolName": item.tool_name,
        "argsRedacted": item.args_redacted,
        "summary": item.summary,
        "risk": ApprovalRisk::High,
        "createdAt": item.created_at,
        "expiresAt": item.expires_at,
        "allowedActions": item
            .allowed_actions
            .into_iter()
            .filter(|action| *action != ApprovalAction::Edit)
            .collect::<Vec<_>>(),
        "policy": policy,
        "runId": item.run_id,
        "version": item.version,
        "state": item.state,
    });
    serde_json::from_value(value).map_err(|error| SurfaceError::Invalid(error.to_string()))
}

#[async_trait]
impl Source for ControlSource {
    fn app_name(&self) -> &str {
        &self.app
    }

    async fn transcript(&self, user: &str, session: &str) -> Result<Option<Vec<Event>>> {
        let request = AttachRequest { user_id: Some(user.to_string()), after: None };
        match self.client.attach(session, &request).await {
            Ok(attached) => Ok(Some(attached.transcript)),
            Err(error) => {
                let error = adk_core::AdkError::from(error);
                match error.category {
                    ErrorCategory::NotFound => Ok(None),
                    _ => Err(SurfaceError::Source(error)),
                }
            }
        }
    }

    async fn changed(&self, max: Duration) {
        let mut changes = self.changes();
        // Only what happens from now on: a clone starts at the version the shared receiver
        // saw first, which every event since has moved past.
        changes.mark_unchanged();
        let bus = *changes.borrow();
        let wait =
            if bus.live && bus.running == 0 { QUIET_IDLE.max(max) } else { max.max(QUIET_REREAD) };
        let change = async {
            if changes.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = change => {}
        }
    }

    async fn requests(
        &self,
        _caller: &Caller,
        session: Option<&str>,
        open_only: bool,
    ) -> Result<Vec<ApprovalRequest>> {
        if let Some(inbox) = &self.inbox {
            return inbox.list(&self.app, session, open_only).await;
        }
        self.client
            .approvals()
            .await?
            .into_iter()
            .filter(|item| session.is_none_or(|session| item.scope.session_id == session))
            .map(request_of)
            .collect()
    }

    async fn request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        if let Some(inbox) = &self.inbox {
            return inbox.get(id).await;
        }
        Ok(self
            .client
            .approvals()
            .await?
            .into_iter()
            .find(|item| item.id == id)
            .map(request_of)
            .transpose()?)
    }

    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> Result<ApprovalRequest> {
        if let Some(inbox) = &self.inbox {
            return inbox.decide(id, expected_version, &decision).await;
        }
        if decision.kind == ApprovalAction::Edit {
            return Err(SurfaceError::unsupported("editing a call over the control plane"));
        }
        let item = self
            .client
            .decide(id, expected_version, decision.kind, decision.reason.as_deref())
            .await?;
        let mut request = request_of(item)?;
        // The control plane answers with the request but not its decisions: record this one,
        // so the card says who answered.
        request.decisions.push(decision);
        Ok(request)
    }

    async fn always_allow_offer(&self, request: &ApprovalRequest) -> Option<AlwaysAllowOffer> {
        let known = |offers: &Offers| {
            offers
                .get(&request.id)
                .filter(|(version, _)| *version == request.version)
                .map(|(_, label)| label.clone())
        };
        if let Some(label) = known(&self.offers.lock().unwrap_or_else(|e| e.into_inner())) {
            return label.map(|label| AlwaysAllowOffer { label });
        }
        // A kernel without the route, or a device's token, offers none, for good; a kernel
        // out of reach for a moment is asked again on the next draw.
        let label = match self.client.always_allow_offer(&request.id).await {
            Ok(label) => label,
            Err(error) if passing(error.category()) => {
                tracing::debug!(error = %error, decision.id = %request.id, "Always allow could not be looked up");
                return None;
            }
            Err(_) => None,
        };
        let mut offers = self.offers.lock().unwrap_or_else(|e| e.into_inner());
        if offers.len() >= OFFERS {
            offers.clear();
        }
        offers.insert(request.id.clone(), (request.version, label.clone()));
        label.map(|label| AlwaysAllowOffer { label })
    }

    async fn prepare_always_allow(
        &self,
        _caller: &Caller,
        request: &ApprovalRequest,
    ) -> Result<Box<dyn RuleCommit>> {
        self.client.prepare_always_allow(&request.id).await?;
        Ok(Box::new(KernelRule { client: self.client.clone(), decision: request.id.clone() }))
    }

    async fn agents(&self, user: &str) -> Result<Vec<AgentState>> {
        let mut latest: BTreeMap<String, AgentState> = BTreeMap::new();
        for process in self.processes(user).await? {
            let run = &process.run;
            if latest.contains_key(&run.target) {
                continue;
            }
            let waiting_on_decision =
                matches!(run.checkpoint, Some(RunCheckpointRef::AwaitingApproval { .. }));
            latest.insert(
                run.target.clone(),
                AgentState {
                    name: run.target.clone(),
                    session_id: run.session_id.clone(),
                    state: run.state,
                    waiting_on_decision,
                    wake_at: (run.state == RunState::Waiting && !waiting_on_decision)
                        .then_some(run.not_before)
                        .flatten(),
                    suspended: process.meta.suspended_by.is_some(),
                },
            );
        }
        // A removed agent leaves the roster. One listing carries every agent's state, removed
        // ones included, so no agent is looked up on its own (spec 3.3 §5.7); a kernel that
        // keeps no agents lists none.
        let gone: Vec<String> = self
            .client
            .agents()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|view| {
                matches!(
                    view.agent.state,
                    adk_kernel::agents::AgentRecordState::Removing
                        | adk_kernel::agents::AgentRecordState::Removed
                )
            })
            .map(|view| view.agent.id)
            .collect();
        latest.retain(|name, _| !gone.contains(name));
        Ok(latest.into_values().collect())
    }

    async fn agent_threads(&self, user: &str, agent: &str) -> Result<Vec<(String, DateTime<Utc>)>> {
        let mut threads: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
        for process in self.processes(user).await? {
            if process.run.target != agent {
                continue;
            }
            if let Some(session) = process.run.session_id.clone() {
                let at = threads.entry(session).or_insert(process.run.updated_at);
                *at = (*at).max(process.run.updated_at);
            }
        }
        Ok(threads.into_iter().collect())
    }

    // The terminal changes agents through `/agent` on the kernel; this source reads them for
    // the roster (spec 3.3 §5.7, §10 L1).
    async fn agent_definitions(&self, user: &str) -> Result<Vec<crate::agents::AgentDetail>> {
        let views = match self.client.agents().await {
            Ok(views) => views,
            // A kernel without agents of a person's own lists none.
            Err(error)
                if matches!(
                    error.category(),
                    adk_core::ErrorCategory::NotFound | adk_core::ErrorCategory::Unsupported
                ) =>
            {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.into()),
        };
        let offset = *chrono::Local::now().offset();
        Ok(views
            .into_iter()
            .map(|view| {
                crate::agents::AgentDetail::of(
                    &view.agent,
                    view.cant_start,
                    user,
                    "ADK-Rust",
                    offset,
                )
            })
            .collect())
    }

    async fn agent(&self, user: &str, id: &str) -> Result<Option<crate::agents::AgentDetail>> {
        // A removed agent has no settings; its threads are read through "Removed agents".
        Ok(self.agent_definitions(user).await?.into_iter().find(|detail| {
            detail.id == id
                && !matches!(
                    detail.state,
                    crate::agents::AgentLife::Removed | crate::agents::AgentLife::Refused
                )
        }))
    }

    async fn busy(&self, user: &str, session: &str) -> Result<bool> {
        Ok(self.processes(user).await?.iter().any(|process| {
            process.run.session_id.as_deref() == Some(session) && runs(process.run.state)
        }))
    }

    async fn waiting_on_decision(&self, user: &str, session: &str) -> Result<bool> {
        Ok(self.processes(user).await?.iter().any(|process| {
            process.run.session_id.as_deref() == Some(session)
                && process.run.state == RunState::Waiting
                && matches!(process.run.checkpoint, Some(RunCheckpointRef::AwaitingApproval { .. }))
        }))
    }

    async fn start_turn(&self, _caller: &Caller, session: &str, text: &str) -> Result<()> {
        let request = SpawnRequest::new(self.target.clone(), json!({ "text": text }))
            .with_session_id(session);
        self.client.spawn(&request).await?;
        Ok(())
    }

    async fn request_call(
        &self,
        caller: &Caller,
        session: &str,
        text: &str,
        call: adk_core::RequestedCall,
    ) -> Result<()> {
        // The thread's agent holds the call in its own target (spec 3.3 §7.4 rule 5): the
        // target of the session's latest run, else this terminal's.
        let target = self
            .agent_of_session(caller.user_id(), session)
            .await?
            .unwrap_or_else(|| self.target.clone());
        let request = SpawnRequest::new(target, json!({ "text": text, "requestedCall": call }))
            .with_session_id(session);
        self.client.spawn(&request).await?;
        Ok(())
    }

    async fn stop_all(&self, _caller: &Caller, reason: &str) -> Result<()> {
        self.client.freeze(Some(reason)).await?;
        Ok(())
    }

    async fn resume_all(&self, _caller: &Caller) -> Result<()> {
        self.client.unfreeze().await?;
        Ok(())
    }

    async fn stopped(&self) -> Result<bool> {
        Ok(self.client.health().await?.frozen)
    }

    async fn mode(&self, _user: &str, session: Option<&str>) -> Result<Option<HarnessMode>> {
        let Some(session) = session else {
            return Ok(None);
        };
        match self.client.session_mode(session).await {
            Ok(mode) => Ok(mode.and_then(|mode| mode.parse().ok())),
            // A paired device's token, or a kernel that offers no commands, reads no mode.
            Err(error)
                if matches!(
                    error.category(),
                    ErrorCategory::Forbidden
                        | ErrorCategory::Unauthorized
                        | ErrorCategory::InvalidInput
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    // The kernel's `/mode` opens a new thread of the person it serves.
    fn opens_sessions(&self) -> bool {
        true
    }

    // The REPL's `/mode`, which the kernel runs over its harness, sets it (spec 3.3 §5.2).
    async fn set_mode(&self, _caller: &Caller, session: &str, mode: HarnessMode) -> Result<()> {
        let word = match mode {
            HarnessMode::ReadOnly => "read-only",
            other => other.as_str(),
        };
        self.client.run_command("mode", word, session).await?;
        Ok(())
    }

    async fn agent_of_session(&self, user: &str, session: &str) -> Result<Option<String>> {
        Ok(self
            .processes(user)
            .await?
            .into_iter()
            .find(|process| process.run.session_id.as_deref() == Some(session))
            .map(|process| process.run.target))
    }

    async fn spend(&self, user: &str) -> Result<Option<SpendToday>> {
        let spent =
            self.processes(user).await?.iter().map(|process| process.view.spend_today).sum();
        Ok(Some(SpendToday { spent, limit: self.daily_limit }))
    }

    async fn ledger(&self, since: DateTime<Utc>) -> Result<Vec<LedgerEntry>> {
        let mut params = LedgerParams { from: Some(since), ..LedgerParams::default() };
        let mut entries = Vec::new();
        loop {
            let page = self.client.ledger(&params).await?;
            entries.extend(page.entries);
            match page.next {
                Some(next) => params.after = Some(next),
                None => break,
            }
        }
        Ok(entries)
    }
}
