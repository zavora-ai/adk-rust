//! Every answer `LlmAgent` writes in place of a tool's result carries a `ToolResultStatus`.
//!
//! Each test runs one framework-answer path and compares the whole function response part the
//! agent writes: payload, call ID, and the status and cause annotations. A tool's own result,
//! and an answer a callback supplies, carry none — except an unknown outcome, which no callback
//! settles.

use adk_agent::{CustomAgentBuilder, LlmAgentBuilder};
use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, AdkError, Agent, ApprovalScope, ApprovalStore,
    BackpressurePolicy, BudgetTracker, CallbackContext, Content, ErrorCategory, ErrorComponent,
    Event, FunctionResponseData, GovernanceControl, InMemoryActionLedger, InMemoryApprovalStore,
    InvocationContext, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result, RunBudget,
    RunConfig, Session, State, TOOL_PERMISSION_DENIED_CODE, TOOL_RESULT_ERROR_CATEGORY_KEY,
    TOOL_RESULT_ERROR_CODE_KEY, TOOL_RESULT_STATUS_KEY, Tool, ToolConcurrencyConfig,
    ToolConfirmationDecision, ToolConfirmationRequest, ToolContext, ToolEffect,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ----- harness ---------------------------------------------------------------

const LEDGER_KEY: &str = "shop/alice/session-1/inv-1/pay-1";

struct EmptyState;

impl State for EmptyState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct EmptySession;

impl Session for EmptySession {
    fn id(&self) -> &str {
        "session-1"
    }
    fn app_name(&self) -> &str {
        "shop"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn state(&self) -> &dyn State {
        &EmptyState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

/// One invocation, `inv-1`, of app `shop` for user `alice` in `session-1`, so a call's
/// idempotency key is known before the run.
struct Invocation {
    user_content: Content,
    run_config: RunConfig,
}

#[async_trait]
impl adk_core::ReadonlyContext for Invocation {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "shopper"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn app_name(&self) -> &str {
        "shop"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for Invocation {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for Invocation {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not used by these tests")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &EmptySession
    }
    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

/// Replays scripted responses, then answers "done".
struct ScriptedModel(Mutex<VecDeque<LlmResponse>>);

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let response = self.0.lock().unwrap().pop_front().unwrap_or_else(|| LlmResponse {
            content: Some(Content::new("model").with_text("done")),
            turn_complete: true,
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// A model turn that issues `calls`, each a tool name, arguments and call ID.
fn turn(calls: &[(&str, Value, &str)]) -> Arc<dyn Llm> {
    let parts = calls
        .iter()
        .map(|(name, args, id)| Part::FunctionCall {
            name: name.to_string(),
            args: args.clone(),
            id: Some(id.to_string()),
            thought_signature: None,
        })
        .collect();
    let response = LlmResponse {
        content: Some(Content { role: "model".to_string(), parts }),
        turn_complete: true,
        ..Default::default()
    };
    Arc::new(ScriptedModel(Mutex::new(VecDeque::from([response]))))
}

/// A model turn that calls `pay` once, as `pay-1`.
fn pays() -> Arc<dyn Llm> {
    turn(&[("pay", json!({ "amount": 50 }), "pay-1")])
}

/// The events of a run, and the error that ended it, if one did.
async fn run(agent: &dyn Agent, run_config: RunConfig) -> (Vec<Event>, Option<AdkError>) {
    let ctx: Arc<dyn InvocationContext> =
        Arc::new(Invocation { user_content: Content::new("user").with_text("buy it"), run_config });
    let mut stream = agent.run(ctx).await.expect("agent starts");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => events.push(event),
            Err(error) => return (events, Some(error)),
        }
    }
    (events, None)
}

/// The function response part that answers call `id`.
fn answer(events: &[Event], id: &str) -> Part {
    events
        .iter()
        .filter(|event| !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .find(|part| matches!(part, Part::FunctionResponse { id: Some(answered), .. } if answered == id))
        .cloned()
        .unwrap_or_else(|| panic!("{id} is answered: {events:#?}"))
}

/// A function response to call `id` of `name` with `annotations`.
fn part(name: &str, id: &str, response: Value, annotations: Option<Value>) -> Part {
    Part::FunctionResponse {
        function_response: FunctionResponseData::new(name, response),
        id: Some(id.to_string()),
        annotations,
    }
}

/// The annotations of a status with no error behind it.
fn status(status: &str) -> Option<Value> {
    Some(json!({ TOOL_RESULT_STATUS_KEY: status }))
}

/// The annotations of a status caused by an error with `code` and `category`.
fn caused(status: &str, code: &str, category: &str) -> Option<Value> {
    Some(json!({
        TOOL_RESULT_STATUS_KEY: status,
        TOOL_RESULT_ERROR_CODE_KEY: code,
        TOOL_RESULT_ERROR_CATEGORY_KEY: category,
    }))
}

/// `pay`: returns `result`, fails with `error`, or hangs, counting its executions.
struct Pay {
    effect: ToolEffect,
    error: Option<fn() -> AdkError>,
    hang: bool,
    runs: Arc<AtomicUsize>,
}

impl Pay {
    fn new() -> Self {
        Self {
            effect: ToolEffect::NonIdempotent,
            error: None,
            hang: false,
            runs: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn failing(error: fn() -> AdkError) -> Self {
        Self { error: Some(error), ..Self::new() }
    }

    fn hanging(effect: ToolEffect) -> Self {
        Self { effect, hang: true, ..Self::new() }
    }
}

#[async_trait]
impl Tool for Pay {
    fn name(&self) -> &str {
        "pay"
    }
    fn description(&self) -> &str {
        "charges a card"
    }
    fn effect(&self) -> ToolEffect {
        self.effect
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        if self.hang {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        match &self.error {
            Some(error) => Err(error()),
            None => Ok(json!({ "charged": true })),
        }
    }
}

fn down() -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unavailable,
        "tool.pay.down",
        "card api down",
    )
}

fn audit_down() -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Unavailable,
        "audit.down",
        "audit sink down",
    )
}

fn shopper(model: Arc<dyn Llm>, tool: Pay) -> LlmAgentBuilder {
    LlmAgentBuilder::new("shopper").model(model).tool(Arc::new(tool))
}

// ----- before the governed path -----------------------------------------------

#[tokio::test]
async fn undecodable_arguments_are_not_run() {
    let args = adk_core::invalid_tool_arguments("arguments are not valid JSON", "{\"amount\": ");
    let agent = shopper(turn(&[("pay", args, "pay-1")]), Pay::new()).build().unwrap();

    let (events, _) = run(&agent, RunConfig::default()).await;

    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": "tool 'pay' was not run: arguments are not valid JSON. Call it again \
                              with arguments that are one JSON object matching its parameters." }),
            status("not_run"),
        )
    );
}

#[tokio::test]
async fn a_failing_before_tool_callback_is_not_run_with_its_cause() {
    let agent = shopper(pays(), Pay::new())
        .before_tool_callback(Box::new(|_| Box::pin(async { Err(audit_down()) })))
        .build()
        .unwrap();

    let (events, _) = run(&agent, RunConfig::default()).await;

    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": "tool 'pay' was not run: a before-tool callback failed with \
                              agent.unavailable (audit.down)" }),
            caused("not_run", "audit.down", "unavailable"),
        )
    );
}

#[tokio::test]
async fn a_before_tool_callback_text_answer_is_not_run_and_its_function_response_is_its_own() {
    let agent = shopper(pays(), Pay::new())
        .before_tool_callback(Box::new(|_| {
            Box::pin(async { Ok(Some(Content::new("model").with_text("payments close at 5"))) })
        }))
        .build()
        .unwrap();
    let (events, _) = run(&agent, RunConfig::default()).await;
    assert_eq!(
        answer(&events, "pay-1"),
        part("pay", "pay-1", json!({ "error": "payments close at 5" }), status("not_run"))
    );

    // A substitute result is the callback's answer: it carries the callback's annotations.
    let agent = shopper(pays(), Pay::new())
        .before_tool_callback(Box::new(|_| {
            Box::pin(async {
                Ok(Some(Content {
                    role: "function".to_string(),
                    parts: vec![part("pay", "cache", json!({ "charged": "cached" }), None)],
                }))
            })
        }))
        .build()
        .unwrap();
    let (events, _) = run(&agent, RunConfig::default()).await;
    assert_eq!(
        answer(&events, "pay-1"),
        part("pay", "pay-1", json!({ "charged": "cached" }), None)
    );
}

#[tokio::test]
async fn an_open_circuit_is_not_run_after_a_failure() {
    let model = turn(&[("pay", json!({}), "pay-1"), ("pay", json!({}), "pay-2")]);
    let agent = shopper(model, Pay::failing(down)).circuit_breaker_threshold(1).build().unwrap();

    let (events, _) = run(&agent, RunConfig::default()).await;

    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": "tool 'pay' failed: tool.unavailable (tool.pay.down)" }),
            caused("failed", "tool.pay.down", "unavailable"),
        )
    );
    assert_eq!(
        answer(&events, "pay-2"),
        part(
            "pay",
            "pay-2",
            json!({ "error": "Tool 'pay' is temporarily disabled after 1 consecutive failures" }),
            status("not_run"),
        )
    );
}

#[tokio::test]
async fn an_unknown_tool_is_not_run() {
    let agent = shopper(turn(&[("refund", json!({}), "refund-1")]), Pay::new()).build().unwrap();

    let (events, _) = run(&agent, RunConfig::default()).await;

    assert_eq!(
        answer(&events, "refund-1"),
        part("refund", "refund-1", json!({ "error": "Tool refund not found" }), status("not_run"))
    );
}

// ----- the governed path ---------------------------------------------------------

#[tokio::test]
async fn a_policy_denial_and_a_declined_confirmation_are_refused() {
    let policy = adk_core::DeclarativePolicy::builder().deny("pay", "payments are closed").build();
    let agent = shopper(pays(), Pay::new()).build().unwrap();
    let (events, _) = run(&agent, RunConfig::builder().tool_policy(Arc::new(policy)).build()).await;
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": "Tool 'pay' denied by policy: payments are closed" }),
            status("refused"),
        )
    );

    let agent = shopper(pays(), Pay::new()).require_tool_confirmation("pay").build().unwrap();
    let config = RunConfig::builder()
        .tool_confirmation_decisions(HashMap::from([(
            "pay-1".to_string(),
            ToolConfirmationDecision::Deny,
        )]))
        .build();
    let (events, _) = run(&agent, config).await;
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": "Tool 'pay' execution denied by confirmation policy" }),
            status("refused"),
        )
    );
}

#[tokio::test]
async fn a_call_waiting_for_a_decision_is_held() {
    let agent = shopper(pays(), Pay::new()).require_tool_confirmation("pay").build().unwrap();

    let (events, _) = run(&agent, RunConfig::default()).await;

    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            adk_core::confirmation_pending_response("Tool 'pay' requires confirmation"),
            status("held"),
        )
    );
}

#[tokio::test]
async fn a_call_the_kill_switch_refuses_is_not_run_with_its_cause() {
    let control = GovernanceControl::new();
    let freezer = control.clone();
    let agent = shopper(pays(), Pay::new())
        .before_tool_callback(Box::new(move |_| {
            freezer.freeze("incident 12");
            Box::pin(async { Ok(None) })
        }))
        .build()
        .unwrap();
    let config = RunConfig::builder().governance(control).build();

    let (events, error) = run(&agent, config.clone()).await;

    let frozen = config.check_governance().unwrap_err();
    assert_eq!(error.map(|error| error.code), Some(frozen.code));
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": adk_core::not_run_text(&frozen) }),
            caused("not_run", frozen.code, "forbidden"),
        )
    );
}

/// Never decides, so the call waits until something else ends the wait.
#[derive(Debug)]
struct NeverDecides;

#[async_trait]
impl adk_core::ToolConfirmationHandler for NeverDecides {
    async fn decide(&self, _request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        std::future::pending().await
    }
}

/// A freeze that ends the wait for a confirmation (#1305) refuses the call as the kill switch
/// does anywhere else: it did not run.
#[tokio::test(start_paused = true)]
async fn a_freeze_that_ends_a_confirmation_wait_is_not_run_with_its_cause() {
    let control = GovernanceControl::new();
    let freezer = control.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        freezer.freeze("incident 4016");
    });
    let tool = Pay::new();
    let runs = tool.runs.clone();
    let agent = shopper(pays(), tool).require_tool_confirmation("pay").build().unwrap();
    let config = RunConfig::builder()
        .tool_confirmation_handler(Arc::new(NeverDecides))
        .governance(control)
        .build();

    let (events, error) = run(&agent, config.clone()).await;

    let frozen = config.check_governance().unwrap_err();
    assert_eq!(error.map(|error| error.code), Some(frozen.code));
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": adk_core::not_run_text(&frozen) }),
            caused("not_run", frozen.code, "forbidden"),
        )
    );
}

/// Freezes `control` as it begins a record, as a freeze landing during the ledger write does.
#[derive(Debug)]
struct FreezingLedger {
    inner: InMemoryActionLedger,
    control: GovernanceControl,
}

#[async_trait]
impl ActionLedger for FreezingLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        self.inner.begin(record).await?;
        self.control.freeze("incident 7");
        Ok(())
    }
    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        self.inner.complete(key, outcome).await
    }
    async fn get(&self, key: &str) -> Result<Option<ActionRecord>> {
        self.inner.get(key).await
    }
}

#[tokio::test]
async fn a_freeze_after_the_ledger_write_is_not_run_and_so_is_its_replay() {
    let control = GovernanceControl::new();
    let ledger =
        Arc::new(FreezingLedger { inner: InMemoryActionLedger::new(), control: control.clone() });
    let config = RunConfig::builder().action_ledger(ledger).governance(control.clone()).build();

    let (events, _) = run(&shopper(pays(), Pay::new()).build().unwrap(), config.clone()).await;
    let frozen = config.check_governance().unwrap_err();
    let not_run = adk_core::not_run_text(&frozen);
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": not_run }),
            caused("not_run", frozen.code, "forbidden"),
        )
    );

    // The replay answers from the record of a call that never started.
    control.unfreeze();
    let (events, _) = run(&shopper(pays(), Pay::new()).build().unwrap(), config).await;
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({
                "status": "already_failed",
                "error": not_run,
                "detail": "this call to 'pay' already failed; it was not executed again",
            }),
            status("not_run"),
        )
    );
}

// ----- before execution ------------------------------------------------------------

#[tokio::test]
async fn a_concurrency_limit_is_not_run_with_its_cause() {
    let config = RunConfig::builder()
        .tool_concurrency(ToolConcurrencyConfig {
            max_concurrency: None,
            per_tool: HashMap::from([("pay".to_string(), 0)]),
            backpressure: BackpressurePolicy::Fail,
        })
        .build();
    let tool = Pay::new();
    let runs = tool.runs.clone();

    let (events, _) = run(&shopper(pays(), tool).build().unwrap(), config).await;

    let limit = AdkError::tool("concurrency limit reached: pay");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": limit.to_string() }),
            caused("not_run", limit.code, "internal"),
        )
    );
}

/// A ledger whose store is unreachable.
#[derive(Debug)]
struct UnreachableLedger;

fn unreachable_store() -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "ledger.unreachable",
        "postgres://ledger.internal refused",
    )
}

#[async_trait]
impl ActionLedger for UnreachableLedger {
    async fn begin(&self, _record: &ActionRecord) -> Result<()> {
        Err(unreachable_store())
    }
    async fn complete(&self, _key: &str, _outcome: ActionOutcome) -> Result<()> {
        Err(unreachable_store())
    }
    async fn get(&self, _key: &str) -> Result<Option<ActionRecord>> {
        Err(unreachable_store())
    }
}

#[tokio::test]
async fn an_unreadable_ledger_is_not_run_with_its_cause() {
    let config = RunConfig::builder().action_ledger(Arc::new(UnreachableLedger)).build();

    let (events, _) = run(&shopper(pays(), Pay::new()).build().unwrap(), config).await;

    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": "tool 'pay' was not executed: the action ledger could not be read: \
                              session.unavailable (ledger.unreachable)" }),
            caused("not_run", "ledger.unreachable", "unavailable"),
        )
    );
}

/// A ledger holding, under `pay-1`'s key, a record completed with `outcome` (begun only when
/// `None`).
async fn ledger_with(outcome: Option<ActionOutcome>) -> Arc<InMemoryActionLedger> {
    let ledger = Arc::new(InMemoryActionLedger::new());
    let record =
        ActionRecord::new(LEDGER_KEY, "pay", &json!({ "amount": 50 }), ToolEffect::NonIdempotent);
    ledger.begin(&record).await.unwrap();
    if let Some(outcome) = outcome {
        ledger.complete(LEDGER_KEY, outcome).await.unwrap();
    }
    ledger
}

#[tokio::test]
async fn a_replay_reports_the_recorded_outcome() {
    let cases = [
        (
            Some(ActionOutcome::Succeeded { result_digest: "fnv1a128:abc".into() }),
            json!({
                "status": "already_succeeded",
                "result_digest": "fnv1a128:abc",
                "detail": "this call to 'pay' already completed; it was not executed again",
            }),
            "succeeded",
        ),
        (
            Some(ActionOutcome::Failed { error: "card declined".into() }),
            json!({
                "status": "already_failed",
                "error": "card declined",
                "detail": "this call to 'pay' already failed; it was not executed again",
            }),
            "failed",
        ),
        (
            None,
            adk_core::outcome_unknown_response(
                "an earlier attempt of this call to 'pay' began but recorded no outcome, so it \
                 may have taken effect; it was not executed again",
            ),
            "outcome_unknown",
        ),
    ];
    for (outcome, response, expected) in cases {
        let tool = Pay::new();
        let runs = tool.runs.clone();
        let config = RunConfig::builder().action_ledger(ledger_with(outcome).await).build();

        let (events, _) = run(&shopper(pays(), tool).build().unwrap(), config).await;

        assert_eq!(runs.load(Ordering::SeqCst), 0, "{expected}");
        assert_eq!(answer(&events, "pay-1"), part("pay", "pay-1", response, status(expected)));
    }
}

// ----- execution -------------------------------------------------------------------

#[tokio::test]
async fn a_tool_error_is_failed_and_an_access_denial_refused() {
    fn denied() -> AdkError {
        AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Forbidden,
            TOOL_PERMISSION_DENIED_CODE,
            "missing required scopes: [payments]",
        )
    }
    let cases: [(fn() -> AdkError, _); 2] = [
        (down, caused("failed", "tool.pay.down", "unavailable")),
        (denied, caused("refused", TOOL_PERMISSION_DENIED_CODE, "forbidden")),
    ];
    for (error, expected) in cases {
        let agent = shopper(pays(), Pay::failing(error)).build().unwrap();
        let (events, _) = run(&agent, RunConfig::default()).await;
        assert_eq!(
            answer(&events, "pay-1"),
            part("pay", "pay-1", adk_core::tool_error_response("pay", &error(), false), expected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_timeout_is_an_unknown_outcome_only_for_a_non_idempotent_tool() {
    let timed_out = "Tool 'pay' timed out after 50ms";
    let cases = [
        (
            ToolEffect::NonIdempotent,
            adk_core::outcome_unknown_response(format!(
                "{timed_out}. The call may have taken effect; check its result before calling it \
                 again."
            )),
            caused("outcome_unknown", "tool.timeout", "timeout"),
        ),
        (
            ToolEffect::Idempotent,
            json!({ "error": timed_out }),
            caused("failed", "tool.timeout", "timeout"),
        ),
    ];
    for (effect, response, expected) in cases {
        let agent = shopper(pays(), Pay::hanging(effect))
            .tool_timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let (events, _) = run(&agent, RunConfig::default()).await;
        assert_eq!(
            answer(&events, "pay-1"),
            part("pay", "pay-1", response, expected),
            "{effect:?}"
        );
    }
}

// ----- after execution ---------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_fallback_is_its_own_answer_unless_the_outcome_is_unknown() {
    let fallback = || -> adk_core::OnToolErrorCallback {
        Box::new(|_, _, _, _| Box::pin(async { Ok(Some(json!({ "queued": true }))) }))
    };
    let agent_down = shopper(pays(), Pay::failing(down)).on_tool_error(fallback()).build().unwrap();
    let (events, _) = run(&agent_down, RunConfig::default()).await;
    assert_eq!(answer(&events, "pay-1"), part("pay", "pay-1", json!({ "queued": true }), None));

    let agent_hung = shopper(pays(), Pay::hanging(ToolEffect::NonIdempotent))
        .tool_timeout(Duration::from_millis(50))
        .on_tool_error(fallback())
        .after_tool_callback(Box::new(|_| {
            Box::pin(async { Ok(Some(Content::new("model").with_text("try later"))) })
        }))
        .build()
        .unwrap();
    let (events, _) = run(&agent_hung, RunConfig::default()).await;
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "result": "try later" }),
            caused("outcome_unknown", "tool.timeout", "timeout"),
        )
    );
}

#[tokio::test]
async fn an_after_tool_replacement_carries_no_status() {
    let agent = shopper(pays(), Pay::failing(down))
        .after_tool_callback(Box::new(|_| {
            Box::pin(async { Ok(Some(Content::new("model").with_text("card api is down"))) })
        }))
        .build()
        .unwrap();

    let (events, _) = run(&agent, RunConfig::default()).await;

    assert_eq!(
        answer(&events, "pay-1"),
        part("pay", "pay-1", json!({ "result": "card api is down" }), None)
    );
}

#[tokio::test]
async fn a_failed_after_tool_stage_withholds_a_result_that_ran() {
    let failing_after =
        || -> adk_core::AfterToolCallback { Box::new(|_| Box::pin(async { Err(audit_down()) })) };
    let (events, _) = run(
        &shopper(pays(), Pay::new()).after_tool_callback(failing_after()).build().unwrap(),
        RunConfig::default(),
    )
    .await;
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            adk_core::result_withheld_response("pay", &audit_down(), false),
            caused("withheld", "audit.down", "unavailable"),
        )
    );

    // A call a before-tool callback answered never ran, so it is not run.
    let agent = shopper(pays(), Pay::new())
        .before_tool_callback(Box::new(|_| {
            Box::pin(async { Ok(Some(Content::new("model").with_text("closed"))) })
        }))
        .after_tool_callback(failing_after())
        .build()
        .unwrap();
    let (events, _) = run(&agent, RunConfig::default()).await;
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": "tool 'pay' was not run, and its after-tool pipeline failed with \
                              agent.unavailable (audit.down)" }),
            caused("not_run", "audit.down", "unavailable"),
        )
    );
}

/// A result screen that withholds every result, as a screening service failing closed does.
#[cfg(feature = "guardrails")]
struct WithholdAll;

#[cfg(feature = "guardrails")]
#[async_trait]
impl adk_agent::guardrails::ToolGuardrail for WithholdAll {
    fn name(&self) -> &str {
        "dlp"
    }
    async fn validate_result(
        &self,
        _tool: &str,
        _args: &Value,
        _result: &Value,
    ) -> adk_agent::guardrails::ToolResultGuardrailResult {
        adk_agent::guardrails::ToolResultGuardrailResult::withhold(
            "the screening service is down",
            adk_agent::guardrails::Severity::High,
        )
    }
}

/// #1006 labelled a replayed call that a result screen withheld `not_run`, though it ran.
#[cfg(feature = "guardrails")]
#[tokio::test]
async fn a_withheld_replay_is_withheld_because_the_call_ran() {
    use adk_core::ToolResultScreen as _;

    let screen = || adk_agent::guardrails::ToolGuardrailSet::new().with(WithholdAll);
    let reason = screen().screen_result("pay", &json!({}), &json!({})).await.unwrap_err();
    let blocked = adk_core::result_blocked_response("pay", &reason);
    let cases = [
        (Some(ActionOutcome::Succeeded { result_digest: "d".into() }), "withheld"),
        (Some(ActionOutcome::Failed { error: "card declined".into() }), "withheld"),
        (None, "outcome_unknown"),
    ];
    for (outcome, expected) in cases {
        let config = RunConfig::builder().action_ledger(ledger_with(outcome).await).build();
        let agent = shopper(pays(), Pay::new()).tool_guardrails(screen()).build().unwrap();

        let (events, _) = run(&agent, config).await;

        assert_eq!(
            answer(&events, "pay-1"),
            part("pay", "pay-1", blocked.clone(), status(expected)),
            "{expected}"
        );
    }

    // A fresh result the screen withholds ran, and a refusal it withholds never did.
    let (events, _) = run(
        &shopper(pays(), Pay::new()).tool_guardrails(screen()).build().unwrap(),
        RunConfig::default(),
    )
    .await;
    assert_eq!(answer(&events, "pay-1"), part("pay", "pay-1", blocked.clone(), status("withheld")));
    let policy = adk_core::DeclarativePolicy::builder().deny("pay", "closed").build();
    let (events, _) = run(
        &shopper(pays(), Pay::new()).tool_guardrails(screen()).build().unwrap(),
        RunConfig::builder().tool_policy(Arc::new(policy)).build(),
    )
    .await;
    assert_eq!(answer(&events, "pay-1"), part("pay", "pay-1", blocked, status("refused")));
}

// ----- the turn ------------------------------------------------------------------------

#[tokio::test]
async fn a_transfer_answers_the_other_calls_not_run() {
    let billing: Arc<dyn Agent> = Arc::new(
        CustomAgentBuilder::new("billing")
            .handler(|_| async { Ok(Box::pin(futures::stream::empty()) as adk_core::EventStream) })
            .build()
            .unwrap(),
    );
    let model = turn(&[
        ("transfer_to_agent", json!({ "agent_name": "billing" }), "move-1"),
        ("pay", json!({}), "pay-1"),
    ]);
    let agent = shopper(model, Pay::new()).sub_agent(billing.clone()).build().unwrap();
    let (events, _) = run(&agent, RunConfig::default()).await;
    assert_eq!(
        answer(&events, "move-1"),
        part("transfer_to_agent", "move-1", json!({ "transferred_to": "billing" }), None)
    );
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": "not run: control transferred to billing" }),
            status("not_run"),
        )
    );

    let model = turn(&[("transfer_to_agent", json!({ "agent_name": "nobody" }), "move-1")]);
    let agent = shopper(model, Pay::new()).sub_agent(billing).build().unwrap();
    let (events, _) = run(&agent, RunConfig::default()).await;
    assert_eq!(
        answer(&events, "move-1"),
        part(
            "transfer_to_agent",
            "move-1",
            json!({ "error": "Agent 'nobody' not found. Available agents: [\"billing\"]" }),
            status("not_run"),
        )
    );
}

#[tokio::test]
async fn a_batch_over_the_tool_budget_is_not_run_with_its_cause() {
    let budget = || RunBudget::new().max_tool_calls(0);
    let exceeded = BudgetTracker::new(budget()).begin_tool_calls(1).unwrap_err();
    let cause = AdkError::from(exceeded.clone());
    let config = RunConfig {
        budget_tracker: Some(Arc::new(BudgetTracker::new(budget()))),
        ..RunConfig::default()
    };

    let (events, error) = run(&shopper(pays(), Pay::new()).build().unwrap(), config).await;

    assert_eq!(error.map(|error| error.code), Some(cause.code));
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            json!({ "error": format!("not run: {exceeded}") }),
            caused("not_run", cause.code, "resource_exhausted"),
        )
    );
}

// ----- a delegate's hold -------------------------------------------------------------------

/// Asks for a confirmation as an agent tool's delegate does — records the request pending and
/// sets it on its actions — then returns, or hangs when `hang` is set.
struct Delegate {
    store: Arc<InMemoryApprovalStore>,
    hang: bool,
}

fn delegated_request() -> ToolConfirmationRequest {
    ToolConfirmationRequest::new("write_file", json!({ "path": "notes.md" }))
        .with_function_call_id("inner-1")
}

fn scope() -> ApprovalScope {
    ApprovalScope::new("shop", "alice", "session-1")
}

#[async_trait]
impl Tool for Delegate {
    fn name(&self) -> &str {
        "pay"
    }
    fn description(&self) -> &str {
        "delegates to an agent that needs approval"
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.store.record_pending(&scope(), &delegated_request()).await?;
        let mut actions = ctx.actions();
        actions.tool_confirmation = Some(delegated_request());
        ctx.set_actions(actions);
        if self.hang {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        Ok(json!({ "delegated": true }))
    }
}

#[tokio::test]
async fn a_delegates_request_holds_the_call() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let agent = LlmAgentBuilder::new("shopper")
        .model(pays())
        .tool(Arc::new(Delegate { store: store.clone(), hang: false }))
        .build()
        .unwrap();

    let (events, _) = run(&agent, RunConfig::builder().approval_store(store.clone()).build()).await;

    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            adk_core::confirmation_pending_response("Tool 'pay' is waiting for a confirmation"),
            status("held"),
        )
    );
    assert_eq!(store.pending(&scope()).await.unwrap().len(), 1, "the request waits");
}

/// #1006 dropped the hold of a call whose outcome is unknown but left its request pending, so a
/// later approval of it could authorize an identical call.
#[tokio::test(start_paused = true)]
async fn an_unknown_outcome_supersedes_a_delegates_request_and_withdraws_it() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let agent = LlmAgentBuilder::new("shopper")
        .model(pays())
        .tool(Arc::new(Delegate { store: store.clone(), hang: true }))
        .tool_timeout(Duration::from_millis(50))
        .build()
        .unwrap();

    let (events, error) =
        run(&agent, RunConfig::builder().approval_store(store.clone()).build()).await;

    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        answer(&events, "pay-1"),
        part(
            "pay",
            "pay-1",
            adk_core::outcome_unknown_response(
                "Tool 'pay' timed out after 50ms. The call may have taken effect; check its \
                 result before calling it again.",
            ),
            caused("outcome_unknown", "tool.timeout", "timeout"),
        )
    );
    assert!(
        events.iter().all(|event| event.actions.tool_confirmation.is_none()),
        "the call is not held"
    );
    assert!(store.pending(&scope()).await.unwrap().is_empty(), "the request is withdrawn");
    assert!(!store.has_decision(&scope(), &delegated_request().fingerprint()).await.unwrap());
}
