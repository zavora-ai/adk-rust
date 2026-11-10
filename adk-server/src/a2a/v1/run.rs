//! One agent run for one A2A task, detached from the request that started it.
//!
//! A run is a task of its own: it outlives the request, a dropped SSE stream and the request
//! timeout, and always settles its task, as a2a-go's task manager runs an executor without
//! the request's cancellation. Every update the run makes goes through the
//! [`V1Executor`], which stores it and sends it to the task's subscribers (the stream that
//! started the run among them) and push notification configs. A run that panics fails its
//! task.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use a2a_protocol_types::Message;
use a2a_protocol_types::events::TaskStatusUpdateEvent;
use a2a_protocol_types::task::{ContextId, TaskId, TaskState};
use futures::{FutureExt, StreamExt};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::convert::{
    ADK_APP_NAME_METADATA_KEY, ADK_EXTENSION_URI, ADK_SESSION_ID_METADATA_KEY,
    ADK_USER_ID_METADATA_KEY, error_message_part,
};
use super::error::A2aError;
use super::executor::V1Executor;
use super::processor::{EventProcessor, OutputMode};
use crate::auth_bridge::RequestContext;

/// What a run needs besides its task.
#[derive(Clone)]
pub(super) struct RunContext {
    pub executor: Arc<V1Executor>,
    pub runner_config: Arc<adk_runner::RunnerConfig>,
    pub caller: Option<RequestContext>,
    pub output_mode: OutputMode,
    pub hooks: super::hooks::ExecutorHooks,
    pub extensions: Vec<String>,
    /// Fails the run when the agent produces nothing for this long.
    pub inactivity_timeout: Option<std::time::Duration>,
}

/// The most runs at once for one session user; 0 for no limit.
pub type RunsForUser = Arc<dyn Fn(&str) -> usize + Send + Sync>;

/// How many runs may go at once, as a2a-go's `ConcurrencyConfig`: at most `max_runs` in all
/// (0 for no limit), and at most `max_runs_for(user)` for one session user.
#[derive(Clone, Default)]
pub struct ConcurrencyLimits {
    /// The most runs at once in all; 0 for no limit.
    pub max_runs: usize,
    /// The most runs at once for one session user; 0 for no limit.
    pub max_runs_for: Option<RunsForUser>,
}

/// Counts the runs in progress against [`ConcurrencyLimits`].
#[derive(Default)]
pub(super) struct RunLimiter {
    limits: ConcurrencyLimits,
    running: std::sync::Mutex<(usize, std::collections::HashMap<String, usize>)>,
}

impl RunLimiter {
    pub(super) fn new(limits: ConcurrencyLimits) -> Arc<Self> {
        Arc::new(Self { limits, running: std::sync::Mutex::default() })
    }

    /// Takes a place for a run of `user`, or refuses it when a limit is reached.
    pub(super) fn acquire(self: &Arc<Self>, user: &str) -> Result<RunPermit, A2aError> {
        let mut running = self.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.limits.max_runs > 0 && running.0 >= self.limits.max_runs {
            return Err(A2aError::Internal {
                message: "max concurrency limit reached".to_string(),
            });
        }
        if let Some(limit_for) = &self.limits.max_runs_for {
            let limit = limit_for(user);
            let active = running.1.get(user).copied().unwrap_or(0);
            if limit > 0 && active >= limit {
                return Err(A2aError::Internal {
                    message: format!("max scope concurrency limit reached for {user:?}"),
                });
            }
            running.1.insert(user.to_string(), active + 1);
        }
        running.0 += 1;
        Ok(RunPermit { limiter: self.clone(), user: user.to_string() })
    }
}

/// A run's place under the [`RunLimiter`], given back when the run ends.
pub(super) struct RunPermit {
    limiter: Arc<RunLimiter>,
    user: String,
}

impl Drop for RunPermit {
    fn drop(&mut self) {
        let mut running =
            self.limiter.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        running.0 = running.0.saturating_sub(1);
        if let Some(active) = running.1.get_mut(&self.user) {
            *active = active.saturating_sub(1);
            if *active == 0 {
                running.1.remove(&self.user);
            }
        }
    }
}

/// Starts the run of `message` for `task_id` on its own task and returns its handle.
/// `guard` keeps the run cancelable until it ends; `cancellation` stops it.
pub(super) fn spawn<G: Send + 'static>(
    run: RunContext,
    task_id: String,
    context_id: String,
    message: Message,
    cancellation: CancellationToken,
    guard: G,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let executor = run.executor.clone();
        let outcome =
            AssertUnwindSafe(execute(&run, &task_id, &context_id, &message, cancellation))
                .catch_unwind()
                .await;
        drop(guard);
        let failure = match outcome {
            Ok(Ok(())) => return,
            Ok(Err(error)) => format!("agent run failed: {error}"),
            Err(panic) => format!("agent run panicked: {}", panic_message(panic.as_ref())),
        };
        tracing::warn!(task_id, error = %failure, "A2A run ended without settling its task");
        if let Err(error) = settle(
            &executor,
            &task_id,
            &context_id,
            TaskState::Failed,
            vec![error_message_part(failure)],
            None,
        )
        .await
        {
            tracing::warn!(task_id, %error, "could not fail the task of a broken A2A run");
        }
    })
}

/// Runs the agent and settles the task. Errors are store failures: a failed agent run is
/// recorded on the task. The run's [`ExecutorHooks`](super::hooks::ExecutorHooks) are
/// called at their points.
async fn execute(
    run: &RunContext,
    task_id: &str,
    context_id: &str,
    message: &Message,
    cancellation: CancellationToken,
) -> Result<(), A2aError> {
    use super::hooks::ExecutionContext;
    use super::processor::FinalStatus;

    let executor = &run.executor;
    let hooks = &run.hooks;
    let base_meta = base_metadata(&run.runner_config, run.caller.as_ref(), context_id);
    let context = Arc::new(ExecutionContext {
        task_id: task_id.to_string(),
        context_id: context_id.to_string(),
        app_name: run.runner_config.app_name.clone(),
        user_id: session_user_id(run.caller.as_ref(), context_id),
        session_id: context_id.to_string(),
        caller: run.caller.clone(),
        message: message.clone(),
        extensions: run.extensions.clone(),
    });
    let failed = |text: String, metadata: &Map<String, Value>| FinalStatus {
        state: TaskState::Failed,
        parts: vec![error_message_part(text)],
        metadata: metadata.clone(),
    };

    let status = 'run: {
        if let Some(before) = &hooks.before_execute
            && let Err(error) = before(context.clone()).await
        {
            break 'run failed(format!("before execute: {error}"), &base_meta);
        }
        executor
            .update_status(
                task_id,
                context_id,
                TaskState::Working,
                Vec::new(),
                Some(base_meta.clone()),
            )
            .await?;

        let mut processor = EventProcessor::new(run.output_mode, base_meta.clone())
            .with_to_wire_part(hooks.to_wire_part.clone());
        let started = start_agent(
            &run.runner_config,
            run.caller.as_ref(),
            context_id,
            message,
            cancellation.clone(),
            hooks.from_wire_part.as_ref(),
        )
        .await;
        let mut events = match started {
            Ok(events) => events,
            Err(error) => break 'run failed(error.to_string(), &base_meta),
        };

        let mut run_error = None;
        loop {
            let next = match run.inactivity_timeout {
                Some(limit) => match tokio::time::timeout(limit, events.next()).await {
                    Ok(next) => next,
                    Err(_) => {
                        // As a2a-go: an agent that goes quiet fails its task.
                        cancellation.cancel();
                        run_error = Some("agent inactivity timeout".to_string());
                        break;
                    }
                },
                None => events.next().await,
            };
            let Some(result) = next else { break };
            match result {
                Ok(event) => {
                    let Some(mut update) = processor.process(&event) else { continue };
                    if let Some(after) = &hooks.after_event
                        && let Err(error) = after(&context, &event, &mut update)
                    {
                        run_error = Some(format!("processor failed: {error}"));
                        break;
                    }
                    executor.apply_artifact_update(task_id, context_id, update).await?;
                }
                Err(error) => {
                    run_error = Some(format!("agent run failed: {error}"));
                    break;
                }
            }
        }
        drop(events);

        // A cancelled run ends on the `CANCELED` status its cancel recorded.
        if cancellation.is_cancelled() && run_error.is_none() {
            let event = settle(
                executor,
                task_id,
                context_id,
                TaskState::Canceled,
                Vec::new(),
                Some(Map::new()),
            )
            .await?;
            if let Some(cleanup) = &hooks.on_cleanup {
                cleanup(&context, &event);
            }
            return Ok(());
        }
        if let Some(reset) = processor.final_artifact_update() {
            executor.apply_artifact_update(task_id, context_id, reset).await?;
        }
        match run_error {
            Some(error) => failed(error, &base_meta),
            None => processor.final_status(),
        }
    };

    let mut status = status;
    if let Some(after) = &hooks.after_execute
        && let Err(error) = after(&context, &mut status)
    {
        status = failed(format!("after execute: {error}"), &base_meta);
    }
    let event =
        settle(executor, task_id, context_id, status.state, status.parts, Some(status.metadata))
            .await?;
    if let Some(cleanup) = &hooks.on_cleanup {
        cleanup(&context, &event);
    }
    Ok(())
}

/// Moves a task to `state` with `parts` as its status message, unless the task already
/// ended another way: a run that ends while it is being cancelled must not fail on the
/// `CANCELED` state the cancel recorded. The returned event then carries the state the
/// task ended in.
///
/// # Errors
///
/// Returns an error if the task store fails, or the transition is not allowed from a
/// state that is not terminal.
pub(super) async fn settle(
    executor: &V1Executor,
    task_id: &str,
    context_id: &str,
    state: TaskState,
    parts: Vec<a2a_protocol_types::Part>,
    metadata: Option<Map<String, Value>>,
) -> Result<TaskStatusUpdateEvent, A2aError> {
    match executor.update_status(task_id, context_id, state, parts, metadata).await {
        Err(error @ A2aError::InvalidStateTransition { .. }) => {
            let current = executor.task_store().get_task(task_id).await?;
            if is_terminal(current.status.state) {
                Ok(TaskStatusUpdateEvent {
                    task_id: TaskId::new(task_id),
                    context_id: ContextId::new(context_id),
                    status: current.status,
                    metadata: None,
                })
            } else {
                Err(error)
            }
        }
        other => other,
    }
}

fn is_terminal(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Canceled | TaskState::Rejected
    )
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

/// The ADK user whose session runs a task in `context_id`: the authenticated caller, or
/// `A2A_USER_{context_id}` without authentication, as adk-go names it.
pub(super) fn session_user_id(caller: Option<&RequestContext>, context_id: &str) -> String {
    caller.map_or_else(|| format!("A2A_USER_{context_id}"), |context| context.user_id.clone())
}

/// The metadata each update of a run carries: the ADK app, user and session
/// (`adk_app_name`, `adk_user_id`, `adk_session_id`).
pub(super) fn base_metadata(
    runner_config: &adk_runner::RunnerConfig,
    caller: Option<&RequestContext>,
    context_id: &str,
) -> Map<String, Value> {
    let mut meta = Map::new();
    meta.insert(ADK_APP_NAME_METADATA_KEY.to_string(), runner_config.app_name.clone().into());
    meta.insert(ADK_USER_ID_METADATA_KEY.to_string(), session_user_id(caller, context_id).into());
    meta.insert(ADK_SESSION_ID_METADATA_KEY.to_string(), context_id.into());
    meta
}

/// The metadata of a task the agent of `runner_config` runs in `context_id`: the run's
/// [`base_metadata`], and the ADK extension marker, as adk-python and adk-go set it.
pub(super) fn adk_task_metadata(
    runner_config: &adk_runner::RunnerConfig,
    caller: Option<&RequestContext>,
    context_id: &str,
) -> std::collections::HashMap<String, Value> {
    let mut meta: std::collections::HashMap<String, Value> =
        base_metadata(runner_config, caller, context_id).into_iter().collect();
    meta.insert(ADK_EXTENSION_URI.to_string(), true.into());
    meta
}

/// Starts the agent on `message` in the session of `context_id`, creating the session when
/// it does not exist.
///
/// Without a `run_config` of its own, the run does not stream model output token by token:
/// each model response is one event, as adk-go and adk-python run agents over A2A. A
/// `run_config` with `StreamingMode::SSE` streams partial events instead.
///
/// # Errors
///
/// Returns an error if the session cannot be read or created, the message cannot be
/// converted, or the runner cannot start.
async fn start_agent(
    runner_config: &Arc<adk_runner::RunnerConfig>,
    caller: Option<&RequestContext>,
    context_id: &str,
    msg: &Message,
    cancellation: CancellationToken,
    from_wire_part: Option<&super::hooks::FromWirePart>,
) -> Result<adk_core::EventStream, A2aError> {
    use adk_core::{SessionId, UserId};
    use adk_session::{CreateRequest, GetRequest};

    let app_name = &runner_config.app_name;
    // The context ID is chosen by the client, so it only names the session user when
    // no authentication is configured.
    let user_id = session_user_id(caller, context_id);
    let session_id = context_id.to_string();

    // Only a missing session is created: a read that fails for another reason must not
    // replace the stored conversation. A create that loses a race finds the session.
    let session_service = &runner_config.session_service;
    let existing = session_service
        .get(GetRequest {
            app_name: app_name.clone(),
            user_id: user_id.clone(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await;
    match existing {
        Ok(_) => {}
        Err(error) if error.is_not_found() => {
            let created = session_service
                .create(CreateRequest {
                    app_name: app_name.clone(),
                    user_id: user_id.clone(),
                    session_id: Some(session_id.clone()),
                    state: std::collections::HashMap::new(),
                })
                .await;
            if let Err(error) = created
                && !error.is_conflict()
            {
                return Err(A2aError::Internal {
                    message: format!("failed to create a session: {error}"),
                });
            }
        }
        Err(error) => {
            return Err(A2aError::Internal { message: format!("session lookup: {error}") });
        }
    }

    let mut adk_parts = Vec::new();
    for part in &msg.parts {
        match from_wire_part {
            Some(convert) => adk_parts.extend(convert(part)?),
            None => adk_parts.push(super::convert::wire_part_to_adk(part)?),
        }
    }
    // An answer to `adk_request_confirmation` decides the held call it names: approved
    // (`{"confirmed": true}`) or denied. It is not shown to the agent.
    let mut decisions = std::collections::HashMap::new();
    adk_parts.retain(|part| match part {
        adk_core::Part::FunctionResponse { function_response, id: Some(id), .. }
            if function_response.name == super::processor::REQUEST_CONFIRMATION_FUNCTION =>
        {
            let confirmed =
                function_response.response.get("confirmed").and_then(serde_json::Value::as_bool)
                    == Some(true);
            let decision = if confirmed {
                adk_core::ToolConfirmationDecision::Approve
            } else {
                adk_core::ToolConfirmationDecision::Deny
            };
            decisions.insert(id.clone(), decision);
            false
        }
        _ => true,
    });
    let content = adk_core::Content { role: "user".to_string(), parts: adk_parts };

    let mut runner_builder = adk_runner::Runner::builder()
        .app_name(runner_config.app_name.clone())
        .agent(runner_config.agent.clone())
        .session_service(runner_config.session_service.clone());
    if let Some(control) = &runner_config.governance {
        runner_builder = runner_builder.governance(control.clone());
    }
    if let Some(policy) = &runner_config.tool_policy {
        runner_builder = runner_builder.tool_policy(policy.clone());
    }
    if let Some(ref artifact_service) = runner_config.artifact_service {
        runner_builder = runner_builder.artifact_service(artifact_service.clone());
    }
    if let Some(ref memory_service) = runner_config.memory_service {
        runner_builder = runner_builder.memory_service(memory_service.clone());
    }
    if let Some(ref plugin_manager) = runner_config.plugin_manager {
        runner_builder = runner_builder.plugin_manager(plugin_manager.clone());
    }
    let mut run_config = runner_config.run_config.clone().unwrap_or_else(|| adk_core::RunConfig {
        streaming_mode: adk_core::StreamingMode::None,
        ..adk_core::RunConfig::default()
    });
    run_config.tool_confirmation_decisions.extend(decisions);
    runner_builder = runner_builder.run_config(run_config);
    if let Some(ref compaction_config) = runner_config.compaction_config {
        runner_builder = runner_builder.compaction_config(compaction_config.clone());
    }
    if let Some(ref context_cache_config) = runner_config.context_cache_config {
        runner_builder = runner_builder.context_cache_config(context_cache_config.clone());
    }
    if let Some(ref cache_capable) = runner_config.cache_capable {
        runner_builder = runner_builder.cache_capable(cache_capable.clone());
    }
    if let Some(ref intra_compaction_config) = runner_config.intra_compaction_config {
        runner_builder = runner_builder.intra_compaction_config(intra_compaction_config.clone());
    }
    if let Some(ref intra_compaction_summarizer) = runner_config.intra_compaction_summarizer {
        runner_builder =
            runner_builder.intra_compaction_summarizer(intra_compaction_summarizer.clone());
    }
    // The caller's own context wins over a context baked into the shared config.
    if let Some(request_context) = caller.cloned().or_else(|| runner_config.request_context.clone())
    {
        runner_builder = runner_builder.request_context(request_context);
    }
    // A child of the configuration's own token (see `RunningTasks::start`), so that token
    // still stops the run.
    runner_builder = runner_builder.cancellation_token(cancellation);
    let runner = runner_builder
        .build()
        .map_err(|e| A2aError::Internal { message: format!("runner create: {e}") })?;

    runner
        .run(
            UserId::new(&user_id).map_err(|e| A2aError::Internal { message: e.to_string() })?,
            SessionId::new(&session_id)
                .map_err(|e| A2aError::Internal { message: e.to_string() })?,
            content,
        )
        .await
        .map_err(|e| A2aError::Internal { message: format!("runner run: {e}") })
}

/// Cancels the remote tasks a cancelled `INPUT_REQUIRED` task waited on, as adk-go's
/// executor does: the calls in the task's status message came from remote agents, whose
/// events in the task's session name the remote task (`a2a:task_id`). Each remote agent of
/// the agent tree cancels its own task, for the session's user. Failures are logged.
pub(super) async fn cancel_waiting_remote_tasks(
    runner_config: &adk_runner::RunnerConfig,
    caller: Option<&RequestContext>,
    entry: &super::task_store::TaskStoreEntry,
) {
    use crate::a2a::remote_agent::v1_remote::A2A_TASK_ID_KEY;
    let Some(message) = &entry.status.message else { return };
    let pending: std::collections::HashSet<String> = message
        .parts
        .iter()
        .filter_map(|part| super::convert::wire_part_to_adk(part).ok())
        .filter_map(|part| match part {
            adk_core::Part::FunctionCall { id: Some(id), .. } => Some(id),
            _ => None,
        })
        .collect();
    if pending.is_empty() {
        return;
    }
    let user_id = session_user_id(caller, &entry.context_id);
    let session = match runner_config
        .session_service
        .get(adk_session::GetRequest {
            app_name: runner_config.app_name.clone(),
            user_id: user_id.clone(),
            session_id: entry.context_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await
    {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(task_id = %entry.id, %error, "cannot read the session to cancel remote tasks");
            return;
        }
    };
    let mut remote_tasks: Vec<(String, String)> = Vec::new();
    for event in session.events().all().iter().rev() {
        let calls_pending = event.content().into_iter().flat_map(|content| &content.parts).any(
            |part| matches!(part, adk_core::Part::FunctionCall { id: Some(id), .. } if pending.contains(id)),
        );
        if let (true, Some(task_id)) = (calls_pending, event.provider_metadata.get(A2A_TASK_ID_KEY))
            && !remote_tasks.iter().any(|(_, known)| known == task_id)
        {
            remote_tasks.push((event.author.clone(), task_id.clone()));
        }
    }
    for (author, task_id) in remote_tasks {
        let Some(agent) = find_agent(&runner_config.agent, &author) else { continue };
        let acting = adk_core::ActingContext::derived(
            &runner_config.app_name,
            &author,
            &user_id,
            adk_core::SubjectKind::Human,
        );
        let cancel = agent.cancel_remote_task(&task_id, &acting);
        match tokio::time::timeout(std::time::Duration::from_secs(10), cancel).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%task_id, agent = %author, %error, "cancellation failed")
            }
            Err(_) => tracing::warn!(%task_id, agent = %author, "cancellation failed: timed out"),
        }
    }
}

/// The agent named `name` in the tree under `root`.
fn find_agent(root: &Arc<dyn adk_core::Agent>, name: &str) -> Option<Arc<dyn adk_core::Agent>> {
    if root.name() == name {
        return Some(root.clone());
    }
    root.sub_agents().iter().find_map(|sub| find_agent(sub, name))
}
