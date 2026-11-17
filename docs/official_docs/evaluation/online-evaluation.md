# Online Evaluation

Offline evaluation replays an eval set before a release. Online evaluation scores the runs real
users start: `adk_eval::online::OnlineEvaluator` samples a fraction of finished production runs,
judges each on a background worker with reference-free criteria, and records the verdicts where
operators and a self-improvement loop read them. It is off until you build an evaluator and
install it, it never delays a run, and every judge call counts against its own budget and stops
when a kill switch is frozen.

## How It Works

| Step | Where | What happens |
|------|-------|--------------|
| Sample | `before_run` plugin hook | The [sampling policy](#sampling) decides from the agent, the request metadata and a seeded hash of the invocation id; a run that is not sampled costs one atomic load per event |
| Buffer | `on_event` | The complete (non-partial) events of a sampled run are kept, up to `max_events_per_run` |
| Hand off | `after_run` | The run's outcome is classified; a run whose outcome the policy admits is queued without waiting — a full queue drops it |
| Judge | Background worker, one run at a time | The [criteria](#criteria) score the run under the evaluator's budget, spend ledger and kill switches |
| Record | Worker | An `OnlineEvalRecord` goes to the [store](#where-results-go) and one `gen_ai.evaluation.result` event per criterion goes to telemetry |

## Quick Start

```rust
use std::sync::Arc;

use adk_core::{GovernanceControl, InMemorySpendLedger, Llm, RunBudget};
use adk_eval::online::{
    JsonlOnlineEvalStore, OnlineEvalConfig, OnlineEvaluator, RunOutcome, SamplingPolicy,
};
use adk_eval::{EvaluationCriteria, Rubric};
use adk_plugin::PluginManager;
use adk_runner::Runner;
use adk_session::InMemorySessionService;

fn build(agent: Arc<dyn adk_core::Agent>, judge: Arc<dyn Llm>) -> anyhow::Result<(Runner, OnlineEvaluator)> {
    let kill_switch = GovernanceControl::new();

    // Judge 10% of the support agent's completed runs.
    let sampling = SamplingPolicy::rate(0.10)?
        .with_seed(7)
        .agent("support")
        .outcome(RunOutcome::Completed);
    let criteria = EvaluationCriteria { safety_score: Some(0.9), ..Default::default() }.with_rubrics(
        0.7,
        vec![Rubric::new("Grounded", "Answers only from the tool results it received")],
    );
    let config = OnlineEvalConfig::new(sampling)
        .with_criteria(criteria)
        .with_min_trajectory_efficiency(0.5)
        .with_budget(RunBudget::new().max_model_calls(2_000).max_cost_usd(10.0));

    let online = OnlineEvaluator::builder(config)
        .judge(judge)
        .store(Arc::new(JsonlOnlineEvalStore::new("online-eval.jsonl")))
        .spend_ledger(Arc::new(InMemorySpendLedger::default()), "support-app")
        .governance(kill_switch)
        .start()?; // inside a Tokio runtime

    let runner = Runner::builder()
        .app_name("support-app")
        .agent(agent)
        .session_service(Arc::new(InMemorySessionService::new()))
        .plugin_manager(Arc::new(PluginManager::new(vec![online.plugin()])))
        .build()?;
    Ok((runner, online))
}
```

The plugin needs `adk-runner`'s `plugins` feature, which is on by default. Keep the
`OnlineEvaluator` to read its [stats](#operating-it), and call `shutdown().await` when the
application stops so the worker finishes the queue.

## Sampling

`SamplingPolicy` selects a run when every filter passes and its sample point falls under the rate.

| Setting | Builder | Default |
|---------|---------|---------|
| Rate, 0.0 to 1.0 | `SamplingPolicy::rate(0.10)?` | Required |
| Seed of the sample point hash | `.with_seed(7)` | `0` |
| Root agents (repeatable) | `.agent("support")` | Every agent |
| Request metadata, a key or a key and value (repeatable) | `.tag("tier", Some("gold"))`, `.tag("channel", None)` | Any metadata |
| Outcomes (repeatable) | `.outcome(RunOutcome::Errored)` | Every outcome |

- **Deterministic.** The sample point is a fixed FNV-1a and SplitMix64 hash of the seed and the
  invocation id, so the same seed selects the same runs in every process and on every replay,
  and `SamplingPolicy::sample_point(id) < rate` tells which runs were selected.
- **Metadata** is the run's `RunConfig::request_metadata` overlaid with its request context's
  (`InvocationContext::request_metadata`); a tag is a metadata key the application sets, such as
  a tenant tier or a channel.
- **Outcomes** come from the run's events: `Completed` (ends on a final response, no error),
  `Errored` (an event carries an error code or message), `Incomplete` (a stream error, a held
  call or an interrupt), `Cancelled`. Sampling `Errored` runs at a higher rate than `Completed`
  ones takes two evaluators.

## Criteria

Production runs have no expected answer, so only reference-free criteria apply.
`tool_trajectory_score`, `response_similarity` and `semantic_match_score` are refused when the
evaluator starts.

| Criterion | Set with | Judge calls per run |
|-----------|----------|---------------------|
| Rubric quality | `EvaluationCriteria::with_rubrics(threshold, rubrics)` | 1 |
| Safety | `EvaluationCriteria { safety_score: Some(0.9), .. }` | 1 |
| Hallucination | `EvaluationCriteria { hallucination_score: Some(0.8), .. }` | 1 |
| Trajectory efficiency | `OnlineEvalConfig::with_min_trajectory_efficiency(0.5)` — `TraceAnalyzer` over the run's tool calls: redundant calls and loops | 0 |
| Thread-level | `OnlineEvalConfig::with_thread(ConversationScorerConfig::default())` — context retention, goal completion, coherence and topic drift of the whole session, after each sampled run | 4 or more |

The judges see the user message and the tool results the run received (each cut to 2,000
characters), so an answer that repeats a tool result is not mistaken for a hallucination.
Thread-level criteria score the session's conversation history as it stands when the run ends;
their goal is the session's first user message. A criterion the judge cannot score — no text
answer, an unparseable reply — fails, as it does offline.

## Budget and Kill Switch

- **Its own budget.** `OnlineEvalConfig::budget` is one `BudgetTracker` for the evaluator's
  lifetime; it defaults to `DEFAULT_ONLINE_EVAL_MAX_MODEL_CALLS` (1,000) model calls, and a
  configuration with judged criteria and an unlimited budget is refused. For a daily cap, pass a
  spend ledger with `SpendLimits` per day through `spend_ledger(ledger, org)`; judge spend is
  reserved and committed under the agent `online-eval` (`urn:adk:agent:{org}/online-eval`).
- **A refusal is not a verdict.** A judge call the budget or the ledger refuses stops that run's
  evaluation and records nothing (`stats().refused`). Once the budget is spent, sampled runs are
  dropped before they are queued (`DropReason::BudgetSpent`).
- **Kill switches.** The evaluator's own controls (`OnlineEvaluatorBuilder::governance`, for
  example the runner's `Runner::governance()`) and the run's (`RunConfig::governance`) are
  checked when a run is queued, before it is judged and before every judge call. While one is
  frozen, sampled runs are dropped (`DropReason::Frozen`) and nothing is judged.
- **Off the request path.** The plugin's hooks do not await the judge; the run's stream pays for
  a lock and a channel send. A full queue (`queue_capacity`, default 64) drops the run
  (`DropReason::QueueFull`) rather than slowing the application.

## Where Results Go

Each judged run produces an `OnlineEvalRecord`: the app, session, invocation and agent, the
outcome, `passed`, the score of every criterion, the failures with the judges' explanations, the
trace analysis and thread metrics when configured, and the judge calls it took. It holds no
message text; an explanation can paraphrase the run.

| Store | Use |
|-------|-----|
| `InMemoryOnlineEvalStore::new(capacity)` | The newest `capacity` records, for a dashboard in the same process; the default |
| `JsonlOnlineEvalStore::new(path)` | One JSON record per line, for a log pipeline or a later process |
| Your own | Implement `OnlineEvalStore` (`append`, `list`) |

`list(&OnlineEvalQuery::default().agent("support").failed_only().since(t).limit(50))` returns
records newest first, and `OnlineEvalSummary::from_records` gives the pass rate and, per
criterion, the runs scored, the runs failed and the mean score.

Every scored criterion is also emitted as a `gen_ai.evaluation.result` tracing event inside an
`adk.eval.online` span (`agent.name`, `invocation.id`, `gen_ai.conversation.id`,
`adk.run.outcome`, `adk.eval.passed`), so an OpenTelemetry exporter carries the verdicts next to
the run's own spans:

| Attribute | Value |
|-----------|-------|
| `gen_ai.evaluation.name` | `safety`, `rubric_quality`, `rubric_<name>`, `hallucination`, `trajectory_efficiency`, `thread_context_retention`, `thread_goal_completion`, `thread_coherence`, `thread_topic_drift` |
| `gen_ai.evaluation.score.value` | The score, 0.0 to 1.0 |
| `gen_ai.evaluation.score.label` | `pass` or `fail` |
| `gen_ai.evaluation.explanation` | The judge's reasoning for a failed criterion |

The names are in `adk_eval::online::attributes`.

## Operating It

| Call | Returns |
|------|---------|
| `stats()` | `observed`, `sampled`, `queued`, `dropped`, `evaluated`, `passed`, `failed`, `refused`, `errors` since start |
| `budget().usage()` | The judge's model calls, tokens and cost so far |
| `store()` | The store, to list records |
| `flush().await` | Waits until every queued run is judged or refused |
| `shutdown().await` | Stops accepting runs, finishes the queue, and stops the worker |

## Runs That Do Not Go Through a Runner Plugin

A host that collects runs itself — a server's background runs, a run store replay, another
framework — builds a `RunTrace` and calls `submit`, which applies the whole sampling policy and
never waits:

```rust
use adk_core::{Content, Event};
use adk_eval::online::{OnlineEvaluator, RunTrace, Submission};

fn hand_over(online: &OnlineEvaluator, session_id: &str, invocation_id: &str, events: Vec<Event>) {
    let trace = RunTrace::new(
        "support-app",
        session_id,
        invocation_id,
        "support",
        Content::new("user").with_text("Where is my order?"),
        events,
    )
    .with_metadata("tier", "gold");
    if let Submission::Dropped(reason) = online.submit(trace) {
        tracing::debug!(?reason, "run not evaluated");
    }
}
```

On the agent kernel, install the plugin on the runner you register with `KernelBuilder::agent`;
on the harness, build the runner with `HarnessAgent::configure_runner(..).plugin_manager(..)` and
wrap it with `Harness::with_runner`.

## Feeding a Self-Improvement Loop

The self-improving harness of spec 3.4 (`docs/design/spec-3.4-self-improving-harness.md` §7.3)
mines its own operation into defect records and reproduces each as an evaluation case before it proposes a
fix. Online evaluation is one of its sources:

1. The miner reads `store.list(&OnlineEvalQuery::default().failed_only().since(last_tick))`.
2. Each failed record's criterion names the defect kind, and its `session_id` and
   `invocation_id` are the evidence reference; the record holds no content, so the defect store
   stays content-free.
3. The reproducer turns the referenced session's events into an `EvalCase` with
   `TestGenerator::generate_from_events` and keeps it only when it fails on the current build.
4. After an upgrade, `OnlineEvalSummary::from_records` over the records since the release shows
   whether the criterion's pass rate recovered.

The evaluator, its criteria and its store sit outside the agent's write scope, as spec 3.4
requires of every grader.
