//! The control-plane source (spec 3.3 §6): a kernel in another process or on another
//! machine, through `adk_kernel::control::ControlClient`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalAction, ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ErrorCategory,
    Event, RunCheckpointRef, RunState,
};
use adk_kernel::control::ControlClient;
use adk_kernel::control::api::{ApprovalItem, AttachRequest, LedgerParams, SpawnRequest};
use adk_kernel::proc::LedgerEntry;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde_json::json;
use tokio::sync::watch;

use crate::error::{Result, SurfaceError};
use crate::inbox::Inbox;
use crate::origin::Caller;
use crate::source::{AgentState, Source, SpendToday};

/// How long the feed waits for a bus event before it reads the session again anyway.
const QUIET_REREAD: Duration = Duration::from_secs(15);

/// The engine's source over a kernel's control plane (spec 3.3 §6, spec 3.2 §7.11): a
/// terminal or a home attached to a kernel elsewhere, over its socket, named pipe or HTTP.
///
/// Sessions, processes, the ledger and the kill switch go through `ControlClient`, so the
/// caller is whoever the client authenticates as (the OS user on the socket, a bearer token
/// over HTTP), and the kernel records each session read as `kernel.control`. The feed re-reads
/// a session when the kernel's event bus reports a change, not on a timer.
///
/// Decisions come from the approvals inbox beside the kernel routes, read in full —
/// `GET /api/approvals?session=`, `GET /api/approvals/{id}`, `POST /api/approvals/{id}/decide`
/// — over the same socket or HTTP endpoint, so cards carry their risk, agent and earlier
/// decisions, "Edit" sends its arguments, and a decision answered elsewhere says who
/// answered. Pass the HTTP control plane's token to [`with_bearer_token`](Self::with_bearer_token)
/// as well as to the client. Over a named pipe (Windows) the source keeps to `ControlClient`'s
/// pending items, which carry none of these: every decision counts as high risk, "Edit" is
/// not offered, and a decision answered elsewhere is no longer found.
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
    changes: Arc<std::sync::OnceLock<watch::Receiver<u64>>>,
    inbox: Option<Inbox>,
}

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
        let inbox = Inbox::for_endpoint(&client.endpoint());
        Self {
            client,
            app: app_name.into(),
            target: target.into(),
            daily_limit: None,
            changes: Arc::new(std::sync::OnceLock::new()),
            inbox,
        }
    }

    /// Sends `token` as a bearer token to the approvals inbox, as the client sends it to the
    /// kernel routes.
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

    /// A counter that moves on every bus event, from one subscription shared by every feed.
    fn changes(&self) -> watch::Receiver<u64> {
        self.changes
            .get_or_init(|| {
                let (sender, receiver) = watch::channel(0_u64);
                let client = self.client.clone();
                tokio::spawn(async move {
                    let mut cursor = None;
                    loop {
                        match client.events(cursor).await {
                            Ok(mut events) => {
                                while let Some(Ok(record)) = events.next().await {
                                    cursor = Some(record.cursor);
                                    if sender.send(record.cursor).is_err() {
                                        return;
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::debug!(error = %error, "the kernel's event stream is unavailable");
                            }
                        }
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
        let wait = max.max(QUIET_REREAD);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = changes.changed() => {}
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
        Ok(latest.into_values().collect())
    }

    async fn busy(&self, user: &str, session: &str) -> Result<bool> {
        Ok(self.processes(user).await?.iter().any(|process| {
            process.run.session_id.as_deref() == Some(session)
                && matches!(process.run.state, RunState::Running | RunState::Queued)
        }))
    }

    async fn start_turn(&self, _caller: &Caller, session: &str, text: &str) -> Result<()> {
        let request = SpawnRequest::new(self.target.clone(), json!({ "text": text }))
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
