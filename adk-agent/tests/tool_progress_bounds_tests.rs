//! Tool progress must be bounded.
//!
//! Each tool batch previously created a `tokio::sync::mpsc::unbounded_channel`,
//! and `emit_progress` sent into it without backpressure or any aggregate limit. A
//! tool producing output faster than the client consumed it — a compiler log, a
//! shell command, a faulty loop — grew the queue until it was drained or the
//! process ran out of memory.
//!
//! The queue is now bounded, each call has a byte budget, and output beyond the
//! budget is replaced by a single deterministic marker.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, FinishReason, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, Session, State, Tool, ToolContext,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The marker the agent emits in place of progress it did not forward.
const TRUNCATION_MARKER: &str = "[adk: tool progress truncated]";

/// The per-call byte budget the agent enforces.
const MAX_TOTAL_BYTES: usize = 1024 * 1024;

// ── Mocks ─────────────────────────────────────────────────────────────

struct SequencedModel {
    responses: Arc<Mutex<VecDeque<LlmResponse>>>,
}

impl SequencedModel {
    fn new(responses: Vec<LlmResponse>) -> Self {
        Self { responses: Arc::new(Mutex::new(responses.into_iter().collect())) }
    }

    fn call(name: &str, id: &str) -> LlmResponse {
        Self::response(Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: name.to_string(),
                args: json!({}),
                id: Some(id.to_string()),
                thought_signature: None,
            }],
        }))
    }

    fn text(text: &str) -> LlmResponse {
        Self::response(Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: text.to_string() }],
        }))
    }

    fn response(content: Option<Content>) -> LlmResponse {
        LlmResponse {
            content,
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: None,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata: None,
            interaction_id: None,
            model: None,
            provider: None,
        }
    }
}

#[async_trait]
impl Llm for SequencedModel {
    fn name(&self) -> &str {
        "sequenced-model"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| SequencedModel::text("done"));
        let s = async_stream::stream! { yield Ok(next); };
        Ok(Box::pin(s))
    }
}

/// A tool that emits far more progress than the per-call budget allows.
struct FloodingTool {
    /// Chunks the tool attempted to emit.
    attempted: Arc<AtomicUsize>,
    chunk_bytes: usize,
    chunks: usize,
}

#[async_trait]
impl Tool for FloodingTool {
    fn name(&self) -> &str {
        "flooding_tool"
    }

    fn description(&self) -> &str {
        "emits a great deal of progress"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": {} }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let chunk = "x".repeat(self.chunk_bytes);
        for _ in 0..self.chunks {
            self.attempted.fetch_add(1, Ordering::Relaxed);
            ctx.emit_progress("stdout", &chunk).await;
        }
        Ok(json!({ "ok": true }))
    }
}

struct MockState;

impl State for MockState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct MockSession;

impl Session for MockSession {
    fn id(&self) -> &str {
        "session-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct MockContext {
    session: MockSession,
    user_content: Content,
}

#[async_trait]
impl adk_core::ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl adk_core::CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not exercised")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        static RUN_CONFIG: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        RUN_CONFIG.get_or_init(RunConfig::default)
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

fn context() -> Arc<MockContext> {
    Arc::new(MockContext {
        session: MockSession,
        user_content: Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "go".to_string() }],
        },
    })
}

/// Total bytes of progress text the stream delivered, and whether the marker appeared.
async fn run_and_measure(tool: Arc<FloodingTool>) -> (usize, bool, usize) {
    let model = Arc::new(SequencedModel::new(vec![
        SequencedModel::call("flooding_tool", "call-1"),
        SequencedModel::text("finished"),
    ]));
    let agent = LlmAgentBuilder::new("test-agent").model(model).tool(tool).build().unwrap();

    let mut stream = agent.run(context()).await.unwrap();
    let mut progress_bytes = 0;
    let mut progress_events = 0;
    let mut saw_marker = false;
    while let Some(result) = stream.next().await {
        let event = result.expect("the run must not fail");
        if event.tool_progress_stream().is_none() {
            continue;
        }
        progress_events += 1;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    if text == TRUNCATION_MARKER {
                        saw_marker = true;
                    } else {
                        progress_bytes += text.len();
                    }
                }
            }
        }
    }
    (progress_bytes, saw_marker, progress_events)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn progress_beyond_the_call_budget_is_replaced_by_one_marker() {
    // 4 MiB attempted against a 1 MiB budget.
    let tool = Arc::new(FloodingTool {
        attempted: Arc::new(AtomicUsize::new(0)),
        chunk_bytes: 8 * 1024,
        chunks: 512,
    });
    let (progress_bytes, saw_marker, _events) = run_and_measure(tool.clone()).await;

    assert!(
        tool.attempted.load(Ordering::Relaxed) > 0,
        "the tool must actually have emitted progress"
    );
    assert!(
        progress_bytes <= MAX_TOTAL_BYTES,
        "forwarded {progress_bytes} bytes of progress, above the {MAX_TOTAL_BYTES} byte budget"
    );
    assert!(saw_marker, "output dropped for budget reasons must be reported by a marker");
}

#[tokio::test]
async fn a_single_oversized_chunk_is_capped() {
    // One 64 KiB chunk against the 8 KiB per-chunk cap.
    let tool = Arc::new(FloodingTool {
        attempted: Arc::new(AtomicUsize::new(0)),
        chunk_bytes: 64 * 1024,
        chunks: 1,
    });
    let (progress_bytes, _marker, events) = run_and_measure(tool).await;

    assert_eq!(events, 1, "one emitted chunk must produce one progress event");
    assert!(
        progress_bytes <= 8 * 1024,
        "a single chunk forwarded {progress_bytes} bytes, above the 8 KiB per-chunk cap"
    );
}

#[tokio::test]
async fn modest_progress_is_forwarded_intact() {
    // Guards against the budget swallowing ordinary output.
    let tool = Arc::new(FloodingTool {
        attempted: Arc::new(AtomicUsize::new(0)),
        chunk_bytes: 16,
        chunks: 8,
    });
    let (progress_bytes, saw_marker, events) = run_and_measure(tool).await;

    assert_eq!(events, 8, "every modest chunk must be forwarded");
    assert_eq!(progress_bytes, 128);
    assert!(!saw_marker, "ordinary output must not be reported as truncated");
}

// ── Forwarded events ──────────────────────────────────────────────────

/// The marker the agent emits once after dropping forwarded partial events.
const FORWARDED_DROP_MARKER: &str = "[adk: forwarded events dropped]";

/// Events a tool's worker task forwards per run.
const FORWARDED_EVENTS: usize = 300;

/// A tool whose worker task forwards an event per step while the tool waits for it.
struct ForwardingTool {
    partial: bool,
}

#[async_trait]
impl Tool for ForwardingTool {
    fn name(&self) -> &str {
        "forwarding_tool"
    }

    fn description(&self) -> &str {
        "reports each step from a worker task"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": {} }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let partial = self.partial;
        let reporter = ctx.clone();
        let worker = tokio::spawn(async move {
            for step in 0..FORWARDED_EVENTS {
                let mut event = adk_core::Event::new(reporter.invocation_id());
                event.author = "worker".to_string();
                event.llm_response.partial = partial;
                event.llm_response.content =
                    Some(Content::new("model").with_text(format!("step {step}")));
                reporter.emit_event(event).await;
            }
        });
        let _ = worker.await;
        Ok(json!({ "ok": true }))
    }
}

/// Forwarded events and drop markers a client receives when it stalls once, mid-run, for
/// longer than a forwarded event may wait.
async fn forward_to_a_stalling_client(partial: bool) -> (usize, usize) {
    let model = Arc::new(SequencedModel::new(vec![
        SequencedModel::call("forwarding_tool", "call-1"),
        SequencedModel::text("finished"),
    ]));
    let agent = LlmAgentBuilder::new("test-agent")
        .model(model)
        .tool(Arc::new(ForwardingTool { partial }))
        .build()
        .unwrap();

    let mut stream = agent.run(context()).await.unwrap();
    let mut forwarded = 0;
    let mut markers = 0;
    while let Some(result) = stream.next().await {
        let event = result.expect("the run must not fail");
        if event.author == "worker" {
            forwarded += 1;
            if forwarded == 1 {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
        }
        let is_marker = event
            .llm_response
            .content
            .iter()
            .flat_map(|content| &content.parts)
            .any(|part| matches!(part, Part::Text { text } if text == FORWARDED_DROP_MARKER));
        if is_marker {
            markers += 1;
        }
    }
    (forwarded, markers)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_forwarded_events_wait_for_a_slow_client() {
    let (forwarded, markers) = forward_to_a_stalling_client(false).await;
    assert_eq!(forwarded, FORWARDED_EVENTS, "a complete event is session history");
    assert_eq!(markers, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_partial_forwarded_events_are_marked_once() {
    let (forwarded, markers) = forward_to_a_stalling_client(true).await;
    assert!(forwarded < FORWARDED_EVENTS, "the stall must have dropped partial events");
    assert_eq!(markers, 1, "the drop is reported by one marker");
}

// ── Truncation under a full queue ─────────────────────────────────────

/// Progress chunks a tool's worker task emits per run; more than the queue holds.
const WORKER_CHUNKS: usize = 1_000;

/// A tool whose worker task streams progress while the tool waits for it, as a shell tool
/// forwards a child's output.
struct StreamingWorkerTool;

#[async_trait]
impl Tool for StreamingWorkerTool {
    fn name(&self) -> &str {
        "streaming_worker"
    }

    fn description(&self) -> &str {
        "streams progress from a worker task"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": {} }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let reporter = ctx.clone();
        let worker = tokio::spawn(async move {
            for line in 0..WORKER_CHUNKS {
                reporter.emit_progress("stdout", &format!("line {line}\n")).await;
            }
        });
        let _ = worker.await;
        Ok(json!({ "ok": true }))
    }
}

/// The marker is what tells a client that stalled while the queue was full that output is
/// missing, so it must reach that client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_truncation_marker_reaches_a_client_that_stalled_on_a_full_queue() {
    let model = Arc::new(SequencedModel::new(vec![
        SequencedModel::call("streaming_worker", "call-1"),
        SequencedModel::text("finished"),
    ]));
    let agent = LlmAgentBuilder::new("test-agent")
        .model(model)
        .tool(Arc::new(StreamingWorkerTool))
        .build()
        .unwrap();

    let mut stream = agent.run(context()).await.unwrap();
    let mut forwarded = 0;
    let mut markers = 0;
    while let Some(result) = stream.next().await {
        let event = result.expect("the run must not fail");
        if event.tool_progress_stream().is_none() {
            continue;
        }
        let is_marker = event
            .llm_response
            .content
            .iter()
            .flat_map(|content| &content.parts)
            .any(|part| matches!(part, Part::Text { text } if text == TRUNCATION_MARKER));
        if is_marker {
            markers += 1;
            continue;
        }
        forwarded += 1;
        if forwarded == 1 {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
    }

    assert!(forwarded < WORKER_CHUNKS, "the stall must have dropped progress");
    assert_eq!(markers, 1, "the dropped progress is reported by one marker");
}
