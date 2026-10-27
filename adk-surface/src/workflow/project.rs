//! The observer's projection (spec 3.3 §12): runs, steps and step details from a thread's
//! checkpoints, its effect log and the inbox matches the caller found. Pure: no store is read
//! here, so the web pane, the tests and any other renderer get the same views.

use std::collections::{BTreeMap, BTreeSet};

use adk_graph::edge::END;
use adk_graph::governed::EffectRecord;
use adk_graph::record::{NodeRecord, NodeStatus, RunRecord, StepKind, StepRecord};
use adk_graph::shape::{GraphShape, NodeKind};
use adk_graph::state::Checkpoint;
use chrono::{DateTime, FixedOffset, Utc};
use serde_json::Value;

use super::values::Display;
use super::view::{
    EdgeView, EffectView, ForkPoint, ForkSummary, GraphView, NodeDecision, NodeDetail, NodeRun,
    NodeState, NodeView, RunLineage, RunStatus, RunStep, RunSummary, StepDetail, StepState,
    WorkflowRun,
};
use super::words;

/// Everything a projection reads besides the checkpoints.
pub(crate) struct Context<'a> {
    pub(crate) session: &'a str,
    pub(crate) thread: &'a str,
    pub(crate) graph: &'a str,
    pub(crate) shape: &'a GraphShape,
    /// The graph's declared channels: a fork's editor lists them even when unset.
    pub(crate) channels: &'a [String],
    pub(crate) offset: FixedOffset,
    pub(crate) display: &'a Display,
}

impl Context<'_> {
    pub(crate) fn hh_mm(&self, at: DateTime<Utc>) -> String {
        at.with_timezone(&self.offset).format("%H:%M").to_string()
    }

    fn hh_mm_ss(&self, at: DateTime<Utc>) -> String {
        at.with_timezone(&self.offset).format("%H:%M:%S").to_string()
    }

    fn kind(&self, node: &str) -> NodeKind {
        self.shape
            .nodes
            .iter()
            .find(|shape| shape.name == node)
            .map_or(NodeKind::Other, |shape| shape.kind)
    }

    fn governed(&self, node: &str) -> bool {
        self.shape.nodes.iter().any(|shape| shape.name == node && shape.governed.is_some())
    }
}

/// A node's effect as its governed bracket treats it: a governed node that declares none
/// has one (adk-graph's `governed` module).
pub(crate) fn effect_of(node: &adk_graph::shape::NodeShape) -> Option<adk_core::ToolEffect> {
    node.effect.or(node.governed.map(|_| adk_core::ToolEffect::NonIdempotent))
}

/// The graph's shape as the run view draws it.
pub(crate) fn graph_view(
    name: &str,
    description: &str,
    shape: &GraphShape,
    channels: Vec<String>,
) -> GraphView {
    let nodes = shape
        .nodes
        .iter()
        .map(|node| {
            let mut badges = Vec::new();
            let governed = node.governed.unwrap_or_default();
            if governed.requires_approval || node.interrupt_before {
                badges.push(words::ASKS_FIRST.to_string());
            }
            if governed.spends_money {
                badges.push(words::SPENDS_MONEY.to_string());
            }
            if effect_of(node) == Some(adk_core::ToolEffect::NonIdempotent) {
                badges.push(words::CANT_BE_UNDONE.to_string());
            }
            NodeView {
                name: node.name.clone(),
                description: node.description.clone(),
                kind: serde_json::to_value(node.kind)
                    .ok()
                    .and_then(|kind| kind.as_str().map(str::to_string))
                    .unwrap_or_default(),
                kind_text: words::node_kind(node.kind).to_string(),
                badges,
            }
        })
        .collect();
    GraphView {
        name: name.to_string(),
        description: description.to_string(),
        nodes,
        edges: shape
            .edges
            .iter()
            .map(|edge| EdgeView {
                from: edge.from.clone(),
                to: edge.to.clone(),
                conditional: edge.conditional,
            })
            .collect(),
        entry: shape.entry.clone(),
        channels,
    }
}

/// One checkpoint with its records read; a malformed record reads as none.
pub(crate) struct Saved {
    pub(crate) checkpoint: Checkpoint,
    pub(crate) run: Option<RunRecord>,
    pub(crate) steps: Vec<StepRecord>,
}

impl Saved {
    pub(crate) fn new(checkpoint: Checkpoint) -> Self {
        let run = checkpoint.run_record().ok().flatten();
        let steps = checkpoint.step_records().unwrap_or_default();
        Self { checkpoint, run, steps }
    }
}

/// One run: consecutive checkpoints that carry the same run record.
pub(crate) struct RunRecords<'a> {
    pub(crate) run: RunRecord,
    /// The checkpoint before the run's first, if the thread has one.
    pub(crate) before: Option<&'a Saved>,
    pub(crate) saved: Vec<&'a Saved>,
}

/// A thread's runs, oldest first.
pub(crate) fn runs(saved: &[Saved]) -> Vec<RunRecords<'_>> {
    let mut runs: Vec<RunRecords<'_>> = Vec::new();
    let mut previous: Option<&Saved> = None;
    for entry in saved {
        if let Some(run) = &entry.run {
            match runs.last_mut() {
                Some(last) if last.run.run_id == run.run_id => last.saved.push(entry),
                _ => {
                    runs.push(RunRecords { run: run.clone(), before: previous, saved: vec![entry] })
                }
            }
        }
        previous = Some(entry);
    }
    runs
}

/// The step label's rows: every record of one label merged, the completed checkpoint first.
struct Row<'a> {
    step: usize,
    state: StepState,
    checkpoint: &'a Checkpoint,
    /// Whether the checkpoint holds the state right after this step: it is the last step the
    /// checkpoint records. Steps written together under `Durability::Exit`, or before a
    /// flush, share one checkpoint that holds only the last one's state.
    addressed: bool,
    nodes: Vec<NodeRecord>,
}

/// Whether a paused step's record is a stop at an `interrupt_after` gate: the step finished,
/// and no node of it waits.
fn after_gate(record: &StepRecord) -> bool {
    record.kind == StepKind::Paused
        && !record.nodes.iter().any(|node| node.status == NodeStatus::Paused)
}

/// Merges a run's step records into one row per step label, in order.
fn rows<'a>(run: &RunRecords<'a>) -> Vec<Row<'a>> {
    let mut rows: Vec<Row<'a>> = Vec::new();
    for saved in &run.saved {
        for (at, record) in saved.steps.iter().enumerate() {
            let addressed = at + 1 == saved.steps.len();
            let state = match record.kind {
                StepKind::Completed => StepState::Completed,
                // The step finished before the run stopped at the gate after it.
                StepKind::Paused if after_gate(record) => StepState::Completed,
                StepKind::Paused => StepState::Paused,
                StepKind::Failed => StepState::Failed,
            };
            match rows.iter_mut().find(|row| row.step == record.step) {
                Some(row) => {
                    for node in &record.nodes {
                        match row.nodes.iter_mut().find(|known| known.node == node.node) {
                            Some(known) => *known = node.clone(),
                            None => row.nodes.push(node.clone()),
                        }
                    }
                    row.state = state;
                    row.checkpoint = &saved.checkpoint;
                    row.addressed = addressed;
                }
                None => rows.push(Row {
                    step: record.step,
                    state,
                    checkpoint: &saved.checkpoint,
                    addressed,
                    nodes: record.nodes.clone(),
                }),
            }
        }
    }
    for row in &mut rows {
        row.nodes.sort_by(|a, b| a.node.cmp(&b.node));
    }
    rows
}

/// The keys the person's message wrote: what the first checkpoint of a run holds that the
/// checkpoint before it did not, less what the run's first step wrote.
fn input_keys(
    before: Option<&Checkpoint>,
    first: &Checkpoint,
    written: &BTreeSet<String>,
) -> Vec<String> {
    first
        .state
        .iter()
        .filter(|(key, value)| {
            !written.contains(*key)
                && before.and_then(|before| before.state.get(*key)) != Some(*value)
        })
        .map(|(key, _)| key.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether a run started from its entry with the person's message, rather than from a fork.
fn starts_with_input(run: &RunRecords<'_>) -> bool {
    match run.before {
        None => true,
        // A fork's first checkpoint carries a lineage and no run.
        Some(before) => {
            before.run.is_some() || before.checkpoint.lineage().ok().flatten().is_none()
        }
    }
}

/// Facts the projection cannot read from the checkpoints.
#[derive(Default)]
pub(crate) struct Found {
    /// A run of this fork that the observer started is under way.
    pub(crate) starting: bool,
    /// The decision each waiting node waits on, by node.
    pub(crate) waiting: BTreeMap<String, String>,
    /// Everything is stopped.
    pub(crate) stopped: bool,
    /// When the run a fork came from started.
    pub(crate) parent_run_started: Option<DateTime<Utc>>,
    /// The nodes the run a fork came from finished up to the fork, with when.
    pub(crate) inherited: Vec<NodeRecord>,
    /// The forks of this thread.
    pub(crate) forks: Vec<ForkSummary>,
    /// The session of the thread a fork came from.
    pub(crate) parent_session: Option<String>,
    /// The question a paused node asked.
    pub(crate) question: Option<String>,
}

fn node_run(context: &Context<'_>, record: &NodeRecord, found: &Found, running: bool) -> NodeRun {
    let (state, decision) = match record.status {
        NodeStatus::Done => (NodeState::Done, None),
        NodeStatus::Paused => match found.waiting.get(&record.node) {
            Some(id) => (NodeState::Waiting, Some(id.clone())),
            None if running => (NodeState::Working, None),
            None => (NodeState::Paused, None),
        },
        NodeStatus::Failed => (NodeState::Failed, None),
        NodeStatus::Recorded => (NodeState::Recorded, None),
        NodeStatus::Refused | NodeStatus::NotRun => (NodeState::NotRun, None),
    };
    let ran = record.attempt > 0 || record.duration_ms > 0;
    NodeRun {
        node: record.node.clone(),
        state,
        state_text: words::node_state(state).to_string(),
        started_at: Some(record.started_at),
        started_text: Some(context.hh_mm_ss(record.started_at)),
        duration_ms: ran.then_some(record.duration_ms),
        // A node that waits, or whose call was not sent, has no run time to show.
        duration_text: ran.then(|| words::duration(record.duration_ms)).filter(|_| {
            !matches!(
                state,
                NodeState::Waiting | NodeState::Paused | NodeState::Recorded | NodeState::NotRun
            )
        }),
        attempt: (record.attempt > 0).then_some(record.attempt),
        writes: record.writes.clone(),
        error: record.error.clone(),
        decision_id: decision,
        inherited_text: None,
    }
}

fn idle(node: &str, state: NodeState) -> NodeRun {
    NodeRun {
        node: node.to_string(),
        state,
        state_text: words::node_state(state).to_string(),
        started_at: None,
        started_text: None,
        duration_ms: None,
        duration_text: None,
        attempt: None,
        writes: Vec::new(),
        error: None,
        decision_id: None,
        inherited_text: None,
    }
}

fn run_step(context: &Context<'_>, row: &Row<'_>, found: &Found, running: bool) -> RunStep {
    let nodes: Vec<NodeRun> =
        row.nodes.iter().map(|record| node_run(context, record, found, running)).collect();
    let wrote: BTreeSet<String> = row.nodes.iter().flat_map(|node| node.writes.clone()).collect();
    let at =
        row.nodes.iter().map(|node| node.started_at).min().unwrap_or(row.checkpoint.created_at);
    let waiting =
        nodes.iter().any(|node| matches!(node.state, NodeState::Waiting | NodeState::Paused));
    RunStep {
        step: row.step,
        label: words::step_label(row.step),
        state: row.state,
        at,
        at_text: context.hh_mm_ss(at),
        checkpoint_id: row.addressed.then(|| row.checkpoint.checkpoint_id.clone()),
        nodes,
        text: (waiting && wrote.is_empty()).then(|| words::NOTHING_WRITTEN_YET.to_string()),
        wrote: wrote.into_iter().collect(),
        forkable: row.addressed
            && row.state == StepState::Completed
            && !row.checkpoint.pending_nodes.is_empty()
            && !row.checkpoint.is_paused(),
    }
}

/// A run's steps, the input first when the person's message started it.
fn steps(
    context: &Context<'_>,
    run: &RunRecords<'_>,
    found: &Found,
    running: bool,
) -> Vec<RunStep> {
    let rows = rows(run);
    let mut steps = Vec::new();
    if starts_with_input(run)
        && let Some(first) = run.saved.first()
    {
        let written: BTreeSet<String> = first
            .steps
            .iter()
            .flat_map(|step| &step.nodes)
            .flat_map(|node| node.writes.clone())
            .collect();
        let keys =
            input_keys(run.before.map(|saved| &saved.checkpoint), &first.checkpoint, &written);
        let step = rows.first().map_or(first.checkpoint.step, |row| row.step).saturating_sub(1);
        steps.push(RunStep {
            step,
            label: words::step_label(step),
            state: StepState::Input,
            at: run.run.started_at,
            at_text: context.hh_mm_ss(run.run.started_at),
            checkpoint_id: None,
            nodes: Vec::new(),
            wrote: keys,
            text: Some(words::YOUR_MESSAGE.to_string()),
            forkable: false,
        });
    }
    steps.extend(rows.iter().map(|row| run_step(context, row, found, running)));
    steps
}

/// Every run of a thread from its records only, oldest first.
pub(crate) fn summaries(context: &Context<'_>, saved: &[Saved]) -> Vec<RunSummary> {
    let found = Found::default();
    runs(saved)
        .iter()
        .map(|run| RunSummary {
            run_id: run.run.run_id.clone(),
            started_at: run.run.started_at,
            started_text: context.hh_mm(run.run.started_at),
            steps: steps(context, run, &found, false),
        })
        .collect()
}

/// The first step still kept when retention removed the ones before it.
pub(crate) fn removed_before(saved: &[Saved]) -> Option<usize> {
    let oldest = saved.first()?;
    (oldest.checkpoint.version > 1)
        .then(|| oldest.steps.first().map_or(oldest.checkpoint.step, |step| step.step))
}

/// The lineage of a forked thread, as the run view names it.
pub(crate) fn lineage(context: &Context<'_>, saved: &[Saved], found: &Found) -> Option<RunLineage> {
    let lineage = saved.last()?.checkpoint.lineage().ok().flatten()?;
    let started = found.parent_run_started.map(|at| context.hh_mm(at));
    Some(RunLineage {
        text: words::fork_of(started.as_deref(), lineage.parent_step, &lineage.edited),
        root: lineage.root,
        parent: lineage.parent,
        parent_session: found.parent_session.clone(),
        parent_checkpoint: lineage.parent_checkpoint,
        parent_step: lineage.parent_step,
        edited: lineage.edited,
        checked: lineage.checked,
    })
}

/// The session's latest run, as the Runs tab shows it.
pub(crate) fn run_view(context: &Context<'_>, saved: &[Saved], found: Found) -> WorkflowRun {
    let all_runs = runs(saved);
    let newest = saved.last().map(|saved| &saved.checkpoint);
    let latest = all_runs.last();
    let lineage = lineage(context, saved, &found);

    let status = match (newest, latest) {
        (None, _) => RunStatus::NoRun,
        (Some(_), None) if lineage.is_some() && found.starting => RunStatus::Working,
        (Some(_), None) if lineage.is_some() => RunStatus::NotStarted,
        (Some(newest), latest) => {
            let last_step =
                latest.and_then(|run| run.saved.last()).and_then(|saved| saved.steps.last());
            let failed = last_step.is_some_and(|step| step.kind == StepKind::Failed);
            if failed {
                RunStatus::Failed
            } else if newest.pending_nodes.is_empty() {
                RunStatus::Done
            } else if newest.is_paused() || last_step.is_some_and(after_gate) {
                let paused: Vec<&NodeRecord> = latest
                    .and_then(|run| run.saved.last())
                    .and_then(|saved| saved.steps.last())
                    .map(|step| {
                        step.nodes.iter().filter(|node| node.status == NodeStatus::Paused).collect()
                    })
                    .unwrap_or_default();
                if paused.iter().any(|node| found.waiting.contains_key(&node.node)) {
                    RunStatus::Waiting
                } else if found.stopped {
                    RunStatus::Stopped
                } else if paused.iter().any(|node| context.governed(&node.node)) {
                    // A decision answered and its run not resumed yet.
                    RunStatus::Working
                } else {
                    RunStatus::Paused
                }
            } else if found.stopped {
                RunStatus::Stopped
            } else {
                RunStatus::Working
            }
        }
    };
    let running = status == RunStatus::Working;
    let run_steps = latest.map(|run| steps(context, run, &found, running)).unwrap_or_default();

    // Each node as it stands: its latest record in the run, else where the run left it.
    let mut latest_records: BTreeMap<String, NodeRun> = BTreeMap::new();
    for step in &run_steps {
        for node in &step.nodes {
            latest_records.insert(node.node.clone(), node.clone());
        }
    }
    let pending: BTreeSet<&String> =
        newest.map(|newest| newest.pending_nodes.iter().collect()).unwrap_or_default();
    let inherited: BTreeMap<&str, &NodeRecord> =
        found.inherited.iter().map(|record| (record.node.as_str(), record)).collect();
    let nodes: Vec<NodeRun> = context
        .shape
        .nodes
        .iter()
        .map(|shape| {
            let name = shape.name.as_str();
            if let Some(record) = latest_records.get(name) {
                return record.clone();
            }
            if let Some(record) = inherited.get(name) {
                let mut run = node_run(context, record, &Found::default(), false);
                run.inherited_text = Some(words::inherited(&context.hh_mm(record.started_at)));
                run.duration_text = None;
                return run;
            }
            let state = match status {
                RunStatus::Failed => NodeState::NotRun,
                RunStatus::Stopped if pending.contains(&shape.name) => NodeState::NotRun,
                RunStatus::Working if pending.contains(&shape.name) => NodeState::Working,
                _ => NodeState::Idle,
            };
            idle(name, state)
        })
        .collect();

    // The edges taken: from each step's finished nodes to the next step's nodes, and to END
    // from a finished run's last step.
    let edges: BTreeSet<(&str, &str)> =
        context.shape.edges.iter().map(|edge| (edge.from.as_str(), edge.to.as_str())).collect();
    let mut layers: Vec<Vec<(String, NodeState)>> = Vec::new();
    if !found.inherited.is_empty() {
        layers.push(
            found.inherited.iter().map(|record| (record.node.clone(), NodeState::Done)).collect(),
        );
    }
    for step in run_steps.iter().filter(|step| step.state != StepState::Input) {
        layers.push(step.nodes.iter().map(|node| (node.node.clone(), node.state)).collect());
    }
    let finished = |state: &NodeState| matches!(state, NodeState::Done | NodeState::Recorded);
    let mut taken = BTreeSet::new();
    for pair in layers.windows(2) {
        for (from, state) in &pair[0] {
            if !finished(state) {
                continue;
            }
            for (to, _) in &pair[1] {
                if edges.contains(&(from.as_str(), to.as_str())) {
                    taken.insert((from.clone(), to.clone()));
                }
            }
        }
    }
    if status == RunStatus::Done
        && let Some(last) = layers.last()
    {
        for (from, state) in last {
            if finished(state) && edges.contains(&(from.as_str(), END)) {
                taken.insert((from.clone(), END.to_string()));
            }
        }
    }
    let taken: Vec<EdgeView> = context
        .shape
        .edges
        .iter()
        .filter(|edge| taken.contains(&(edge.from.clone(), edge.to.clone())))
        .map(|edge| EdgeView {
            from: edge.from.clone(),
            to: edge.to.clone(),
            conditional: edge.conditional,
        })
        .collect();

    let started_at = latest.map(|run| run.run.started_at);
    let ended_at = latest.and_then(|run| run.saved.last()).map(|saved| saved.checkpoint.created_at);
    let duration_ms = started_at
        .zip(ended_at)
        .map(|(start, end)| u64::try_from((end - start).num_milliseconds().max(0)).unwrap_or(0));
    let summary = match (status, started_at, duration_ms) {
        (RunStatus::NoRun, ..) => words::NO_RUN_YET.to_string(),
        (RunStatus::NotStarted, ..) => words::made_summary(
            &context.hh_mm(newest.map_or_else(Utc::now, |newest| newest.created_at)),
            lineage.as_ref().map_or(0, |lineage| lineage.parent_step),
        ),
        (_, Some(start), Some(ms)) => {
            words::run_summary(&context.hh_mm(start), &words::duration(ms), run_steps.len())
        }
        _ => String::new(),
    };
    let error = (status == RunStatus::Failed)
        .then(|| nodes.iter().rev().find_map(|node| node.error.clone()))
        .flatten();

    WorkflowRun {
        session_id: context.session.to_string(),
        thread_id: context.thread.to_string(),
        graph: context.graph.to_string(),
        run_id: latest.map(|run| run.run.run_id.clone()),
        status,
        status_text: words::run_status(status).to_string(),
        started_at,
        duration_ms,
        summary,
        nodes,
        taken,
        steps: run_steps,
        lineage,
        forks: found.forks,
        removed_before: removed_before(saved),
        error,
        question: (status == RunStatus::Paused).then_some(found.question).flatten(),
    }
}

/// `POST 127.0.0.1/charges`: an HTTP node's call in short, from its call arguments.
pub(crate) fn call_summary(args: &Value) -> Option<String> {
    let method = args.get("method")?.as_str()?;
    let url = args.get("url")?.as_str()?;
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    Some(format!("{method} {rest}"))
}

/// What a step detail reads besides its checkpoint.
pub(crate) struct StepInputs<'a> {
    /// The checkpoint before it in the thread.
    pub(crate) previous: Option<&'a Checkpoint>,
    /// The thread's checkpoints after it, oldest first.
    pub(crate) later: &'a [Saved],
    /// The lineage's effect log.
    pub(crate) effects: &'a [EffectRecord],
    /// The decision each governed node of the step waited on.
    pub(crate) decisions: BTreeMap<String, NodeDecision>,
    /// Each governed node's call in short, at this checkpoint's state.
    pub(crate) calls: BTreeMap<String, String>,
}

/// One step, from its checkpoint (spec 3.3 §12).
pub(crate) fn step_detail(
    context: &Context<'_>,
    saved: &Saved,
    inputs: StepInputs<'_>,
) -> StepDetail {
    let checkpoint = &saved.checkpoint;
    let record = saved.steps.last();
    let step = record.map_or(checkpoint.step, |record| record.step);
    let state = match record.map(|record| record.kind) {
        Some(StepKind::Paused) => StepState::Paused,
        Some(StepKind::Failed) => StepState::Failed,
        _ => StepState::Completed,
    };
    let found = Found {
        waiting: inputs
            .decisions
            .iter()
            .filter(|(_, decision)| decision.waiting)
            .map(|(node, decision)| (node.clone(), decision.id.clone()))
            .collect(),
        ..Found::default()
    };
    let nodes: Vec<NodeDetail> = record
        .map(|record| &record.nodes[..])
        .unwrap_or_default()
        .iter()
        .map(|node| {
            let run = node_run(context, node, &found, false);
            let effect = (node.status == NodeStatus::Recorded)
                .then(|| {
                    let key = node.effect_key.as_deref()?;
                    let entry = inputs.effects.iter().rev().find(|entry| entry.key == key)?;
                    Some(effect_view(context, entry, inputs.calls.get(&node.node).cloned()))
                })
                .flatten();
            let text = match (&effect, node.status, node.error.as_deref()) {
                (Some(effect), ..) => Some(words::not_sent_again(&effect.at_text)),
                (None, NodeStatus::Failed, Some(error))
                    if error.starts_with("graph.recorded_failure") =>
                {
                    Some(words::FAILED_EARLIER.to_string())
                }
                _ => None,
            };
            let kind = context.kind(&node.node);
            NodeDetail {
                run,
                kind_text: words::node_kind(kind).to_string(),
                model_step: kind == NodeKind::ModelStep,
                effect,
                decision: inputs.decisions.get(&node.node).cloned(),
                text,
            }
        })
        .collect();

    // The state before: the thread's previous checkpoint, else the input the run started on.
    let after: BTreeMap<String, Value> =
        checkpoint.state.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
    let before: BTreeMap<String, Value> = match inputs.previous {
        Some(previous) => {
            previous.state.iter().map(|(key, value)| (key.clone(), value.clone())).collect()
        }
        None => {
            let written: BTreeSet<&String> = saved
                .steps
                .iter()
                .flat_map(|step| &step.nodes)
                .flat_map(|node| &node.writes)
                .collect();
            after
                .iter()
                .filter(|(key, _)| !written.contains(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        }
    };
    let changes = context.display.changes(&before, &after);
    // The declared channels, which a fork may edit; a graph that declares none, its state.
    let channels: BTreeSet<String> = if context.channels.is_empty() {
        after.keys().cloned().collect()
    } else {
        context.channels.iter().cloned().collect()
    };
    let state_values =
        channels.iter().map(|key| context.display.channel(key, after.get(key))).collect();

    // What a fork from here would run again, and the calls that already happened after it.
    let mut runs_again: Vec<String> = Vec::new();
    let mut done_after: BTreeMap<String, &NodeRecord> = BTreeMap::new();
    for later in inputs.later {
        for record in &later.steps {
            for node in &record.nodes {
                if !runs_again.contains(&node.node) {
                    runs_again.push(node.node.clone());
                }
                if matches!(node.status, NodeStatus::Done | NodeStatus::Recorded) {
                    done_after.entry(node.node.clone()).or_insert(node);
                }
            }
        }
    }
    if runs_again.is_empty() {
        runs_again = checkpoint.pending_nodes.clone();
    }
    let already_happened = runs_again
        .iter()
        .filter(|node| context.governed(node))
        .filter_map(|node| {
            let record = done_after.get(node)?;
            let at = record
                .effect_key
                .as_deref()
                .and_then(|key| inputs.effects.iter().find(|entry| entry.key == key))
                .map_or(record.started_at, |entry| entry.at);
            let call = inputs.calls.get(node).cloned();
            let at_text = context.hh_mm(at);
            Some(EffectView {
                text: words::effect(node, call.as_deref(), &at_text),
                node: node.clone(),
                call,
                at,
                at_text,
                writes: record.writes.clone(),
            })
        })
        .collect();
    let asks: Vec<bool> = runs_again
        .iter()
        .filter_map(|node| context.shape.nodes.iter().find(|shape| shape.name == *node))
        .filter_map(|shape| shape.governed.map(|governed| governed.requires_approval))
        .collect();
    let changed_calls = if asks.iter().any(|asks| *asks) {
        Some(words::CHANGED_CALL_WAITS.to_string())
    } else if !asks.is_empty() {
        Some(words::CHANGED_CALL_RULES.to_string())
    } else {
        None
    };
    let refusal = if checkpoint.is_paused() {
        Some(words::FORK_FROM_THE_COMPLETED_STEP.to_string())
    } else if checkpoint.pending_nodes.is_empty() {
        Some(words::NOTHING_RUNS_AFTER_THE_LAST_STEP.to_string())
    } else {
        None
    };

    StepDetail {
        session_id: context.session.to_string(),
        checkpoint_id: checkpoint.checkpoint_id.clone(),
        step,
        label: words::step_label(step),
        state,
        at_text: context.hh_mm_ss(
            record
                .and_then(|record| record.nodes.iter().map(|node| node.started_at).min())
                .unwrap_or(checkpoint.created_at),
        ),
        nodes,
        changes,
        state_values,
        fork: ForkPoint {
            allowed: refusal.is_none(),
            refusal,
            runs_again,
            already_happened,
            changed_calls,
        },
    }
}

fn effect_view(context: &Context<'_>, entry: &EffectRecord, call: Option<String>) -> EffectView {
    let at_text = context.hh_mm(entry.at);
    EffectView {
        text: words::effect(&entry.node, call.as_deref(), &at_text),
        node: entry.node.clone(),
        call,
        at: entry.at,
        at_text,
        writes: entry.writes.keys().cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use adk_graph::record::{RUN_METADATA_KEY, STEPS_METADATA_KEY};
    use adk_graph::shape::{NodeKind, NodeShape};
    use adk_graph::state::State;
    use serde_json::json;

    use super::*;

    fn done(node: &str) -> NodeRecord {
        NodeRecord {
            node: node.to_string(),
            started_at: Utc::now(),
            duration_ms: 5,
            status: NodeStatus::Done,
            writes: vec![node.to_string()],
            attempt: 1,
            effect_key: None,
            error: None,
        }
    }

    fn saved(step: usize, pending: &[&str], steps: Vec<StepRecord>) -> Saved {
        Saved::new(
            Checkpoint::new(
                "t1",
                State::new(),
                step,
                pending.iter().map(|n| n.to_string()).collect(),
            )
            .with_metadata(RUN_METADATA_KEY, json!({ "runId": "r1", "startedAt": Utc::now() }))
            .with_metadata(STEPS_METADATA_KEY, serde_json::to_value(steps).unwrap()),
        )
    }

    fn shape(nodes: &[&str]) -> GraphShape {
        GraphShape {
            nodes: nodes
                .iter()
                .map(|name| NodeShape {
                    name: name.to_string(),
                    description: String::new(),
                    kind: NodeKind::Function,
                    effect: None,
                    interrupt_before: false,
                    interrupt_after: false,
                    governed: None,
                })
                .collect(),
            edges: Vec::new(),
            entry: vec![nodes[0].to_string()],
        }
    }

    fn view(saved: &[Saved], nodes: &[&str]) -> WorkflowRun {
        let shape = shape(nodes);
        let display = Display::default();
        let context = Context {
            session: "s1",
            thread: "t1",
            graph: "g",
            shape: &shape,
            channels: &[],
            offset: FixedOffset::east_opt(0).unwrap(),
            display: &display,
        };
        run_view(&context, saved, Found::default())
    }

    fn completed(step: usize, node: &str) -> StepRecord {
        StepRecord { step, kind: StepKind::Completed, nodes: vec![done(node)] }
    }

    /// Spec 3.3 §12: steps written together under `Durability::Exit` share one checkpoint,
    /// which holds the last one's state; the earlier steps are not opened or forked from it.
    #[test]
    fn steps_saved_together_address_only_the_last() {
        let together = saved(2, &["c"], vec![completed(1, "a"), completed(2, "b")]);
        let id = together.checkpoint.checkpoint_id.clone();
        let run = view(&[together], &["a", "b", "c"]);
        let rows: Vec<(usize, Option<String>, bool)> = run
            .steps
            .iter()
            .filter(|step| step.state != StepState::Input)
            .map(|step| (step.step, step.checkpoint_id.clone(), step.forkable))
            .collect();
        assert_eq!(rows, vec![(1, None, false), (2, Some(id), true)]);
    }

    /// Spec 3.3 §12: a run stopped at an `interrupt_after` gate is paused, not working, and
    /// the gated step is a finished step a fork may start from.
    #[test]
    fn a_run_stopped_after_a_gate_is_paused() {
        let gated = StepRecord { step: 1, kind: StepKind::Paused, nodes: vec![done("a")] };
        let run = view(&[saved(0, &["b"], vec![gated])], &["a", "b"]);
        assert_eq!(run.status, RunStatus::Paused);
        let step = run.steps.iter().find(|step| step.step == 1).unwrap();
        assert_eq!((step.state, step.forkable), (StepState::Completed, true));
    }
}
