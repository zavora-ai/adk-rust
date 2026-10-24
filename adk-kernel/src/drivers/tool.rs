//! Driver tools: the partition's toolset, and calls that park on a server's question and
//! resume from their checkpoint (spec 3.2 §7.7, §13 Q9).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use adk_core::{
    AdkError, ApprovalScope, ReadonlyContext, Result, Tool, ToolApproval, ToolConfirmationDecision,
    ToolConfirmationRequest, ToolContext, ToolEffect, ToolGate, ToolOrigin, Toolset, async_trait,
    confirmation_pending_response,
};
use adk_tool::mcp::rmcp::model::{
    ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationSchema, InputRequest,
    InputResponses,
};
use adk_tool::mcp::{
    ElicitationHandler, McpCallOutcome, McpServerManager, ParkedCallKind, ParkedMcpCall,
};
use chrono::Utc;
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::store::DriverPark;
use super::supervisor::{DriverSupervisor, declaration_digest};
use super::{EffectClass, driver_of, driver_source};
use crate::partition::{
    CrossPartition, CrossingAccess, CrossingResource, CrossingTool, PartitionId, PartitionPolicy,
};

/// The tools of the drivers a partition mounts (spec 3.2 §7.7, §7.8).
pub(crate) struct DriverToolset {
    drivers: DriverSupervisor,
    partition: PartitionId,
    /// The partition's own driver list; `None` serves every driver whose manifest lists
    /// the partition.
    allowed: Option<Vec<String>>,
}

impl DriverToolset {
    pub(crate) fn new(
        drivers: DriverSupervisor,
        partition: PartitionId,
        allowed: Option<Vec<String>>,
    ) -> Self {
        Self { drivers, partition, allowed }
    }
}

#[async_trait]
impl Toolset for DriverToolset {
    fn name(&self) -> &str {
        "kernel_drivers"
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        self.drivers.load().await?;
        let mut ids: Vec<String> = match &self.allowed {
            Some(allowed) => allowed.clone(),
            None => self.drivers.mounted(),
        };
        ids.sort();
        ids.dedup();
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        let mut claimed: HashMap<String, String> = HashMap::new();
        for id in &ids {
            let serves = self
                .drivers
                .record(id)
                .is_some_and(|record| record.manifest.partitions.contains(&self.partition));
            if !serves {
                continue;
            }
            let manager = match self.drivers.manager(id).await {
                Ok(manager) => manager,
                Err(error) => {
                    tracing::warn!(driver.id = %id, error = %error, "driver did not start; its tools are left out");
                    continue;
                }
            };
            let listed = match manager.tools(ctx.clone()).await {
                Ok(listed) => listed,
                Err(error) => {
                    tracing::warn!(driver.id = %id, error = %error, "driver's tools could not be listed");
                    continue;
                }
            };
            let observed: BTreeMap<String, String> = listed
                .iter()
                .map(|tool| (tool.name().to_string(), declaration_digest(id, tool.as_ref())))
                .collect();
            self.drivers.observe(id, observed).await;
            let Some(record) = self.drivers.record(id) else { continue };
            for tool in listed {
                let name = tool.name().to_string();
                if let Some(owner) = claimed.get(&name) {
                    tracing::warn!(driver.id = %id, tool = %name, claimed_by = %owner, "driver tool left out: another driver of the partition has a tool of that name");
                    continue;
                }
                claimed.insert(name.clone(), id.clone());
                tools.push(Arc::new(DriverTool {
                    class: record.manifest.class_of(&name),
                    inner: tool,
                    driver: id.clone(),
                    source: driver_source(id),
                    drivers: self.drivers.clone(),
                }));
            }
        }
        Ok(tools)
    }
}

/// The drivers of other partitions offered to a coordinator as crossing tools (spec 3.2
/// §7.7, §7.8, K-I15).
pub(crate) struct DriverCrossings {
    drivers: DriverSupervisor,
    policy: PartitionPolicy,
    targets: Vec<PartitionId>,
}

impl DriverCrossings {
    pub(crate) fn new(
        drivers: DriverSupervisor,
        policy: PartitionPolicy,
        targets: Vec<PartitionId>,
    ) -> Self {
        Self { drivers, policy, targets }
    }
}

#[async_trait]
impl Toolset for DriverCrossings {
    fn name(&self) -> &str {
        "kernel_driver_crossings"
    }

    /// The tools each target partition's own toolset serves, read from the partition table
    /// at every listing, each wrapped as the crossing `<to>__<tool>`: a partition that takes
    /// no crossing, the coordinator's own when it takes none, and one the coordinator's
    /// partition blocks offer nothing. A read is offered as a read only when the manifest
    /// classes the tool `read`.
    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let table = self.policy.partitions();
        let home = table.get(self.policy.home()).await.map_err(AdkError::from)?;
        if home.as_ref().is_some_and(|home| home.cross_partition == CrossPartition::Deny) {
            return Ok(Vec::new());
        }
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        for to in &self.targets {
            if to == self.policy.home()
                || home.as_ref().is_some_and(|home| home.blocked_crossings.contains(to))
            {
                continue;
            }
            let Some(partition) = table.get(to).await.map_err(AdkError::from)? else {
                continue;
            };
            if partition.cross_partition == CrossPartition::Deny {
                continue;
            }
            let listed =
                DriverToolset::new(self.drivers.clone(), to.clone(), Some(partition.drivers))
                    .tools(ctx.clone())
                    .await?;
            for tool in listed {
                let Some(driver) = driver_of(tool.origin()).map(str::to_string) else {
                    continue;
                };
                // A read is reused for the rest of a run only when the manifest classes the
                // tool `read` and its declaration is the approved one; anything else is
                // asked each call (K-I9, K-I14).
                let stale = self
                    .drivers
                    .record(&driver)
                    .is_none_or(|record| record.stale.contains_key(tool.name()));
                let access = match self.drivers.class_of(tool.origin(), tool.name()) {
                    Some(EffectClass::Read) if !stale => CrossingAccess::Read,
                    _ => CrossingAccess::Write,
                };
                let name = tool.name().to_string();
                let crossing = CrossingTool::new(
                    tool,
                    &self.policy,
                    to.clone(),
                    CrossingResource::Driver,
                    access,
                    driver.clone(),
                );
                self.drivers.register_crossing(&driver, crossing.name(), &name);
                tools.push(Arc::new(crossing));
            }
        }
        Ok(tools)
    }
}

/// One tool of a driver, classed by the manifest (spec 3.2 §7.7).
///
/// It reports the manifest's effect, never the server's hints (3.1 I12), and the driver's
/// own source, `driver:<id>`. A call the server answers with a question parks: the
/// [`DriverPark`] is stored as the run's checkpoint, the question is asked through the run's
/// confirmation sources (a person's decision in the approval inbox, a live prompter), and
/// the call is held until it is decided. The agent dispatches the held call again once the
/// decision exists — in this process or after a restart — and the call is re-issued with
/// the answers (spec 3.2 §13 Q9).
pub(crate) struct DriverTool {
    inner: Arc<dyn Tool>,
    driver: String,
    source: String,
    class: EffectClass,
    drivers: DriverSupervisor,
}

/// The result of asking a parked call's question.
enum Asked {
    /// Answered now: re-issue with these responses.
    Answered(ToolApproval),
    /// Waiting for a person; the call is held.
    Held,
}

impl DriverTool {
    fn manager_error(error: crate::KernelError) -> AdkError {
        error.into()
    }

    /// The held request that asks `parked`'s questions.
    fn question(
        &self,
        ctx: &dyn ToolContext,
        arguments: &Map<String, Value>,
        parked: &ParkedMcpCall,
    ) -> ToolConfirmationRequest {
        let questions: Map<String, Value> = parked
            .input_requests
            .iter()
            .map(|(key, request)| (key.clone(), describe(request)))
            .collect();
        ToolConfirmationRequest::new(
            format!("{}/elicitation", self.source),
            json!({
                "driver": self.driver,
                "tool": self.inner.name(),
                "arguments": arguments,
                "round": parked.input_rounds,
                "inputRequestsDigest": parked.input_requests_digest,
                "questions": questions,
            }),
        )
        .with_function_call_id(ctx.function_call_id())
        .with_acting_context(ctx.acting_context())
        .with_origin(ToolOrigin::of(self))
    }

    fn reason(&self, park: &DriverPark) -> String {
        let messages: Vec<String> = park
            .call
            .input_requests
            .values()
            .filter_map(|request| match request {
                InputRequest::Elicitation(elicit) => match &elicit.params {
                    ElicitRequestParams::FormElicitationParams { message, .. }
                    | ElicitRequestParams::UrlElicitationParams { message, .. } => {
                        Some(message.clone())
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        format!(
            "driver `{}` asks before `{}` can continue: {}",
            self.driver,
            self.inner.name(),
            messages.join(" / ")
        )
    }

    /// Asks the question of `park` through the run's confirmation sources.
    async fn ask(&self, ctx: &dyn ToolContext, park: &DriverPark) -> Result<Asked> {
        let Some(config) = ctx.run_config() else {
            return Ok(Asked::Held);
        };
        let gate = ToolGate::new(config, ctx.agent_name(), ctx.invocation_id(), park.scope.clone())
            .with_acting_context(ctx.acting_context());
        Ok(match adk_core::confirm_tool_call(&gate, &park.request, &self.reason(park)).await? {
            Some(approval) => Asked::Answered(approval),
            None => Asked::Held,
        })
    }

    /// The fingerprint the governed path gives this call with `arguments`.
    fn call_fingerprint(&self, ctx: &dyn ToolContext, arguments: &Map<String, Value>) -> String {
        ToolConfirmationRequest::for_tool(self, Value::Object(arguments.clone()))
            .with_acting_context(ctx.acting_context())
            .fingerprint()
    }

    /// Whether this dispatch resumes a hold of fingerprint `held`: the agent keys a
    /// resumed call's ledger record by the request it was held for.
    fn resumes_hold(ctx: &dyn ToolContext, held: &str) -> bool {
        let key = adk_agent::held_call_ledger_key(
            ctx.app_name(),
            ctx.user_id(),
            ctx.session_id(),
            ctx.function_call_id(),
            held,
        );
        ctx.idempotency_key() == key
    }

    /// Holds the call on `park`'s question: the event carries the request, and the run
    /// waits for its decision.
    ///
    /// When a person approved the call itself before its server asked, the approval is
    /// carried to the call that resumes, once, until the question lapses: the person is not
    /// asked to approve the same call again, and the agent does not hold the call twice
    /// under one fingerprint. Any other call that resumes is decided again under the
    /// permissions in force (3.1 I4); a denial always wins.
    async fn hold(
        &self,
        ctx: &dyn ToolContext,
        park: &DriverPark,
        arguments: &Map<String, Value>,
    ) -> Result<Value> {
        if park.carries_approval
            && let Some(store) = ctx.run_config().and_then(|config| config.approval_store.clone())
        {
            let mut approval = ToolApproval::approve();
            approval.expires_at = park.expires_at;
            store.decide(&park.scope, &self.call_fingerprint(ctx, arguments), approval).await?;
        }
        let mut actions = ctx.actions();
        actions.tool_confirmation = Some(park.request.clone());
        ctx.set_actions(actions);
        tracing::info!(
            driver.id = %self.driver,
            tool.name = %self.inner.name(),
            call.id = %park.call_id,
            park.kind = ?park.call.kind,
            "driver call parked on its server's question"
        );
        Ok(confirmation_pending_response(self.reason(park)))
    }

    /// Ends a parked call: its checkpoint, and an approval carried to it that the call did
    /// not need, are spent.
    async fn settle(
        &self,
        ctx: &dyn ToolContext,
        park: &DriverPark,
        arguments: &Map<String, Value>,
    ) {
        if let Err(error) = self.drivers.store().delete_park(&park.scope, &park.call_id).await {
            tracing::warn!(driver.id = %self.driver, error = %error, "could not delete a settled driver park");
        }
        if park.carries_approval
            && let Some(store) = ctx.run_config().and_then(|config| config.approval_store.clone())
        {
            let _ = store.take_approval(&park.scope, &self.call_fingerprint(ctx, arguments)).await;
        }
    }

    /// Calls the tool in park mode; a legacy question ends the call as a legacy park.
    async fn call(
        &self,
        ctx: &Arc<dyn ToolContext>,
        manager: &McpServerManager,
        arguments: Map<String, Value>,
    ) -> Result<McpCallOutcome> {
        let call_id = ctx.function_call_id().to_string();
        let (parked, _guard) = self.drivers.inner.legacy.register(&self.driver, &call_id);
        tokio::select! {
            outcome = manager.call_tool_resumable(&self.driver, ctx.clone(), self.inner.name(), arguments) => outcome,
            Ok(parked) = parked => Ok(McpCallOutcome::Parked(parked)),
        }
    }

    /// Re-issues `park`'s call with `approval`'s answers.
    async fn reissue(
        &self,
        ctx: &Arc<dyn ToolContext>,
        manager: &McpServerManager,
        park: &DriverPark,
        approval: &ToolApproval,
    ) -> Result<McpCallOutcome> {
        let answers = answers(&park.call, approval)?;
        let call_id = park.call_id.clone();
        let (parked, _guard) = self.drivers.inner.legacy.register(&self.driver, &call_id);
        let responses = if park.call.kind == ParkedCallKind::Legacy {
            // The call runs again and asks again; the handler answers from the decision.
            for (key, answer) in &answers {
                if let Some(request) = park.call.input_requests.get(key) {
                    self.drivers.inner.legacy.stash(
                        &self.driver,
                        &call_id,
                        question_digest(request),
                        answer.clone(),
                    );
                }
            }
            InputResponses::new()
        } else {
            answers
        };
        tokio::select! {
            outcome = manager.reissue_parked_call_for(ctx.clone(), &park.call, responses) => outcome,
            Ok(parked) = parked => Ok(McpCallOutcome::Parked(parked)),
        }
    }
}

#[async_trait]
impl Tool for DriverTool {
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
        &self.source
    }

    fn effect(&self) -> ToolEffect {
        self.class.tool_effect()
    }

    fn is_read_only(&self) -> bool {
        self.class == EffectClass::Read
    }

    fn is_concurrency_safe(&self) -> bool {
        self.class == EffectClass::Read
    }

    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        self.inner.timeout_override()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let arguments = match args {
            Value::Object(arguments) => arguments,
            Value::Null => Map::new(),
            _ => return Err(AdkError::tool("Tool arguments must be an object")),
        };
        let manager = self.drivers.manager(&self.driver).await.map_err(Self::manager_error)?;
        let scope = ApprovalScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id());
        let call_id = ctx.function_call_id().to_string();
        let store = self.drivers.store().clone();

        let mut previous = if call_id.is_empty() {
            None
        } else {
            store.get_park(&scope, &call_id).await.map_err(Self::manager_error)?
        };
        // A person approved this call when this dispatch resumes the governed path's hold
        // of the call itself, or a question hold an approval was carried to.
        let approved = !call_id.is_empty()
            && (Self::resumes_hold(ctx.as_ref(), &self.call_fingerprint(ctx.as_ref(), &arguments))
                || previous.as_ref().is_some_and(|park| {
                    park.carries_approval && Self::resumes_hold(ctx.as_ref(), &park.fingerprint)
                }));
        let mut outcome = match &previous {
            // The call parked earlier; its question's decision resumes it.
            Some(park) => {
                if park.request.origin.as_ref() != Some(&ToolOrigin::of(self))
                    || park.request.acting.as_ref().map(|acting| &acting.agent.id)
                        != Some(&ctx.acting_context().agent.id)
                {
                    self.settle(ctx.as_ref(), park, &arguments).await;
                    if let Some(approvals) = ctx.run_config().and_then(|c| c.approval_store.clone())
                    {
                        let _ = approvals.take_approval(&park.scope, &park.fingerprint).await;
                    }
                    return Err(AdkError::tool(format!(
                        "`{}` was not run: its declaration or its caller changed while its question \
                         waited; call it again if it is still needed",
                        self.inner.name()
                    )));
                }
                match self.ask(ctx.as_ref(), park).await? {
                    Asked::Answered(approval) => {
                        match self.reissue(&ctx, &manager, park, &approval).await {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                self.settle(ctx.as_ref(), park, &arguments).await;
                                return Err(error);
                            }
                        }
                    }
                    Asked::Held => {
                        let mut park = park.clone();
                        park.carries_approval = approved;
                        store.put_park(park.clone()).await.map_err(Self::manager_error)?;
                        return self.hold(ctx.as_ref(), &park, &arguments).await;
                    }
                }
            }
            None => self.call(&ctx, &manager, arguments.clone()).await?,
        };
        loop {
            match outcome {
                McpCallOutcome::Completed(value) => {
                    if let Some(park) = &previous {
                        self.settle(ctx.as_ref(), park, &arguments).await;
                    }
                    return Ok(value);
                }
                McpCallOutcome::Parked(parked) => {
                    let request = self.question(ctx.as_ref(), &arguments, &parked);
                    let mut park = DriverPark::new(scope.clone(), call_id.clone(), parked, request)
                        .with_expires_at(
                            Utc::now()
                                + chrono::Duration::from_std(self.drivers.inner.question_timeout)
                                    .unwrap_or(chrono::Duration::hours(24)),
                        );
                    if let Some(run_id) =
                        ctx.run_config().and_then(|config| config.durable_run_id.clone())
                    {
                        park = park.with_run_id(run_id);
                    }
                    park.carries_approval = approved;
                    // The checkpoint is written before the question is asked, so no answer
                    // ever arrives for a park that is not stored.
                    store.put_park(park.clone()).await.map_err(Self::manager_error)?;
                    match self.ask(ctx.as_ref(), &park).await? {
                        Asked::Answered(approval) => {
                            outcome = match self.reissue(&ctx, &manager, &park, &approval).await {
                                Ok(outcome) => outcome,
                                Err(error) => {
                                    self.settle(ctx.as_ref(), &park, &arguments).await;
                                    return Err(error);
                                }
                            };
                            previous = Some(park);
                        }
                        Asked::Held => return self.hold(ctx.as_ref(), &park, &arguments).await,
                    }
                }
            }
        }
    }
}

/// What the person sees of one input request.
fn describe(request: &InputRequest) -> Value {
    match request {
        InputRequest::Elicitation(elicit) => match &elicit.params {
            ElicitRequestParams::FormElicitationParams { message, requested_schema, .. } => {
                json!({ "message": message, "requestedSchema": requested_schema })
            }
            ElicitRequestParams::UrlElicitationParams { message, url, .. } => {
                json!({ "message": message, "url": url })
            }
            other => json!({ "unsupported": serde_json::to_value(other).unwrap_or(Value::Null) }),
        },
        other => json!({ "unsupported": serde_json::to_value(other).unwrap_or(Value::Null) }),
    }
}

/// The digest of one question, by what it asks.
fn question_digest(request: &InputRequest) -> String {
    adk_core::json_digest(&describe(request))
}

/// The defaults a form's schema declares, as its content when the person approves without
/// an answer.
fn defaults_of(schema: &ElicitationSchema) -> Value {
    let schema = serde_json::to_value(schema).unwrap_or(Value::Null);
    let defaults: Map<String, Value> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .filter_map(|(name, property)| {
                    property.get("default").map(|value| (name.clone(), value.clone()))
                })
                .collect()
        })
        .unwrap_or_default();
    Value::Object(defaults)
}

/// The answers `approval` gives `parked`'s questions (spec 3.2 §7.7).
///
/// A denial declines every question. An approval accepts each: a form's content is the
/// approval's edited arguments — keyed by the question's key when the call asks several
/// forms — or the schema's defaults; a URL question is accepted as is.
fn answers(parked: &ParkedMcpCall, approval: &ToolApproval) -> Result<InputResponses> {
    let forms = parked
        .input_requests
        .values()
        .filter(|request| {
            matches!(
                request,
                InputRequest::Elicitation(elicit)
                    if matches!(elicit.params, ElicitRequestParams::FormElicitationParams { .. })
            )
        })
        .count();
    let mut responses = InputResponses::new();
    for (key, request) in &parked.input_requests {
        let InputRequest::Elicitation(elicit) = request else {
            return Err(AdkError::tool(format!(
                "driver `{}` asked for input other than an elicitation, which drivers do not answer",
                parked.server_id
            )));
        };
        let result = match approval.decision {
            ToolConfirmationDecision::Deny => ElicitResult::new(ElicitationAction::Decline),
            ToolConfirmationDecision::Approve => match &elicit.params {
                ElicitRequestParams::FormElicitationParams { requested_schema, .. } => {
                    let content = match &approval.edited_args {
                        Some(Value::Object(edits)) if forms > 1 => edits.get(key).cloned(),
                        Some(edits) => Some(edits.clone()),
                        None => None,
                    };
                    ElicitResult::new(ElicitationAction::Accept)
                        .with_content(content.unwrap_or_else(|| defaults_of(requested_schema)))
                }
                ElicitRequestParams::UrlElicitationParams { .. } => {
                    ElicitResult::new(ElicitationAction::Accept)
                }
                // A kind of question this release cannot answer is declined.
                _ => ElicitResult::new(ElicitationAction::Decline),
            },
        };
        responses.insert(
            key.clone(),
            serde_json::to_value(result).map_err(|e| AdkError::tool(e.to_string()))?,
        );
    }
    Ok(responses)
}

/// Calls of drivers' tools in flight, which a legacy `elicitation/create` belongs to
/// (spec 3.2 §13 Q9).
#[derive(Default)]
pub(crate) struct LegacyWaiters {
    calls: Mutex<HashMap<(String, String), Slot>>,
}

#[derive(Default)]
struct Slot {
    /// Ends the call as a legacy park.
    park: Option<oneshot::Sender<ParkedMcpCall>>,
    /// Cancelled when the call is no longer waited for.
    released: CancellationToken,
    /// Answers decided for the call's questions, by question digest.
    answers: HashMap<String, Value>,
}

/// Unregisters a call when its attempt ends.
pub(crate) struct WaiterGuard {
    waiters: Arc<LegacyWaiters>,
    key: (String, String),
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let slot = self.waiters.calls.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.key);
        if let Some(slot) = slot {
            slot.released.cancel();
        }
    }
}

/// What the handler does with a legacy question.
enum Legacy {
    Answer(Value),
    Park(CancellationToken),
    Unclaimed,
}

impl LegacyWaiters {
    /// Registers call `call_id` of `driver` until the guard drops; the receiver yields the
    /// call's legacy park when its server asks a question nobody has answered.
    pub(crate) fn register(
        self: &Arc<Self>,
        driver: &str,
        call_id: &str,
    ) -> (oneshot::Receiver<ParkedMcpCall>, WaiterGuard) {
        let (sender, receiver) = oneshot::channel();
        let key = (driver.to_string(), call_id.to_string());
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        let slot = calls.entry(key.clone()).or_default();
        slot.park = Some(sender);
        drop(calls);
        (receiver, WaiterGuard { waiters: Arc::clone(self), key })
    }

    /// Records the decided answer to a question of the call, for its next attempt.
    fn stash(&self, driver: &str, call_id: &str, digest: String, answer: Value) {
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = calls.get_mut(&(driver.to_string(), call_id.to_string())) {
            slot.answers.insert(digest, answer);
        }
    }

    fn on_question(&self, driver: &str, call: ParkedMcpCall) -> Legacy {
        let Some(call_id) = call.call_id.clone() else { return Legacy::Unclaimed };
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = calls.get_mut(&(driver.to_string(), call_id)) else {
            return Legacy::Unclaimed;
        };
        let digest = call.input_requests.values().next().map(question_digest);
        if let Some(answer) = digest.and_then(|digest| slot.answers.remove(&digest)) {
            return Legacy::Answer(answer);
        }
        match slot.park.take() {
            Some(park) => {
                let released = slot.released.clone();
                let _ = park.send(call);
                Legacy::Park(released)
            }
            None => Legacy::Unclaimed,
        }
    }
}

/// A driver's elicitation handler (spec 3.2 §7.7): answers the legacy questions of its
/// driver tools' calls from their decisions, ends a call whose question nobody has
/// answered yet as a legacy park, and hands any other elicitation to the idle handler.
pub(crate) struct DriverElicitation {
    driver: String,
    waiters: Arc<LegacyWaiters>,
    idle: Option<Arc<dyn ElicitationHandler>>,
}

impl DriverElicitation {
    pub(crate) fn new(
        driver: String,
        waiters: Arc<LegacyWaiters>,
        idle: Option<Arc<dyn ElicitationHandler>>,
    ) -> Self {
        Self { driver, waiters, idle }
    }

    async fn answer(
        &self,
    ) -> Option<std::result::Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>>> {
        let call = ParkedMcpCall::current()
            .filter(|call| call.kind == ParkedCallKind::Legacy && call.server_id == self.driver)?;
        match self.waiters.on_question(&self.driver, call) {
            Legacy::Answer(answer) => Some(
                serde_json::from_value(answer)
                    .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
            ),
            Legacy::Park(released) => {
                // The call is abandoned and parked; its server's request ends cancelled.
                released.cancelled().await;
                Some(Ok(ElicitResult::new(ElicitationAction::Cancel)))
            }
            Legacy::Unclaimed => None,
        }
    }
}

#[async_trait]
impl ElicitationHandler for DriverElicitation {
    async fn handle_form_elicitation(
        &self,
        message: &str,
        schema: &ElicitationSchema,
        metadata: Option<&Value>,
    ) -> std::result::Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(answer) = self.answer().await {
            return answer;
        }
        match &self.idle {
            Some(idle) => idle.handle_form_elicitation(message, schema, metadata).await,
            None => Ok(ElicitResult::new(ElicitationAction::Decline)),
        }
    }

    async fn handle_url_elicitation(
        &self,
        message: &str,
        url: &str,
        elicitation_id: &str,
        metadata: Option<&Value>,
    ) -> std::result::Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(answer) = self.answer().await {
            return answer;
        }
        match &self.idle {
            Some(idle) => idle.handle_url_elicitation(message, url, elicitation_id, metadata).await,
            None => Ok(ElicitResult::new(ElicitationAction::Decline)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(message: &str) -> InputRequest {
        let schema = serde_json::from_value(json!({
            "type": "object",
            "properties": { "ok": { "type": "boolean", "default": true }, "note": { "type": "string" } }
        }))
        .unwrap();
        InputRequest::Elicitation(adk_tool::mcp::rmcp::model::ElicitRequest::new(
            ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: message.into(),
                requested_schema: schema,
            },
        ))
    }

    fn parked(questions: &[(&str, &str)]) -> ParkedMcpCall {
        let requests: adk_tool::mcp::rmcp::model::InputRequests =
            questions.iter().map(|(key, message)| ((*key).to_string(), form(message))).collect();
        serde_json::from_value(json!({
            "serverId": "email",
            "toolName": "send_draft",
            "callId": "call-1",
            "inputRequests": requests,
            "inputRequestsDigest": ParkedMcpCall::digest_input_requests(&requests),
            "inputRounds": 1,
            "kind": "mrtr"
        }))
        .unwrap()
    }

    #[test]
    fn an_approval_answers_a_form_with_its_edits_or_the_defaults() {
        let call = parked(&[("confirm", "Send it?")]);
        let mut approval = ToolApproval::approve();
        let defaults = answers(&call, &approval).unwrap();
        assert_eq!(defaults["confirm"], json!({ "action": "accept", "content": { "ok": true } }));
        approval.edited_args = Some(json!({ "ok": false, "note": "later" }));
        let edited = answers(&call, &approval).unwrap();
        assert_eq!(edited["confirm"]["content"], json!({ "ok": false, "note": "later" }));
        let denied = answers(&call, &ToolApproval::deny()).unwrap();
        assert_eq!(denied["confirm"], json!({ "action": "decline" }));
    }

    #[test]
    fn several_forms_take_their_answers_by_key() {
        let call = parked(&[("a", "First?"), ("b", "Second?")]);
        let mut approval = ToolApproval::approve();
        approval.edited_args = Some(json!({ "a": { "ok": false } }));
        let responses = answers(&call, &approval).unwrap();
        assert_eq!(responses["a"]["content"], json!({ "ok": false }));
        assert_eq!(responses["b"]["content"], json!({ "ok": true }));
    }

    #[tokio::test]
    async fn a_legacy_question_parks_its_call_once_and_is_answered_from_the_decision() {
        let waiters = Arc::new(LegacyWaiters::default());
        let mut call = parked(&[("7", "Send it?")]);
        call.kind = ParkedCallKind::Legacy;
        let (receiver, guard) = waiters.register("email", "call-1");
        let Legacy::Park(released) = waiters.on_question("email", call.clone()) else {
            panic!("the first question parks the call")
        };
        assert_eq!(receiver.await.unwrap(), call);
        drop(guard);
        released.cancelled().await;

        // The next attempt is answered from the decision when it asks the same question.
        let (_receiver, _guard) = waiters.register("email", "call-1");
        let digest = question_digest(&call.input_requests["7"]);
        waiters.stash("email", "call-1", digest, json!({ "action": "accept" }));
        let mut asked_again = call.clone();
        asked_again.input_requests = [("9".to_string(), form("Send it?"))].into();
        assert!(matches!(waiters.on_question("email", asked_again), Legacy::Answer(_)));
        assert!(matches!(waiters.on_question("other", call), Legacy::Unclaimed));
    }
}
