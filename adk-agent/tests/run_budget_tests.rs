//! Run budgets end to end: a run stops before the model or tool call that would
//! start past a reached limit, partial chunks never inflate usage, and every
//! agent in the invocation counts against one shared tracker.
//!
//! The scripted models stream Gemini-shaped chunks — cumulative usage on every
//! partial chunk — through `adk-model`'s shared pricing path, so each response
//! carries the provider, model and cost a `GeminiModel` would attach.

use adk_agent::{
    LlmAgentBuilder, RelationshipKind, SequentialAgent, TeamBudget, TeamExecutionStatus,
    TeamMemberSpec, TeamPolicy, TeamRelationship, TeamSpec,
};
use adk_core::{
    AdkError, Agent, BUDGET_LIMIT_KEY, BudgetTracker, Content, ErrorCategory, Event, FinishReason,
    Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result, RunBudget, RunConfig, Tool,
    ToolContext, UsageMetadata,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Produces the chunks of one model call from the zero-based call number.
type Script = Arc<dyn Fn(usize) -> Vec<LlmResponse> + Send + Sync>;

struct ScriptedModel {
    name: String,
    calls: Arc<AtomicUsize>,
    script: Script,
}

impl ScriptedModel {
    fn new(name: &str, script: impl Fn(usize) -> Vec<LlmResponse> + Send + Sync + 'static) -> Self {
        Self {
            name: name.to_string(),
            calls: Arc::new(AtomicUsize::new(0)),
            script: Arc::new(script),
        }
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = (self.script)(call);
        let stream: LlmResponseStream = Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)));
        Ok(adk_model::usage_tracking::with_priced_usage_tracking(
            stream,
            tracing::Span::none(),
            "gemini",
            self.name.clone(),
        ))
    }
}

fn usage(prompt: i32, output: i32) -> UsageMetadata {
    UsageMetadata {
        prompt_token_count: prompt,
        candidates_token_count: output,
        total_token_count: prompt + output,
        ..Default::default()
    }
}

fn chunk(parts: Vec<Part>, usage: UsageMetadata, partial: bool) -> LlmResponse {
    LlmResponse {
        content: Some(Content { role: "model".to_string(), parts }),
        usage_metadata: Some(usage),
        finish_reason: (!partial).then_some(FinishReason::Stop),
        partial,
        turn_complete: false,
        ..Default::default()
    }
}

fn text(text: &str) -> Part {
    Part::Text { text: text.to_string() }
}

fn call(name: &str) -> Part {
    Part::FunctionCall {
        name: name.to_string(),
        args: json!({ "request": "go" }),
        id: None,
        thought_signature: None,
    }
}

/// One Gemini turn on `gemini-3.5-flash-lite` that calls `tool`.
///
/// The final chunk reports 1M prompt tokens ($0.30) and 3.88M output tokens
/// ($9.70): $10.00. The two partial chunks repeat cumulative usage worth
/// $2.725 and $5.15, which must never be added to the total.
fn ten_dollar_turn(tool: &str) -> Vec<LlmResponse> {
    vec![
        chunk(vec![text("working")], usage(1_000_000, 970_000), true),
        chunk(vec![text(" on it")], usage(1_000_000, 1_940_000), true),
        chunk(vec![call(tool)], usage(1_000_000, 3_880_000), false),
    ]
}

/// A turn that answers in text, with `tokens` total tokens on the final chunk.
fn answer_turn(answer: &str, tokens: i32) -> Vec<LlmResponse> {
    vec![
        chunk(vec![text("…")], usage(tokens / 2, 0), true),
        LlmResponse {
            turn_complete: true,
            ..chunk(vec![text(answer)], usage(tokens / 2, tokens - tokens / 2), false)
        },
    ]
}

struct CountingTool {
    name: String,
    runs: Arc<AtomicUsize>,
    delay: Duration,
}

impl CountingTool {
    fn new(name: &str) -> Self {
        Self { name: name.to_string(), runs: Arc::new(AtomicUsize::new(0)), delay: Duration::ZERO }
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Counts its calls"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        tokio::time::sleep(self.delay).await;
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ok": true }))
    }
}

async fn runner_for(
    agent: Arc<dyn Agent>,
    run_config: RunConfig,
) -> (Runner, Arc<InMemorySessionService>) {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "budget-app".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("budget-app")
        .agent(agent)
        .session_service(sessions.clone())
        .run_config(run_config)
        .build()
        .unwrap();
    (runner, sessions)
}

async fn run(
    runner: &Runner,
    run_config: Option<RunConfig>,
) -> Vec<std::result::Result<Event, AdkError>> {
    runner
        .run_with_config(
            "user".try_into().unwrap(),
            "session".try_into().unwrap(),
            Content::new("user").with_text("go"),
            run_config,
        )
        .await
        .unwrap()
        .collect()
        .await
}

fn budget_error(results: &[std::result::Result<Event, AdkError>]) -> &AdkError {
    let error = results.last().unwrap().as_ref().expect_err("the run ends with an error");
    assert_eq!(error.category, ErrorCategory::ResourceExhausted, "{error}");
    error
}

fn stop_event(results: &[std::result::Result<Event, AdkError>]) -> &Event {
    results[results.len() - 2].as_ref().expect("a stop event precedes the error")
}

fn worker_team(
    supervisor_model: Arc<ScriptedModel>,
    work: Arc<CountingTool>,
    budget: TeamBudget,
) -> Arc<adk_agent::CompiledTeam> {
    let supervisor =
        LlmAgentBuilder::new("supervisor").model(supervisor_model).tool(work).build().unwrap();
    let specialist = LlmAgentBuilder::new("specialist")
        .model(Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| answer_turn("done", 10))))
        .build()
        .unwrap();
    Arc::new(
        TeamSpec {
            name: "gemini_team".to_string(),
            description: "A Gemini team under a spend cap".to_string(),
            coordinator: "supervisor".to_string(),
            members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("specialist")],
            relationships: vec![TeamRelationship::new(
                "supervisor",
                "specialist",
                RelationshipKind::Handoff,
            )],
            policy: TeamPolicy { budget, ..TeamPolicy::default() },
        }
        .compile([Arc::new(supervisor) as Arc<dyn Agent>, Arc::new(specialist)])
        .unwrap(),
    )
}

/// Phase 1 gate: a $50 cap stops a Gemini team at $50.
#[tokio::test]
async fn a_fifty_dollar_team_budget_stops_a_gemini_team_at_fifty_dollars() {
    let model = Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| ten_dollar_turn("work")));
    let work = Arc::new(CountingTool::new("work"));
    let team = worker_team(
        model.clone(),
        work.clone(),
        TeamBudget { max_cost_microusd: Some(50_000_000), ..TeamBudget::default() },
    );
    let (runner, sessions) = runner_for(team.clone(), RunConfig::default()).await;

    let results = run(&runner, None).await;

    // Five $10 calls reach the cap; nothing starts after it — not the fifth
    // call's tool, not a sixth call.
    assert_eq!(model.calls.load(Ordering::SeqCst), 5);
    assert_eq!(work.runs.load(Ordering::SeqCst), 4);
    let error = budget_error(&results);
    assert_eq!(error.code, "budget.cost");
    let stop = stop_event(&results);
    assert_eq!(stop.llm_response.error_code.as_deref(), Some("budget.cost"));
    assert_eq!(stop.provider_metadata[BUDGET_LIMIT_KEY], "cost_micro_usd");
    assert_eq!(stop.author, "supervisor");

    let receipt = team.execution_snapshots().pop().unwrap();
    assert_eq!(receipt.status, TeamExecutionStatus::BudgetExceeded);
    assert_eq!(receipt.usage.cost_microusd, 50_000_000);
    assert_eq!(receipt.usage.model_requests, 5);
    assert_eq!(receipt.usage.tokens, 5 * 4_880_000);
    assert_eq!(receipt.usage.tool_calls, 4);
    // Five model events, four tool results and the refused batch's responses; the
    // partial events the members streamed (ten model chunks among them) are not counted.
    let partial_chunks = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .filter(|event| event.llm_response.partial && event.llm_response.usage_metadata.is_some())
        .count();
    assert_eq!(partial_chunks, 10);
    assert_eq!(receipt.usage.events, 10);

    // The stop event is persisted with the rest of the run.
    let session = sessions
        .get(adk_session::GetRequest {
            app_name: "budget-app".to_string(),
            user_id: "user".to_string(),
            session_id: "session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    let persisted = session.events().all();
    assert_eq!(persisted.last().unwrap().llm_response.error_code.as_deref(), Some("budget.cost"));
}

/// The same cap set on the run instead of the team reaches the team's members.
#[tokio::test]
async fn a_run_budget_reaches_team_members() {
    let model = Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| ten_dollar_turn("work")));
    let work = Arc::new(CountingTool::new("work"));
    let team = worker_team(model.clone(), work.clone(), TeamBudget::default());
    let (runner, _) =
        runner_for(team, RunConfig::builder().budget(RunBudget::new().max_cost_usd(50.0)).build())
            .await;

    let results = run(&runner, None).await;

    assert_eq!(model.calls.load(Ordering::SeqCst), 5);
    assert_eq!(budget_error(&results).code, "budget.cost");
}

#[tokio::test]
async fn partial_chunks_do_not_inflate_usage() {
    // 1,000 tokens per call on the final chunk; the partial chunk repeats 500.
    let model = Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| {
        vec![
            chunk(vec![text("…")], usage(250, 250), true),
            chunk(vec![call("work")], usage(500, 500), false),
        ]
    }));
    let agent = LlmAgentBuilder::new("assistant")
        .model(model.clone())
        .tool(Arc::new(CountingTool::new("work")))
        .build()
        .unwrap();
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_total_tokens(2_500)));
    let (runner, _) = runner_for(Arc::new(agent), RunConfig::default()).await;
    let config = RunConfig { budget_tracker: Some(tracker.clone()), ..RunConfig::default() };

    let results = run(&runner, Some(config)).await;

    // 1,000 + 1,000 tokens leave room for a third call; counting the partial
    // chunks would have stopped the run after the first.
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    assert_eq!(budget_error(&results).code, "budget.total_tokens");
    let usage = tracker.usage();
    assert_eq!((usage.model_calls, usage.total_tokens), (3, 3_000));
}

#[tokio::test]
async fn max_tool_calls_stops_before_a_batch_that_does_not_fit() {
    let model = Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| {
        vec![chunk(vec![call("work"), call("work")], usage(10, 10), false)]
    }));
    let work = Arc::new(CountingTool::new("work"));
    let agent =
        LlmAgentBuilder::new("assistant").model(model.clone()).tool(work.clone()).build().unwrap();
    let (runner, _) = runner_for(
        Arc::new(agent),
        RunConfig::builder().budget(RunBudget::new().max_tool_calls(3)).build(),
    )
    .await;

    let results = run(&runner, None).await;

    assert_eq!(work.runs.load(Ordering::SeqCst), 2);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(budget_error(&results).code, "budget.tool_calls");
    // Every call of the refused batch is answered, so the session history stays valid.
    let refused = results[results.len() - 3].as_ref().unwrap();
    let responses: Vec<_> = refused.tool_results();
    assert_eq!(responses.len(), 2);
    assert!(responses.iter().all(|result| {
        result.response["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("not run: budget exhausted"))
    }));
}

#[tokio::test]
async fn wall_time_stops_the_next_model_call() {
    let model = Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| {
        vec![chunk(vec![call("slow")], usage(10, 10), false)]
    }));
    let slow =
        Arc::new(CountingTool { delay: Duration::from_millis(80), ..CountingTool::new("slow") });
    let agent =
        LlmAgentBuilder::new("assistant").model(model.clone()).tool(slow.clone()).build().unwrap();
    let (runner, _) = runner_for(
        Arc::new(agent),
        RunConfig::builder()
            .budget(RunBudget::new().max_wall_time(Duration::from_millis(50)))
            .build(),
    )
    .await;

    let results = run(&runner, None).await;

    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(slow.runs.load(Ordering::SeqCst), 1);
    assert_eq!(budget_error(&results).code, "budget.wall_time");
}

#[tokio::test]
async fn a_cost_cap_fails_closed_on_an_unpriced_model() {
    let model = Arc::new(ScriptedModel::new("gemini-unreleased-x", |_| ten_dollar_turn("work")));
    let work = Arc::new(CountingTool::new("work"));
    let agent =
        LlmAgentBuilder::new("assistant").model(model.clone()).tool(work.clone()).build().unwrap();
    let (runner, _) = runner_for(Arc::new(agent), RunConfig::default()).await;

    let capped = RunConfig::builder().budget(RunBudget::new().max_cost_usd(50.0)).build();
    let results = run(&runner, Some(capped)).await;

    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(work.runs.load(Ordering::SeqCst), 0);
    let error = budget_error(&results);
    assert_eq!(error.code, "budget.cost_unknown");
    assert!(error.message.contains("cost unknown for model 'gemini-unreleased-x'"), "{error}");

    // The documented opt-out counts the unpriced calls as free.
    let opted_out = RunConfig::builder()
        .budget(RunBudget::new().max_cost_usd(50.0).max_model_calls(3).allow_unpriced_models())
        .build();
    let results = run(&runner, Some(opted_out)).await;
    assert_eq!(budget_error(&results).code, "budget.model_calls");
    assert_eq!(model.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn an_agent_tool_shares_its_parents_budget() {
    let parent_model = Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| {
        vec![chunk(vec![call("researcher")], usage(10, 10), false)]
    }));
    let child_model =
        Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| answer_turn("found it", 20)));
    let researcher = LlmAgentBuilder::new("researcher")
        .description("Looks things up")
        .model(child_model.clone())
        .build()
        .unwrap();
    let parent = LlmAgentBuilder::new("assistant")
        .model(parent_model.clone())
        .tool(Arc::new(adk_tool::AgentTool::new(Arc::new(researcher))))
        .build()
        .unwrap();
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(3)));
    let (runner, _) = runner_for(Arc::new(parent), RunConfig::default()).await;

    let results = run(
        &runner,
        Some(RunConfig { budget_tracker: Some(tracker.clone()), ..RunConfig::default() }),
    )
    .await;

    // Parent, child, parent: the third call exhausts the shared budget, so the
    // child's second call never starts and the run ends.
    assert_eq!(parent_model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(child_model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(tracker.usage().model_calls, 3);
    assert_eq!(budget_error(&results).code, "budget.model_calls");
}

#[tokio::test]
async fn workflow_sub_agents_share_one_budget() {
    let first = Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| answer_turn("one", 20)));
    let second = Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| answer_turn("two", 20)));
    let pipeline = SequentialAgent::new(
        "pipeline",
        vec![
            Arc::new(LlmAgentBuilder::new("first").model(first.clone()).build().unwrap()),
            Arc::new(LlmAgentBuilder::new("second").model(second.clone()).build().unwrap()),
        ],
    );
    let (runner, _) = runner_for(
        Arc::new(pipeline),
        RunConfig::builder().budget(RunBudget::new().max_model_calls(1)).build(),
    )
    .await;

    let results = run(&runner, None).await;

    assert_eq!(first.calls.load(Ordering::SeqCst), 1);
    assert_eq!(second.calls.load(Ordering::SeqCst), 0);
    assert_eq!(budget_error(&results).code, "budget.model_calls");
    assert_eq!(stop_event(&results).author, "first");
}

#[tokio::test]
async fn each_run_gets_fresh_counters() {
    let model = Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| answer_turn("hi", 20)));
    let agent = LlmAgentBuilder::new("assistant").model(model.clone()).build().unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "budget-app".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("budget-app")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .budget(RunBudget::new().max_model_calls(1))
        .build()
        .unwrap();

    for _ in 0..3 {
        let results = run(&runner, None).await;
        assert!(results.iter().all(std::result::Result::is_ok));
    }
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
}

/// Model events carry one bounded request copy, so a long history no longer
/// makes every new event larger (D4).
#[tokio::test]
async fn model_event_size_does_not_grow_with_history() {
    async fn terminal_event_size(history_turns: usize) -> usize {
        let model =
            Arc::new(ScriptedModel::new("gemini-3.5-flash-lite", |_| answer_turn("ok", 20)));
        let agent = LlmAgentBuilder::new("assistant").model(model).build().unwrap();
        let (runner, sessions) = runner_for(Arc::new(agent), RunConfig::default()).await;
        let identity = adk_core::AdkIdentity::new(
            "budget-app".try_into().unwrap(),
            "user".try_into().unwrap(),
            "session".try_into().unwrap(),
        );
        for turn in 0..history_turns {
            let mut event = Event::new(format!("inv-{turn}"));
            event.author = "user".to_string();
            event.llm_response.content = Some(Content::new("user").with_text("x".repeat(4 * 1024)));
            sessions
                .append_event_for_identity(adk_session::AppendEventRequest {
                    identity: identity.clone(),
                    event,
                })
                .await
                .unwrap();
        }
        let results = run(&runner, None).await;
        let terminal = results
            .iter()
            .filter_map(|result| result.as_ref().ok())
            .find(|event| !event.llm_response.partial && event.author == "assistant")
            .unwrap();
        assert!(!terminal.provider_metadata.contains_key("gcp.vertex.agent.llm_request"));
        serde_json::to_vec(terminal).unwrap().len()
    }

    let short = terminal_event_size(2).await;
    let long = terminal_event_size(200).await;
    // 200 turns of 4 KiB is 800 KiB of history; the event stays a few KiB.
    assert!(long < 8 * 1024, "terminal event is {long} bytes");
    assert!(long.abs_diff(short) < 256, "{short} vs {long}");
}
