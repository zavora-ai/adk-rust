//! The plan graph (spec 3.1 §7.11, feature `graph`).
//!
//! An approved `exit_plan_mode` with `steps` compiles to a checkpointed `StateGraph`: one
//! node per step running the harness agent with the step's instruction, edges from
//! `depends_on`, `interrupt_before` on every `Approve` gate, and a reviewer node after every
//! `Review` gate. From the user's next turn, while the session is in Ask or Auto, each turn
//! runs the graph instead of a free-form turn, until the plan is done.
//!
//! | Gate | Node | What happens |
//! |---|---|---|
//! | `None` | `step:<id>` | the step runs |
//! | `Approve` | `step:<id>`, with `interrupt_before` | the run pauses on a held call (`plan_step_gate`) whose fingerprint binds the plan hash and the step id; `Harness::decide` answers it |
//! | `Review` | `step:<id>` → `review:<id>` | the step runs, then the reviewer member reviews it in an isolated delegation |
//!
//! Checkpoints live in the harness's [`Checkpointer`], so a step survives process loss, and
//! [`PlanGraphs::rewind`] forks a thread at the checkpoint after a step with
//! `TimeTravelHandle::fork_at`, so the steps before it never run again.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};

use adk_core::audit_log::AuditActor;
use adk_core::{
    AdkError, Agent, ApprovalScope, Content, ErrorCategory, ErrorComponent, Event, EventStream,
    FunctionResponseData, InvocationContext, Part, Result, ToolConfirmationDecision,
    ToolConfirmationRequest, ToolContext,
};
use adk_graph::agent::session_thread_id;
use adk_graph::{
    Checkpoint, Checkpointer, CompiledGraph, END, ExecutionConfig, GraphError, Interrupt, Node,
    NodeContext, NodeOutput, START, State, StateGraph, StateSchema,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::audit::HarnessAudit;
use crate::run_context::ScopedContext;
use crate::tools::{PlanGate, PlanStep};

/// Name of the held call an `Approve` gate surfaces (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::PLAN_GATE_TOOL, "plan_step_gate");
/// ```
pub const PLAN_GATE_TOOL: &str = "plan_step_gate";

/// The node of plan step `id`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::plan_step_node("2"), "step:2");
/// ```
pub fn plan_step_node(id: &str) -> String {
    format!("step:{id}")
}

/// The reviewer node after plan step `id`, for a `Review` gate.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::plan_review_node("2"), "review:2");
/// ```
pub fn plan_review_node(id: &str) -> String {
    format!("review:{id}")
}

/// State key a step writes when it completes.
fn done_key(id: &str) -> String {
    format!("done:{id}")
}

/// The shape a plan compiles to (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// use adk_harness::{PlanGate, PlanStep, compile_plan};
///
/// let step = |id: &str, deps: &[&str], gate| PlanStep {
///     id: id.into(),
///     title: id.into(),
///     instruction: format!("do {id}"),
///     depends_on: deps.iter().map(|d| d.to_string()).collect(),
///     gate,
/// };
/// let shape = compile_plan(
///     &[step("1", &[], PlanGate::None), step("2", &["1"], PlanGate::Approve)],
///     "abc",
/// )
/// .unwrap();
/// assert_eq!(shape.nodes, vec!["step:1", "step:2"]);
/// assert_eq!(shape.interrupt_before, vec!["step:2"]);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanGraphShape {
    /// The approved plan's hash.
    pub plan_hash: String,
    /// Node names in a dependency order: `step:<id>` per step and `review:<id>` after a
    /// `Review` gate.
    pub nodes: Vec<String>,
    /// Edges, `__start__` and `__end__` included, sorted.
    pub edges: Vec<(String, String)>,
    /// Nodes the run pauses before: every `Approve` gate.
    pub interrupt_before: Vec<String>,
    /// Super-steps one run may take: the node count plus one.
    pub recursion_limit: usize,
}

impl PlanGraphShape {
    /// The graph as a `WorkflowSchema`, the interchange format of adk-graph (spec 3.1
    /// §7.11): every node is an agent node.
    pub fn workflow_schema(&self) -> adk_graph::workflow::WorkflowSchema {
        adk_graph::workflow::WorkflowSchema {
            edges: self
                .edges
                .iter()
                .map(|(from, to)| adk_graph::workflow::WorkflowEdge {
                    from: from.clone(),
                    to: to.clone(),
                    condition: None,
                    from_port: None,
                    to_port: None,
                })
                .collect(),
            conditions: Vec::new(),
            action_nodes: HashMap::new(),
            agent_nodes: self.nodes.clone(),
        }
    }
}

fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "harness.plan.invalid",
        message,
    )
}

/// Compiles plan steps to the plan graph's shape (spec 3.1 §7.11).
///
/// # Errors
///
/// Returns `harness.plan.invalid` when there are no steps, an id is empty or repeated, a
/// step depends on itself or on an unknown step, or the dependencies form a cycle.
pub fn compile_plan(steps: &[PlanStep], plan_hash: &str) -> Result<PlanGraphShape> {
    if steps.is_empty() {
        return Err(invalid("a plan graph needs at least one step"));
    }
    let mut index = HashMap::new();
    for (position, step) in steps.iter().enumerate() {
        if step.id.trim().is_empty() {
            return Err(invalid("step ids must not be empty"));
        }
        if index.insert(step.id.as_str(), position).is_some() {
            return Err(invalid(format!("step id '{}' is used twice", step.id)));
        }
    }
    for step in steps {
        for dep in &step.depends_on {
            if dep == &step.id {
                return Err(invalid(format!("step '{}' depends on itself", step.id)));
            }
            if !index.contains_key(dep.as_str()) {
                return Err(invalid(format!(
                    "step '{}' depends on an unknown step '{dep}'",
                    step.id
                )));
            }
        }
    }
    // Kahn's algorithm in declaration order, so the node order is stable.
    let mut indegree: Vec<usize> = steps.iter().map(|step| step.depends_on.len()).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); steps.len()];
    for (position, step) in steps.iter().enumerate() {
        for dep in &step.depends_on {
            dependents[index[dep.as_str()]].push(position);
        }
    }
    let mut ready: VecDeque<usize> = (0..steps.len()).filter(|i| indegree[*i] == 0).collect();
    let mut order = Vec::with_capacity(steps.len());
    while let Some(next) = ready.pop_front() {
        order.push(next);
        for &dependent in &dependents[next] {
            indegree[dependent] -= 1;
            if indegree[dependent] == 0 {
                ready.push_back(dependent);
            }
        }
    }
    if order.len() != steps.len() {
        return Err(invalid("the steps' dependencies form a cycle"));
    }
    let exit = |step: &PlanStep| match step.gate {
        PlanGate::Review => plan_review_node(&step.id),
        PlanGate::None | PlanGate::Approve => plan_step_node(&step.id),
    };
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut interrupt_before = Vec::new();
    for &position in &order {
        let step = &steps[position];
        nodes.push(plan_step_node(&step.id));
        match step.gate {
            PlanGate::Review => {
                nodes.push(plan_review_node(&step.id));
                edges.push((plan_step_node(&step.id), plan_review_node(&step.id)));
            }
            PlanGate::Approve => interrupt_before.push(plan_step_node(&step.id)),
            PlanGate::None => {}
        }
        if step.depends_on.is_empty() {
            edges.push((START.to_string(), plan_step_node(&step.id)));
        }
        for dep in &step.depends_on {
            edges.push((exit(&steps[index[dep.as_str()]]), plan_step_node(&step.id)));
        }
        if dependents[position].is_empty() {
            edges.push((exit(step), END.to_string()));
        }
    }
    edges.sort();
    edges.dedup();
    let recursion_limit = nodes.len() + 1;
    Ok(PlanGraphShape {
        plan_hash: plan_hash.to_string(),
        nodes,
        edges,
        interrupt_before,
        recursion_limit,
    })
}

/// Where an installed plan stands (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// assert_eq!(serde_json::to_value(adk_harness::PlanRunStatus::Done).unwrap(), "done");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanRunStatus {
    /// Turns run the plan graph.
    Running,
    /// Every step ran.
    Done,
    /// A gate was denied.
    Cancelled,
}

/// The plan graph a session runs (spec 3.1 §7.11), kept in the harness's checkpointer.
///
/// # Example
///
/// ```rust,ignore
/// if let Some(plan) = graphs.active("app", "alice", "s1").await? {
///     println!("{} steps on {}", plan.steps.len(), plan.thread_id);
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivePlan {
    /// The approved plan's hash.
    pub plan_hash: String,
    /// The approved plan.
    pub plan: String,
    /// Its steps.
    pub steps: Vec<PlanStep>,
    /// The graph thread turns run on.
    pub thread_id: String,
    /// Where it stands.
    pub status: PlanRunStatus,
    /// How many times it was rewound; part of every gate's fingerprint, so a step that runs
    /// again after a rewind is approved again.
    pub rewinds: u32,
}

/// The plan graphs of a harness (spec 3.1 §7.11): the store of installed plans and the
/// runner of their turns.
///
/// # Example
///
/// ```rust,ignore
/// let graphs = harness.runtime().plan_graph().expect("feature graph, plan_graph on");
/// let new_thread = graphs.rewind("app", "alice", "s1", "2").await?;
/// ```
pub struct PlanGraphs {
    checkpointer: Arc<dyn Checkpointer>,
    harness: String,
    reviewer: OnceLock<Arc<dyn Agent>>,
}

impl std::fmt::Debug for PlanGraphs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanGraphs")
            .field("harness", &self.harness)
            .field("reviewer", &self.reviewer.get().map(|agent| agent.name().to_string()))
            .finish_non_exhaustive()
    }
}

/// Something the turn's stream yields for a node: an event, or one the node waits on until
/// the caller has taken it (and a runner has recorded it).
pub(crate) enum Forward {
    Event(Event),
    Synced(Event, oneshot::Sender<()>),
}

pub(crate) type Sink = mpsc::UnboundedSender<Forward>;

/// Streams the events a graph run's nodes forward while it runs, then what `finish` makes
/// of its outcome.
pub(crate) fn drive<F, G, S>(
    run: F,
    mut forwarded: mpsc::UnboundedReceiver<Forward>,
    finish: G,
) -> EventStream
where
    F: std::future::Future<Output = adk_graph::Result<State>> + Send + 'static,
    G: FnOnce(adk_graph::Result<State>) -> S + Send + 'static,
    S: futures::Stream<Item = Result<Event>> + Send + 'static,
{
    Box::pin(async_stream::stream! {
        let outcome = {
            tokio::pin!(run);
            loop {
                tokio::select! {
                    biased;
                    Some(next) = forwarded.recv() => match next {
                        Forward::Event(event) => yield Ok(event),
                        Forward::Synced(event, taken) => {
                            yield Ok(event);
                            let _ = taken.send(());
                        }
                    },
                    outcome = &mut run => break outcome,
                }
            }
        };
        while let Ok(next) = forwarded.try_recv() {
            match next {
                Forward::Event(event) => yield Ok(event),
                Forward::Synced(event, taken) => {
                    yield Ok(event);
                    let _ = taken.send(());
                }
            }
        }
        let tail = finish(outcome);
        tokio::pin!(tail);
        while let Some(item) = futures::StreamExt::next(&mut tail).await {
            yield item;
        }
    })
}

/// Runs the harness agent `root` as graph node `node`, with `content` as the turn's user
/// content, forwarding its events. Returns whether it held a call, and its last answer.
pub(crate) async fn run_agent_node(
    node: &str,
    root: &Arc<dyn Agent>,
    harness: &str,
    parent: Arc<dyn InvocationContext>,
    content: Content,
    sink: &Sink,
) -> adk_graph::Result<(bool, String)> {
    run_agent_node_counted(node, root, harness, parent, content, sink)
        .await
        .map(|(held, output, _)| (held, output))
}

/// [`run_agent_node`], also returning how many tool calls the harness made, for the goal
/// stall rule (spec 3.2 §10 HK3).
pub(crate) async fn run_agent_node_counted(
    node: &str,
    root: &Arc<dyn Agent>,
    harness: &str,
    parent: Arc<dyn InvocationContext>,
    content: Content,
    sink: &Sink,
) -> adk_graph::Result<(bool, String, usize)> {
    let scoped = Arc::new(
        ScopedContext::new(parent)
            .for_agent(root.clone(), harness.to_string())
            .with_user_content(content),
    );
    let mut stream = root.run(scoped).await.map_err(|error| failed(node, error))?;
    let mut held = false;
    let mut output = String::new();
    let mut tool_calls = 0;
    while let Some(item) = futures::StreamExt::next(&mut stream).await {
        let event = item.map_err(|error| failed(node, error))?;
        held |= event.actions.tool_confirmation.is_some();
        if event.author == harness
            && !event.llm_response.partial
            && let Some(content) = &event.llm_response.content
        {
            let text: String = content.parts.iter().filter_map(Part::text).collect();
            if !text.is_empty() {
                output = text;
            }
            tool_calls += content
                .parts
                .iter()
                .filter(|part| matches!(part, Part::FunctionCall { .. }))
                .count();
        }
        let _ = sink.send(Forward::Event(event));
    }
    Ok((held, output, tool_calls))
}

impl PlanGraphs {
    pub(crate) fn new(checkpointer: Arc<dyn Checkpointer>, harness: &str) -> Self {
        Self { checkpointer, harness: harness.to_string(), reviewer: OnceLock::new() }
    }

    #[cfg(feature = "team")]
    pub(crate) fn set_reviewer(&self, reviewer: Arc<dyn Agent>) {
        let _ = self.reviewer.set(reviewer);
    }

    /// The checkpointer plans and their graph threads are kept in.
    pub fn checkpointer(&self) -> &Arc<dyn Checkpointer> {
        &self.checkpointer
    }

    /// The agent `Review` gates delegate to, if the harness has one.
    pub fn reviewer(&self) -> Option<&Arc<dyn Agent>> {
        self.reviewer.get()
    }

    fn pointer_thread(app: &str, user: &str, session: &str) -> String {
        format!("{}:harness-plan", session_thread_id(app, user, session))
    }

    /// The plan installed for the session, in any status.
    ///
    /// # Errors
    ///
    /// Returns the checkpointer's error.
    pub async fn active(&self, app: &str, user: &str, session: &str) -> Result<Option<ActivePlan>> {
        let Some(checkpoint) =
            self.checkpointer.load(&Self::pointer_thread(app, user, session)).await?
        else {
            return Ok(None);
        };
        Ok(checkpoint
            .state
            .get("plan")
            .and_then(|value| serde_json::from_value(value.clone()).ok()))
    }

    async fn save(&self, app: &str, user: &str, session: &str, plan: &ActivePlan) -> Result<()> {
        let thread = Self::pointer_thread(app, user, session);
        let version = self.checkpointer.load(&thread).await?.map_or(1, |head| head.version + 1);
        let mut state = State::new();
        state.insert(
            "plan".to_string(),
            serde_json::to_value(plan).map_err(|error| invalid(error.to_string()))?,
        );
        let checkpoint = Checkpoint::new(&thread, state, 0, Vec::new()).with_version(version);
        self.checkpointer.save(&checkpoint).await?;
        Ok(())
    }

    /// Checks that `steps` compile, and that a `Review` gate has a reviewer.
    pub(crate) fn validate(&self, steps: &[PlanStep], plan_hash: &str) -> Result<PlanGraphShape> {
        let shape = compile_plan(steps, plan_hash)?;
        if self.reviewer.get().is_none()
            && let Some(step) = steps.iter().find(|step| step.gate == PlanGate::Review)
        {
            return Err(invalid(format!(
                "step '{}' has a review gate, but the harness has no reviewer member",
                step.id
            )));
        }
        Ok(shape)
    }

    /// Installs an approved plan for the session: from its next turn in Ask or Auto, turns
    /// run the plan graph. Records `harness.plan.compiled` (spec 3.1 §9.3).
    pub(crate) async fn install(
        &self,
        ctx: &dyn ToolContext,
        plan: &str,
        steps: &[PlanStep],
        plan_hash: &str,
    ) -> Result<PlanGraphShape> {
        let shape = self.validate(steps, plan_hash)?;
        let (app, user, session) = (ctx.app_name(), ctx.user_id(), ctx.session_id());
        let short = &plan_hash[..plan_hash.len().min(16)];
        let active = ActivePlan {
            plan_hash: plan_hash.to_string(),
            plan: plan.to_string(),
            steps: steps.to_vec(),
            thread_id: format!("{}:plan:{short}", session_thread_id(app, user, session)),
            status: PlanRunStatus::Running,
            rewinds: 0,
        };
        if let Some(audit) = HarnessAudit::for_tool(ctx) {
            audit
                .record(
                    "harness.plan.compiled",
                    json!({
                        "planHash": plan_hash,
                        "steps": steps.iter().map(|step| json!({
                            "id": step.id,
                            "title": step.title,
                            "dependsOn": step.depends_on,
                            "gate": step.gate,
                        })).collect::<Vec<_>>(),
                        "nodes": shape.nodes,
                        "interruptBefore": shape.interrupt_before,
                    }),
                )
                .await?;
        }
        self.save(app, user, session, &active).await?;
        tracing::info!(plan.hash = %plan_hash, plan.steps = steps.len(), "plan graph installed");
        Ok(shape)
    }

    /// Rewinds the session's plan to just after `step_id` completed (spec 3.1 §7.11, H4):
    /// forks its thread at that checkpoint with `TimeTravelHandle::fork_at` and makes the
    /// fork the thread the next turn runs. Nothing up to `step_id` runs again; the steps
    /// after it run on the next turn, their `Approve` gates asking again.
    ///
    /// Returns the new thread's id.
    ///
    /// # Errors
    ///
    /// Returns `harness.plan.invalid` when the session has no plan, the plan has no step
    /// `step_id`, or no checkpoint shows the step completed.
    pub async fn rewind(
        &self,
        app: &str,
        user: &str,
        session: &str,
        step_id: &str,
    ) -> Result<String> {
        let mut plan = self
            .active(app, user, session)
            .await?
            .ok_or_else(|| invalid("the session has no plan to rewind"))?;
        if !plan.steps.iter().any(|step| step.id == step_id) {
            return Err(invalid(format!("the plan has no step '{step_id}'")));
        }
        let shape = compile_plan(&plan.steps, &plan.plan_hash)?;
        let mut graph = StateGraph::new(StateSchema::simple(&[]));
        for node in &shape.nodes {
            graph = graph.add_node_fn(node, |_ctx| async { Ok(NodeOutput::new()) });
        }
        for (from, to) in &shape.edges {
            graph = graph.add_edge(from, to);
        }
        let graph = graph.compile()?.with_checkpointer_arc(self.checkpointer.clone());
        let handle = graph.time_travel(&plan.thread_id)?;
        let key = done_key(step_id);
        let step = handle
            .steps()
            .await?
            .into_iter()
            .find(|info| info.state_keys.contains(&key))
            .ok_or_else(|| invalid(format!("step '{step_id}' has not completed on this plan")))?;
        let base = plan.thread_id.split(":rewind:").next().unwrap_or(&plan.thread_id).to_string();
        let thread = format!("{base}:rewind:{}", plan.rewinds + 1);
        handle.fork_at(step.step, &thread).await?;
        plan.thread_id = thread.clone();
        plan.rewinds += 1;
        plan.status = PlanRunStatus::Running;
        self.save(app, user, session, &plan).await?;
        tracing::info!(plan.hash = %plan.plan_hash, plan.step = step_id, plan.thread = %thread, "plan rewound");
        Ok(thread)
    }

    /// Runs this turn of the session's plan, when one is running and the mode lets it.
    pub(crate) async fn route(
        self: &Arc<Self>,
        mode: crate::mode::HarnessMode,
        root: Arc<dyn Agent>,
        ctx: Arc<dyn InvocationContext>,
    ) -> Result<Option<EventStream>> {
        use crate::mode::HarnessMode;
        if !matches!(mode, HarnessMode::Ask | HarnessMode::Auto) {
            return Ok(None);
        }
        let Some(plan) = self.active(ctx.app_name(), ctx.user_id(), ctx.session_id()).await? else {
            return Ok(None);
        };
        if plan.status != PlanRunStatus::Running {
            return Ok(None);
        }
        Ok(Some(self.clone().run_turn(plan, root, ctx)?))
    }

    fn build(
        &self,
        plan: &ActivePlan,
        shape: &PlanGraphShape,
        root: Arc<dyn Agent>,
        sink: Sink,
    ) -> Result<CompiledGraph> {
        let gates = Arc::new(GateBook {
            author: plan_author(&self.harness),
            plan_hash: plan.plan_hash.clone(),
            rewinds: plan.rewinds,
        });
        let mut graph = StateGraph::new(StateSchema::simple(&["input"]));
        for step in &plan.steps {
            graph = graph.add_node(StepNode {
                node: plan_step_node(&step.id),
                step: step.clone(),
                harness: self.harness.clone(),
                root: root.clone(),
                gates: gates.clone(),
                sink: sink.clone(),
            });
            if step.gate == PlanGate::Review {
                let reviewer = self.reviewer.get().cloned().ok_or_else(|| {
                    invalid(format!("step '{}' has a review gate but no reviewer", step.id))
                })?;
                graph = graph.add_node(ReviewNode {
                    node: plan_review_node(&step.id),
                    step: step.clone(),
                    reviewer,
                    gates: gates.clone(),
                    sink: sink.clone(),
                });
            }
        }
        for (from, to) in &shape.edges {
            graph = graph.add_edge(from, to);
        }
        let interrupt: Vec<&str> = shape.interrupt_before.iter().map(String::as_str).collect();
        Ok(graph
            .compile()?
            .with_checkpointer_arc(self.checkpointer.clone())
            .with_interrupt_before(&interrupt)
            .with_recursion_limit(shape.recursion_limit))
    }

    fn run_turn(
        self: Arc<Self>,
        plan: ActivePlan,
        root: Arc<dyn Agent>,
        ctx: Arc<dyn InvocationContext>,
    ) -> Result<EventStream> {
        let shape = compile_plan(&plan.steps, &plan.plan_hash)?;
        let (sink, forwarded) = mpsc::unbounded_channel();
        let graph = self.build(&plan, &shape, root, sink)?;
        let gates = GateBook {
            author: plan_author(&self.harness),
            plan_hash: plan.plan_hash.clone(),
            rewinds: plan.rewinds,
        };
        let config = ExecutionConfig::new(&plan.thread_id).with_parent_context(ctx.clone());
        tracing::info!(plan.hash = %plan.plan_hash, plan.thread = %plan.thread_id, "running the plan graph");
        let run = async move { graph.invoke(State::new(), config).await };
        Ok(drive(run, forwarded, move |outcome| {
            async_stream::stream! {
                let (app, user, session) = (ctx.app_name(), ctx.user_id(), ctx.session_id());
                match outcome {
                    Ok(state) => {
                        let cancelled =
                            state.get("planStatus").and_then(Value::as_str) == Some("cancelled");
                        let mut finished = plan.clone();
                        finished.status =
                            if cancelled { PlanRunStatus::Cancelled } else { PlanRunStatus::Done };
                        if let Err(error) = self.save(app, user, session, &finished).await {
                            yield Err(error);
                            return;
                        }
                        let text = if cancelled {
                            format!("The plan stopped: a step's gate was denied ({} steps planned).", plan.steps.len())
                        } else {
                            format!("The plan is complete: {} steps ran.", plan.steps.len())
                        };
                        yield Ok(gates.note(ctx.as_ref(), text));
                    }
                    Err(GraphError::Interrupted(interrupted)) => {
                        for interrupt in &interrupted.interrupts {
                            // A gate the executor paused before: surface it as a held call.
                            if let Interrupt::Before(node) = interrupt
                                && let Some(step) =
                                    plan.steps.iter().find(|step| plan_step_node(&step.id) == *node)
                            {
                                match gates.pause(ctx.as_ref(), step).await {
                                    Ok(events) => {
                                        for event in events {
                                            yield Ok(event);
                                        }
                                    }
                                    Err(error) => {
                                        yield Err(error);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => yield Err(error.into()),
                }
            }
        }))
    }
}

/// Author of the plan's own events: gate holds, their answers and plan notes.
fn plan_author(harness: &str) -> String {
    format!("{harness}_plan")
}

/// The `Approve` gates of one plan run.
struct GateBook {
    author: String,
    plan_hash: String,
    rewinds: u32,
}

impl GateBook {
    fn call_id(&self, step: &PlanStep) -> String {
        format!(
            "plan-gate-{}-{}-{}",
            &self.plan_hash[..self.plan_hash.len().min(12)],
            self.rewinds,
            step.id
        )
    }

    /// The request a person decides; its fingerprint binds the plan hash, the run and the
    /// step, so an edited plan or a rewound step needs a new approval.
    fn request(&self, ctx: &dyn InvocationContext, step: &PlanStep) -> ToolConfirmationRequest {
        ToolConfirmationRequest::new(
            PLAN_GATE_TOOL,
            json!({
                "planHash": self.plan_hash,
                "run": self.rewinds,
                "stepId": step.id,
                "title": step.title,
                "instruction": step.instruction,
            }),
        )
        .with_function_call_id(self.call_id(step))
        .with_acting_context(ctx.acting_context())
    }

    fn audit(ctx: &dyn InvocationContext) -> Option<HarnessAudit> {
        let trail = ctx.run_config().audit.clone()?;
        Some(HarnessAudit::new(trail, AuditActor::for_context(ctx), ctx.session_id()))
    }

    async fn record(
        &self,
        ctx: &dyn InvocationContext,
        step: &PlanStep,
        decision: &str,
    ) -> Result<()> {
        if let Some(audit) = Self::audit(ctx) {
            audit
                .record(
                    "harness.plan.step",
                    json!({
                        "planHash": self.plan_hash,
                        "stepId": step.id,
                        "gate": step.gate,
                        "decision": decision,
                        "run": self.rewinds,
                    }),
                )
                .await?;
        }
        Ok(())
    }

    fn event(&self, ctx: &dyn InvocationContext) -> Event {
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.author.clone();
        event.branch = ctx.branch().to_string();
        event
    }

    fn note(&self, ctx: &dyn InvocationContext, text: String) -> Event {
        let mut event = self.event(ctx);
        event.llm_response.content = Some(Content::new("model").with_text(text));
        event
    }

    /// How the session answered the gate, if it did: `Some(true)` approved,
    /// `Some(false)` denied.
    fn answered(&self, ctx: &dyn InvocationContext, step: &PlanStep) -> Option<bool> {
        let call_id = self.call_id(step);
        let mut answer = None;
        for content in ctx.session().conversation_history() {
            for part in &content.parts {
                if let Part::FunctionResponse { function_response, id, .. } = part
                    && id.as_deref() == Some(call_id.as_str())
                    && function_response.name == PLAN_GATE_TOOL
                {
                    answer = function_response
                        .response
                        .get("status")
                        .and_then(Value::as_str)
                        .map(|status| status == "approved");
                }
            }
        }
        answer
    }

    /// Whether the gate's call is held in the session now.
    fn held(&self, ctx: &dyn InvocationContext) -> Vec<String> {
        ctx.session()
            .held_tool_calls(&self.author, ctx.branch())
            .into_iter()
            .filter_map(|call| call.request.function_call_id)
            .collect()
    }

    /// Surfaces the gate as a held call for a person to decide, and records it pending.
    /// Returns the hold's events — the call, then its pending response carrying the
    /// request, as an agent holds a call — or none when the call is held already.
    async fn pause(&self, ctx: &dyn InvocationContext, step: &PlanStep) -> Result<Vec<Event>> {
        let request = self.request(ctx, step);
        if let Some(store) = &ctx.run_config().approval_store {
            store
                .record_pending(
                    &ApprovalScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id()),
                    &request,
                )
                .await?;
        }
        if self.held(ctx).contains(&self.call_id(step)) {
            return Ok(Vec::new());
        }
        self.record(ctx, step, "pending").await?;
        tracing::info!(plan.step = %step.id, "plan step waits for approval");
        let mut call = self.event(ctx);
        call.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: PLAN_GATE_TOOL.to_string(),
                args: request.args.clone(),
                id: request.function_call_id.clone(),
                thought_signature: None,
            }],
        });
        let mut held = self.event(ctx);
        held.llm_response.content = Some(Content {
            role: "function".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    PLAN_GATE_TOOL,
                    adk_core::confirmation_pending_response(format!(
                        "plan step '{}' waits for approval",
                        step.id
                    )),
                ),
                id: request.function_call_id.clone(),
                annotations: None,
            }],
        });
        held.llm_response.turn_complete = true;
        held.actions.tool_confirmation = Some(request);
        Ok(vec![call, held])
    }

    /// The gate's decision from the approval store, consumed: `Some(true)` approved.
    async fn take(&self, ctx: &dyn InvocationContext, step: &PlanStep) -> Result<Option<bool>> {
        let Some(store) = &ctx.run_config().approval_store else {
            return Ok(None);
        };
        let scope = ApprovalScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id());
        let fingerprint = self.request(ctx, step).fingerprint();
        Ok(store
            .take_approval(&scope, &fingerprint)
            .await?
            .map(|approval| approval.decision == ToolConfirmationDecision::Approve))
    }

    fn answer(&self, ctx: &dyn InvocationContext, step: &PlanStep, approved: bool) -> Event {
        let mut event = self.event(ctx);
        let status = if approved { "approved" } else { "denied" };
        event.llm_response.content = Some(Content {
            role: "function".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    PLAN_GATE_TOOL,
                    json!({ "status": status, "stepId": step.id }),
                ),
                id: Some(self.call_id(step)),
                annotations: None,
            }],
        });
        event
    }
}

/// Sends `event` and waits until the turn's caller has taken it, so a runner has recorded
/// it before the step's agent reads the session.
async fn send_synced(sink: &Sink, event: Event) {
    let (taken, waited) = oneshot::channel();
    if sink.send(Forward::Synced(event, taken)).is_ok() {
        let _ = waited.await;
    }
}

fn failed(node: &str, error: AdkError) -> GraphError {
    GraphError::AgentFailed { node: node.to_string(), source: Box::new(error) }
}

fn node_error(node: &str, message: impl Into<String>) -> GraphError {
    GraphError::NodeExecutionFailed { node: node.to_string(), message: message.into() }
}

/// One plan step: runs the harness agent with the step's instruction (spec 3.1 §7.11).
struct StepNode {
    node: String,
    step: PlanStep,
    harness: String,
    root: Arc<dyn Agent>,
    gates: Arc<GateBook>,
    sink: Sink,
}

impl StepNode {
    fn instruction(&self) -> String {
        format!(
            "[plan step {id}: {title} — plan {hash}, run {run}]\n{instruction}\n\nDo this step \
             only, then stop and summarize what you did.",
            id = self.step.id,
            title = self.step.title,
            hash = &self.gates.plan_hash[..self.gates.plan_hash.len().min(12)],
            run = self.gates.rewinds,
            instruction = self.step.instruction,
        )
    }

    fn started(&self, ctx: &dyn InvocationContext, instruction: &str) -> bool {
        ctx.session().conversation_history().iter().any(|content| {
            content.role == "user"
                && content.parts.iter().any(|part| part.text() == Some(instruction))
        })
    }
}

#[async_trait::async_trait]
impl Node for StepNode {
    fn name(&self) -> &str {
        &self.node
    }

    fn description(&self) -> &str {
        &self.step.title
    }

    async fn execute(&self, ctx: &NodeContext) -> adk_graph::Result<NodeOutput> {
        let parent =
            ctx.config.parent_context.clone().ok_or_else(|| {
                node_error(&self.node, "a plan step runs only inside a harness turn")
            })?;
        if self.step.gate == PlanGate::Approve {
            match self.gates.answered(parent.as_ref(), &self.step) {
                Some(true) => {}
                Some(false) => {
                    return Ok(NodeOutput::new()
                        .with_update("planStatus", "cancelled")
                        .with_goto([END]));
                }
                None => match self
                    .gates
                    .take(parent.as_ref(), &self.step)
                    .await
                    .map_err(|e| failed(&self.node, e))?
                {
                    Some(approved) => {
                        let decision = if approved { "approved" } else { "denied" };
                        self.gates
                            .record(parent.as_ref(), &self.step, decision)
                            .await
                            .map_err(|e| failed(&self.node, e))?;
                        send_synced(
                            &self.sink,
                            self.gates.answer(parent.as_ref(), &self.step, approved),
                        )
                        .await;
                        if !approved {
                            return Ok(NodeOutput::new()
                                .with_update("planStatus", "cancelled")
                                .with_goto([END]));
                        }
                    }
                    None => {
                        for event in self
                            .gates
                            .pause(parent.as_ref(), &self.step)
                            .await
                            .map_err(|e| failed(&self.node, e))?
                        {
                            let _ = self.sink.send(Forward::Event(event));
                        }
                        return Ok(NodeOutput::interrupt_with_data(
                            "plan step waits for approval",
                            json!({ "stepId": self.step.id }),
                        ));
                    }
                },
            }
        }

        let instruction = self.instruction();
        let content = Content::new("user").with_text(instruction.clone());
        if !self.started(parent.as_ref(), &instruction) {
            let mut event = Event::new(parent.invocation_id());
            event.author = "user".to_string();
            event.branch = parent.branch().to_string();
            event.llm_response.content = Some(content.clone());
            send_synced(&self.sink, event).await;
        }
        let (held, output) =
            run_agent_node(&self.node, &self.root, &self.harness, parent, content, &self.sink)
                .await?;
        if held {
            return Ok(NodeOutput::interrupt_with_data(
                "plan step holds a tool call",
                json!({ "stepId": self.step.id }),
            ));
        }
        Ok(NodeOutput::new().with_update(&done_key(&self.step.id), json!({ "output": output })))
    }
}

/// The reviewer after a `Review` gate: an isolated delegation to the reviewer member
/// (spec 3.1 §7.10, §7.11).
struct ReviewNode {
    node: String,
    step: PlanStep,
    reviewer: Arc<dyn Agent>,
    gates: Arc<GateBook>,
    sink: Sink,
}

#[async_trait::async_trait]
impl Node for ReviewNode {
    fn name(&self) -> &str {
        &self.node
    }

    fn description(&self) -> &str {
        "Reviews the step before the plan continues."
    }

    async fn execute(&self, ctx: &NodeContext) -> adk_graph::Result<NodeOutput> {
        let parent =
            ctx.config.parent_context.clone().ok_or_else(|| {
                node_error(&self.node, "a review runs only inside a harness turn")
            })?;
        let output = review(parent.clone(), &self.reviewer, &self.step, &self.sink).await.map_err(
            |error| GraphError::AgentFailed { node: self.node.clone(), source: Box::new(error) },
        )?;
        self.gates
            .record(parent.as_ref(), &self.step, "reviewed")
            .await
            .map_err(|e| failed(&self.node, e))?;
        Ok(NodeOutput::new().with_update(&format!("review:{}", self.step.id), output))
    }
}

#[cfg(feature = "team")]
async fn review(
    parent: Arc<dyn InvocationContext>,
    reviewer: &Arc<dyn Agent>,
    step: &PlanStep,
    sink: &Sink,
) -> Result<Value> {
    use adk_core::Tool;
    let tool = adk_tool::AgentTool::new(reviewer.clone())
        .session_snapshot(adk_tool::AgentToolSessionSnapshot::Isolated)
        .forward_artifacts(false)
        .forward_shared_state(false)
        .forward_memory(false)
        .forward_events(true)
        .history_max_events(0)
        .state_keys(Vec::<String>::new())
        .max_delegation_depth(1)
        .timeout(crate::team::MEMBER_TIMEOUT);
    let sink = sink.clone();
    let tool_ctx = Arc::new(
        crate::run_context::InvocationToolContext::new(parent, format!("plan-review-{}", step.id))
            .with_event_sink(Arc::new(move |event| {
                let _ = sink.send(Forward::Event(event));
            })),
    );
    tool.execute(
        tool_ctx,
        json!({
            "request": format!(
                "Review the work of plan step {}: {}.\nThe step's instruction was:\n{}\n\nReport \
                 problems with file paths, or say it is ready.",
                step.id, step.title, step.instruction
            )
        }),
    )
    .await
}

#[cfg(not(feature = "team"))]
async fn review(
    _parent: Arc<dyn InvocationContext>,
    _reviewer: &Arc<dyn Agent>,
    step: &PlanStep,
    _sink: &Sink,
) -> Result<Value> {
    Err(invalid(format!("step '{}' needs a reviewer member, which needs feature `team`", step.id)))
}
