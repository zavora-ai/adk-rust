//! Regression tests for span parenting across suspension points.
//!
//! Two distinct defects are covered:
//!
//! 1. `LlmAgent::run` used to hold a `call_llm` span guard (`Span::enter()`)
//!    across the `.await`/`yield` points of its `async_stream` generator. An
//!    entered guard is bound to the *thread*, not the task, so it was not exited
//!    when the generator suspended nor re-entered when it resumed. Everything
//!    created after the first suspension point silently detached from `call_llm`.
//!
//! 2. `Runner` used to `.instrument()` only the future that *constructs* the
//!    agent stream, not the polling of it. Since the entire agent execution
//!    happens while the stream is drained, every span it produced was created
//!    outside `agent.execute`.
//!
//! Both are invisible without an assertion on the span *tree* — the spans are
//! still emitted, with correct names and durations, just attached to the wrong
//! parent. Hence these tests inspect parentage rather than span presence.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, FinishReason, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, Session, State,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use tracing::span::{Attributes, Id};
use tracing::{Instrument, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

// --- Span capture ---

/// Records `(span name, contextual parent name)` at span creation.
///
/// `lookup_current()` in `on_new_span` is precisely the signal both bugs
/// corrupt: the span is created, but the ambient parent at creation time is
/// wrong (or absent).
/// A span's name paired with the name of its contextual parent, if any.
type ParentEdge = (String, Option<String>);

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<ParentEdge>>>);

impl Capture {
    fn drain(&self) -> Vec<ParentEdge> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl<S> Layer<S> for Capture
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, ctx: Context<'_, S>) {
        let parent = ctx.lookup_current().map(|s| s.name().to_string());
        self.0.lock().unwrap().push((attrs.metadata().name().to_string(), parent));
    }
}

/// One global subscriber for the whole test binary — a thread-local subscriber
/// (`with_default`) would miss spans created on other runtime workers, which is
/// exactly where these bugs manifest.
fn capture() -> &'static Capture {
    static CAPTURE: OnceLock<Capture> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let c = Capture::default();
        tracing_subscriber::registry().with(c.clone()).init();
        c
    })
}

/// Contextual parent recorded for the first span named `name`.
fn parent_of<'a>(spans: &'a [ParentEdge], name: &str) -> Option<&'a Option<String>> {
    spans.iter().find(|(n, _)| n == name).map(|(_, p)| p)
}

// --- Mocks ---

fn chunk(text: &str, final_chunk: bool) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: text.to_string() }],
        }),
        usage_metadata: None,
        finish_reason: final_chunk.then_some(FinishReason::Stop),
        citation_metadata: None,
        partial: !final_chunk,
        turn_complete: final_chunk,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata: None,
        interaction_id: None,
    }
}

/// Streams two chunks with a real suspension between them, and creates a probe
/// span *after* that suspension.
///
/// The probe is created while the agent is polling this stream, so a correctly
/// instrumented agent must have `call_llm` as its ambient parent. Before the fix
/// the guard had already been torn off the thread stack by the first suspension,
/// so the probe attached to the caller instead.
struct SuspendingModel;

#[async_trait]
impl Llm for SuspendingModel {
    fn name(&self) -> &str {
        "suspending-mock"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let s = async_stream::stream! {
            yield Ok(chunk("first", false));

            // Force the generator to return `Pending` at least once. Without a
            // genuine suspension neither bug can be observed.
            tokio::task::yield_now().await;

            let _probe = tracing::info_span!("probe_after_suspension");
            yield Ok(chunk("second", true));
        };
        Ok(Box::pin(s))
    }
}

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str {
        "session-456"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "user-123"
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
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

struct MockContext {
    session: MockSession,
    user_content: Content,
}

impl MockContext {
    fn new() -> Self {
        Self {
            session: MockSession,
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "hello".to_string() }],
            },
        }
    }
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
        "user-123"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-456"
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
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        static RUN_CONFIG: OnceLock<RunConfig> = OnceLock::new();
        RUN_CONFIG.get_or_init(RunConfig::default)
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

// --- Tests ---

/// Both scenarios share one global subscriber, so they run in a single test
/// function on one multi-threaded runtime rather than racing each other.
#[test]
fn spans_keep_their_parents_across_suspension_points() {
    let capture = capture();
    let rt =
        tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();

    // --- Bug 1: a span created after a suspension still nests under `call_llm`.
    rt.block_on(async {
        let agent =
            LlmAgentBuilder::new("test-agent").model(Arc::new(SuspendingModel)).build().unwrap();

        // The caller instruments the whole drain, which is the documented
        // correct usage — so any misparenting below is the library's, not ours.
        async {
            let mut stream = agent.run(Arc::new(MockContext::new())).await.unwrap();
            while stream.next().await.is_some() {}
        }
        .instrument(tracing::info_span!("caller"))
        .await;
    });

    // Collected now, asserted at the end, so that both scenarios always run and
    // a reviewer sees both defects rather than only the first to trip.
    let agent_spans = capture.drain();

    // --- Bug 2: `call_llm` nests under `agent.execute`, not the caller.
    rt.block_on(async {
        let agent =
            LlmAgentBuilder::new("test-agent").model(Arc::new(SuspendingModel)).build().unwrap();

        let sessions: Arc<dyn adk_session::SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());
        sessions
            .create(adk_session::CreateRequest {
                app_name: "test-app".into(),
                user_id: "user-123".into(),
                session_id: Some("session-456".into()),
                state: HashMap::new(),
            })
            .await
            .unwrap();

        let runner = adk_runner::Runner::builder()
            .app_name("test-app")
            .agent(Arc::new(agent))
            .session_service(sessions)
            .build()
            .unwrap();

        async {
            let mut stream = runner
                .run(
                    adk_core::UserId::new("user-123").unwrap(),
                    adk_core::SessionId::new("session-456").unwrap(),
                    Content::new("user").with_text("hello"),
                )
                .await
                .unwrap();
            while stream.next().await.is_some() {}
        }
        .instrument(tracing::info_span!("caller"))
        .await;
    });

    let runner_spans = capture.drain();

    // --- Assertions ---

    let probe = parent_of(&agent_spans, "probe_after_suspension")
        .expect("probe span was never created — the model stream did not run to completion");
    assert_eq!(
        probe.as_deref(),
        Some("call_llm"),
        "a span created after the model stream suspended must still nest under \
         `call_llm`; got {probe:?}. This means the `call_llm` guard was torn off \
         the thread stack at the first suspension point (captured: {agent_spans:?})"
    );

    let call_llm = parent_of(&runner_spans, "call_llm")
        .expect("`call_llm` span was never created — the runner did not reach the model");
    assert_eq!(
        call_llm.as_deref(),
        Some("agent.execute"),
        "`call_llm` must nest under `agent.execute`; got {call_llm:?}. This means \
         the runner instrumented only the construction of the agent stream and \
         not the draining of it (captured: {runner_spans:?})"
    );
}
