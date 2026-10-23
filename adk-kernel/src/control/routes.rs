//! The control plane's route handlers (spec 3.2 §7.11), over a [`ControlBackend`]: the
//! kernel, or a test double.

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalService, DurableRunStore, ErrorCategory, ErrorComponent, Event,
    GovernanceControl, NewRun, RequestContext, RunFilter, ToolConfirmationRequest, held_tool_calls,
};
use adk_server::rest::FreezeDelegate;
use adk_session::{GetRequest, SessionService};
use async_trait::async_trait;
use axum::extract::{FromRequestParts, Path as UrlPath, Query, State};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::json;

use super::api::{
    AgendaItemResponse, AgendaQuery, AgendaResponse, AttachRequest, AttachResponse, EventsQuery,
    FreezeRequest, FreezeResponse, KillRequest, NiceRequest, ProcessResponse, ProcessesQuery,
    ProcessesResponse, RejectRequest, ScheduleRequest, SpawnRequest,
};
use super::{ControlTransport, paths};
use crate::agenda::Agenda;
use crate::audit;
use crate::bus::EventSubscriber;
use crate::error::{KernelError, Result, UNAUTHORIZED_CODE};
use crate::kernel::{Kernel, KernelHealth};
use crate::process::{Process, ProcessKind, ProcessSpec};
use crate::sched::QueueClass;
use crate::stores::ProcessFilter;

/// The scope the socket and a loopback bind grant the OS user; the approvals inbox
/// `adk-rust up` serves routes held calls to this group, as `adk-rust serve` does.
pub(crate) const OPERATOR_SCOPE: &str = "operator";

/// What the routes read and change: the kernel, or a test double.
#[async_trait]
pub(crate) trait ControlBackend: Send + Sync + 'static {
    fn app_name(&self) -> &str;
    fn health(&self) -> KernelHealth;
    fn governance(&self) -> &GovernanceControl;
    fn run_store(&self) -> &Arc<dyn DurableRunStore>;
    fn sessions(&self) -> Option<&Arc<dyn SessionService>>;
    fn approvals(&self) -> Option<&Arc<dyn ApprovalService>>;
    fn audit_trail(&self) -> Option<&AuditTrail>;
    /// The targets processes may run, for the message refusing another.
    fn targets(&self) -> Vec<String>;
    fn has_target(&self, target: &str) -> bool;
    async fn spawn(&self, run: NewRun, spec: ProcessSpec) -> Result<Process>;
    async fn ps(&self, filter: ProcessFilter) -> Result<Vec<Process>>;
    async fn process(&self, pid: &str) -> Result<Option<Process>>;
    async fn kill(&self, pid: &str, reason: &str) -> Result<()>;
    /// Moves the process and returns the moved one, under its new pid.
    async fn nice(&self, pid: &str, queue: QueueClass) -> Result<Process>;
    async fn suspend(&self, pid: &str) -> Result<()>;
    async fn resume(&self, pid: &str) -> Result<()>;
    /// Freezes and returns how many running processes this replica parked.
    async fn freeze(&self, reason: &str, by: AuditActor) -> Result<usize>;
    async fn unfreeze(&self, by: AuditActor) -> Result<()>;
    /// The bus from just after `after`, or from now on.
    async fn events(&self, after: Option<u64>) -> Result<EventSubscriber>;
    /// The agenda (spec 3.2 §7.5); `None` on a backend without one.
    fn agenda(&self) -> Option<Agenda> {
        None
    }
}

#[async_trait]
impl ControlBackend for Kernel {
    fn app_name(&self) -> &str {
        &self.config().app_name
    }

    fn health(&self) -> KernelHealth {
        Kernel::health(self)
    }

    fn governance(&self) -> &GovernanceControl {
        &self.config().governance
    }

    fn run_store(&self) -> &Arc<dyn DurableRunStore> {
        self.processes().run_store()
    }

    fn sessions(&self) -> Option<&Arc<dyn SessionService>> {
        self.session_service()
    }

    fn approvals(&self) -> Option<&Arc<dyn ApprovalService>> {
        self.approval_service()
    }

    fn audit_trail(&self) -> Option<&AuditTrail> {
        Kernel::audit_trail(self)
    }

    fn targets(&self) -> Vec<String> {
        Kernel::targets(self)
    }

    fn has_target(&self, target: &str) -> bool {
        Kernel::has_target(self, target)
    }

    async fn spawn(&self, run: NewRun, spec: ProcessSpec) -> Result<Process> {
        self.processes().spawn(run, spec).await
    }

    async fn ps(&self, filter: ProcessFilter) -> Result<Vec<Process>> {
        self.processes().ps(filter).await
    }

    async fn process(&self, pid: &str) -> Result<Option<Process>> {
        self.processes().get(pid).await
    }

    async fn kill(&self, pid: &str, reason: &str) -> Result<()> {
        self.processes().kill(pid, reason).await
    }

    async fn nice(&self, pid: &str, queue: QueueClass) -> Result<Process> {
        self.processes().nice(pid, queue).await
    }

    async fn suspend(&self, pid: &str) -> Result<()> {
        self.processes().suspend(pid).await
    }

    async fn resume(&self, pid: &str) -> Result<()> {
        self.processes().resume(pid).await
    }

    async fn freeze(&self, reason: &str, by: AuditActor) -> Result<usize> {
        // The delegate's freeze is `Kernel::freeze` with the count the response reports.
        Ok(FreezeDelegate::freeze(self, reason, by).await?.parked_runs)
    }

    async fn unfreeze(&self, by: AuditActor) -> Result<()> {
        Kernel::unfreeze(self, by).await
    }

    async fn events(&self, after: Option<u64>) -> Result<EventSubscriber> {
        match after {
            Some(cursor) => self.bus().subscribe_after(cursor).await,
            None => Ok(Kernel::events(self)),
        }
    }

    fn agenda(&self) -> Option<Agenda> {
        Some(Kernel::agenda(self).clone())
    }
}

/// The routes over `backend`, relative to `/api`.
pub(crate) fn routes(backend: Arc<dyn ControlBackend>) -> Router {
    Router::new()
        .route(paths::HEALTH, get(health))
        .route(paths::PROCESSES, get(processes).post(spawn))
        .route(paths::KILL, post(kill))
        .route(paths::NICE, post(nice))
        .route(paths::SUSPEND, post(suspend))
        .route(paths::RESUME, post(resume))
        .route(paths::EVENTS, get(events))
        .route(paths::ATTACH, post(attach))
        .route(paths::FREEZE, post(freeze))
        .route(paths::UNFREEZE, post(unfreeze))
        .route(paths::AGENDA, get(agenda))
        .route(paths::AGENDA_APPROVE, post(agenda_approve))
        .route(paths::AGENDA_REJECT, post(agenda_reject))
        .route(paths::AGENDA_SCHEDULE, post(agenda_schedule))
        .with_state(backend)
}

type Backend = State<Arc<dyn ControlBackend>>;

/// The authenticated caller of a route (K-I16).
pub(crate) struct Caller {
    context: RequestContext,
    transport: ControlTransport,
}

impl Caller {
    fn subject(&self) -> SubjectRef {
        SubjectRef::human(self.context.user_id.clone())
    }

    fn actor(&self) -> AuditActor {
        AuditActor::subject(self.subject())
    }

    /// Refuses a caller without the `operator` scope the routes that change the kernel need.
    fn require_operator(&self, action: &str) -> Result<()> {
        if self.context.scopes.iter().any(|scope| scope == OPERATOR_SCOPE) {
            return Ok(());
        }
        Err(KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Forbidden,
            UNAUTHORIZED_CODE,
            format!(
                "{} may not {action}: changing the kernel needs the `{OPERATOR_SCOPE}` scope, \
                 which the kernel's socket and a loopback bind grant",
                self.context.user_id
            ),
        )))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = KernelError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self> {
        let context = parts.extensions.get::<Option<RequestContext>>().cloned().flatten();
        let transport =
            parts.extensions.get::<ControlTransport>().copied().unwrap_or(ControlTransport::Http);
        match context {
            Some(context) => Ok(Self { context, transport }),
            None => Err(KernelError::Unauthorized {
                reason: "the kernel control plane needs an authenticated caller: connect over \
                         the kernel's socket as its OS user, or configure a \
                         RequestContextExtractor (a bearer token or SSO) for HTTP"
                    .into(),
            }),
        }
    }
}

/// Writes the `kernel.control` record of `action` on `target` by `caller` (spec 3.2 §9.3).
///
/// # Errors
///
/// The trail's error under `AuditFailureMode::Block`; the caller refuses the action.
async fn record(
    backend: &dyn ControlBackend,
    caller: &Caller,
    action: &str,
    target: Option<&str>,
    extra: &[(&str, serde_json::Value)],
) -> Result<()> {
    tracing::info!(
        control.action = action,
        control.subject = %caller.context.user_id,
        control.transport = ?caller.transport,
        control.target = target.unwrap_or_default(),
        "kernel control action"
    );
    let Some(trail) = backend.audit_trail() else {
        return Ok(());
    };
    let draft = AuditRecordDraft::new(
        AuditRecordKind::Other(audit::KERNEL_AUDIT_KIND.into()),
        audit::CONTROL,
    )
    .actor(caller.actor())
    .ext("subject", json!(caller.subject()))
    .ext("transport", json!(caller.transport))
    .ext("action", json!(action))
    .ext("target", json!(target));
    let draft = extra.iter().fold(draft, |draft, (key, value)| draft.ext(*key, value.clone()));
    trail.record(draft).await.map_err(KernelError::from)
}

/// The process `pid` after an action, or `NotFound` when it is gone.
async fn after_action(backend: &dyn ControlBackend, pid: &str) -> Result<Json<ProcessResponse>> {
    match backend.process(pid).await? {
        Some(process) => Ok(Json(ProcessResponse { process })),
        None => Err(KernelError::NotFound { pid: pid.to_string() }),
    }
}

async fn health(State(backend): Backend, _caller: Caller) -> Json<KernelHealth> {
    Json(backend.health())
}

async fn processes(
    State(backend): Backend,
    _caller: Caller,
    Query(query): Query<ProcessesQuery>,
) -> Result<Json<ProcessesResponse>> {
    let processes = backend.ps(query.to_filter()).await?;
    Ok(Json(ProcessesResponse { processes }))
}

async fn spawn(
    State(backend): Backend,
    caller: Caller,
    Json(request): Json<SpawnRequest>,
) -> Result<Json<ProcessResponse>> {
    caller.require_operator("spawn")?;
    if !backend.has_target(&request.target) {
        let targets = backend.targets();
        let runs = if targets.is_empty() { "none".to_string() } else { targets.join(", ") };
        return Err(KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::NotFound,
            crate::error::NOT_FOUND_CODE,
            format!("no target {} on this kernel; it runs {runs}", request.target),
        )));
    }
    let queue = request.queue.unwrap_or(QueueClass::Interactive);
    // The run is the caller's own, as `/api/runs` makes an authenticated submission.
    let mut run = NewRun::new(
        queue.as_str(),
        backend.app_name(),
        caller.context.user_id.clone(),
        request.target.clone(),
        request.input,
    )
    .with_authenticated_user();
    if let Some(session_id) = request.session_id {
        run = run.with_session_id(session_id);
    }
    let mut spec = ProcessSpec::new(request.kind.unwrap_or(ProcessKind::Interactive));
    if let Some(parent) = request.parent {
        spec = spec.with_parent(parent);
    }
    if let Some(wake) = request.wake {
        spec = spec.with_wake(wake);
    }
    for (key, value) in request.labels {
        spec = spec.with_label(key, value);
    }
    let process = backend.spawn(run, spec).await?;
    let pid = process.meta.pid.clone();
    let extra = [("pid", json!(pid))];
    if let Err(error) =
        record(backend.as_ref(), &caller, "spawn", Some(&request.target), &extra).await
    {
        // A process nobody recorded starting does not run.
        if let Err(cancel) = backend.kill(&pid, "its control record could not be written").await {
            tracing::error!(process.pid = %pid, error = %cancel, "could not cancel an unrecorded process");
        }
        return Err(error);
    }
    Ok(Json(ProcessResponse { process }))
}

async fn kill(
    State(backend): Backend,
    caller: Caller,
    UrlPath(pid): UrlPath<String>,
    Json(request): Json<KillRequest>,
) -> Result<Json<ProcessResponse>> {
    caller.require_operator("kill")?;
    record(backend.as_ref(), &caller, "kill", Some(&pid), &[]).await?;
    let reason = request.reason.unwrap_or_else(|| {
        format!("killed by {} over the kernel control plane", caller.context.user_id)
    });
    backend.kill(&pid, &reason).await?;
    after_action(backend.as_ref(), &pid).await
}

async fn nice(
    State(backend): Backend,
    caller: Caller,
    UrlPath(pid): UrlPath<String>,
    Json(request): Json<NiceRequest>,
) -> Result<Json<ProcessResponse>> {
    caller.require_operator("nice")?;
    record(backend.as_ref(), &caller, "nice", Some(&pid), &[]).await?;
    // The run store has no queue change, so the moved process has a new pid.
    let process = backend.nice(&pid, request.queue).await?;
    Ok(Json(ProcessResponse { process }))
}

async fn suspend(
    State(backend): Backend,
    caller: Caller,
    UrlPath(pid): UrlPath<String>,
) -> Result<Json<ProcessResponse>> {
    caller.require_operator("suspend")?;
    record(backend.as_ref(), &caller, "suspend", Some(&pid), &[]).await?;
    backend.suspend(&pid).await?;
    after_action(backend.as_ref(), &pid).await
}

async fn resume(
    State(backend): Backend,
    caller: Caller,
    UrlPath(pid): UrlPath<String>,
) -> Result<Json<ProcessResponse>> {
    caller.require_operator("resume")?;
    record(backend.as_ref(), &caller, "resume", Some(&pid), &[]).await?;
    backend.resume(&pid).await?;
    after_action(backend.as_ref(), &pid).await
}

async fn events(
    State(backend): Backend,
    _caller: Caller,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Response> {
    // A reconnecting `EventSource` sends the last id it saw; it wins over the query.
    let after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .or(query.after);
    let subscriber = backend.events(after).await?;
    let stream = subscriber.into_stream().map(|record| {
        let event = SseEvent::default().id(record.cursor.to_string()).event(record.event.name());
        Ok::<_, std::convert::Infallible>(event.json_data(&record).unwrap_or_else(|error| {
            tracing::warn!(error = %error, "kernel event could not be encoded");
            SseEvent::default().comment("an event could not be encoded")
        }))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()).into_response())
}

async fn attach(
    State(backend): Backend,
    caller: Caller,
    UrlPath(session_id): UrlPath<String>,
    Json(request): Json<AttachRequest>,
) -> Result<Json<AttachResponse>> {
    // A surface attaches to its caller's own sessions; the owner checks of `/api/runs` apply.
    let user_id = match request.user_id {
        Some(user) if user != caller.context.user_id => {
            return Err(KernelError::Unauthorized {
                reason: format!(
                    "{} cannot attach to a session of {user}: a surface attaches to its \
                     caller's own sessions",
                    caller.context.user_id
                ),
            });
        }
        Some(user) => user,
        None => caller.context.user_id.clone(),
    };
    record(backend.as_ref(), &caller, "attach", Some(&session_id), &[]).await?;
    let sessions = backend.sessions().ok_or_else(|| KernelError::Config {
        reason: "attach reads the kernel's sessions: build the kernel with \
                 KernelBuilder::session_service"
            .into(),
    })?;
    let session = sessions
        .get(GetRequest {
            app_name: backend.app_name().to_string(),
            user_id: user_id.clone(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    let events = session.events().all();
    let pending = pending_holds(&events);
    let transcript: Vec<Event> = events
        .into_iter()
        .filter(|event| request.after.is_none_or(|after| event.timestamp > after))
        .collect();
    let cursor = transcript.last().map(|event| event.timestamp).or(request.after);
    let process = session_process(backend.as_ref(), &user_id, &session_id).await?;
    Ok(Json(AttachResponse { session_id, transcript, cursor, pending, process }))
}

/// The calls `events` hold for a decision, for every agent that held one.
fn pending_holds(events: &[Event]) -> Vec<ToolConfirmationRequest> {
    let mut holders: Vec<(&str, &str)> = Vec::new();
    for event in events.iter().filter(|event| event.actions.tool_confirmation.is_some()) {
        let holder = (event.author.as_str(), event.branch.as_str());
        if !holders.contains(&holder) {
            holders.push(holder);
        }
    }
    holders
        .into_iter()
        .flat_map(|(author, branch)| held_tool_calls(events, author, branch))
        .map(|held| held.request)
        .collect()
}

/// The unfinished process continuing `session_id`, the latest when several are.
async fn session_process(
    backend: &dyn ControlBackend,
    user_id: &str,
    session_id: &str,
) -> Result<Option<Process>> {
    let runs = backend
        .run_store()
        .list(
            RunFilter::default()
                .app_name(backend.app_name())
                .user_id(user_id)
                .session_id(session_id),
        )
        .await?;
    match runs.into_iter().filter(|run| !run.state.is_terminal()).max_by_key(|run| run.updated_at) {
        Some(run) => backend.process(&run.run_id).await,
        None => Ok(None),
    }
}

async fn freeze(
    State(backend): Backend,
    caller: Caller,
    Json(request): Json<FreezeRequest>,
) -> Result<Json<FreezeResponse>> {
    caller.require_operator("freeze")?;
    // The kill switch takes effect even when its control record cannot be written; the
    // answer says the record is missing.
    let audit_error = match record(backend.as_ref(), &caller, "freeze", None, &[]).await {
        Ok(()) => None,
        Err(error) => {
            tracing::warn!(error = %error, "the freeze's control record failed; freezing regardless");
            Some(error.to_string())
        }
    };
    let reason = request.reason.unwrap_or_else(|| {
        format!("frozen by {} over the kernel control plane", caller.context.user_id)
    });
    let parked = backend.freeze(&reason, caller.actor()).await?;
    let governance = backend.governance();
    Ok(Json(FreezeResponse {
        frozen: governance.is_frozen(),
        reason: governance.reason(),
        parked_runs: Some(parked),
        audit_error,
    }))
}

async fn unfreeze(State(backend): Backend, caller: Caller) -> Result<Json<FreezeResponse>> {
    caller.require_operator("unfreeze")?;
    record(backend.as_ref(), &caller, "unfreeze", None, &[]).await?;
    backend.unfreeze(caller.actor()).await?;
    let governance = backend.governance();
    Ok(Json(FreezeResponse {
        frozen: governance.is_frozen(),
        reason: governance.reason(),
        parked_runs: None,
        audit_error: None,
    }))
}

/// The agenda of `backend`, or a `Config` error naming why there is none.
fn agenda_of(backend: &dyn ControlBackend) -> Result<Agenda> {
    backend.agenda().ok_or_else(|| KernelError::Config {
        reason: "this control plane serves no agenda; the agenda routes need a kernel".into(),
    })
}

async fn agenda(
    State(backend): Backend,
    _caller: Caller,
    Query(query): Query<AgendaQuery>,
) -> Result<Json<AgendaResponse>> {
    let items = agenda_of(backend.as_ref())?.list(&query.to_filter()).await?;
    Ok(Json(AgendaResponse { items }))
}

async fn agenda_approve(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<AgendaItemResponse>> {
    caller.require_operator("approve an agenda item")?;
    let agenda = agenda_of(backend.as_ref())?;
    record(backend.as_ref(), &caller, "agenda.approve", Some(&id), &[]).await?;
    let item = agenda.approve(&id, &caller.subject()).await?;
    Ok(Json(AgendaItemResponse { item }))
}

async fn agenda_reject(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
    Json(request): Json<RejectRequest>,
) -> Result<Json<AgendaItemResponse>> {
    caller.require_operator("reject an agenda item")?;
    let agenda = agenda_of(backend.as_ref())?;
    record(backend.as_ref(), &caller, "agenda.reject", Some(&id), &[]).await?;
    let item = agenda.reject(&id, &caller.subject(), request.reason.as_deref()).await?;
    Ok(Json(AgendaItemResponse { item }))
}

async fn agenda_schedule(
    State(backend): Backend,
    caller: Caller,
    UrlPath(id): UrlPath<String>,
    Json(request): Json<ScheduleRequest>,
) -> Result<Json<AgendaItemResponse>> {
    caller.require_operator("schedule an agenda item")?;
    let agenda = agenda_of(backend.as_ref())?;
    record(
        backend.as_ref(),
        &caller,
        "agenda.schedule",
        Some(&id),
        &[("wake", json!(request.wake))],
    )
    .await?;
    let item = agenda.schedule(&id, request.wake, &caller.subject()).await?;
    Ok(Json(AgendaItemResponse { item }))
}
