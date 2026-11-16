//! Auto-compaction inside a run: an `LlmAgent` driven through a `Runner` by a scripted model
//! with a tiny context window compacts its history between tool calls, records each
//! compaction in the session, and compacts and resends a request the model rejects as too long.

use adk_agent::LlmAgentBuilder;
use adk_core::intra_compaction::{COMPACTION_SUMMARY_HEADING, estimate_content_tokens};
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, Event, FinishReason, IntraCompactionConfig,
    Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result, RunConfig, SessionId,
    ToolContext, UserId,
};
use adk_runner::Runner;
use adk_runner::compaction::{CompactionConfig, CompactionStrategy, TruncationCompaction};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP: &str = "compaction";

/// Calls `lookup` until it has made `steps` calls, then answers. Summary requests get a short
/// summary. A request past `hard_limit` estimated tokens is rejected inside the stream, as
/// providers reject a prompt longer than the context window.
struct Scripted {
    window: Option<u64>,
    hard_limit: Option<u64>,
    steps: usize,
    /// After this many lookups, the next answer calls `transfer` once.
    hold_after: Option<usize>,
    held: std::sync::atomic::AtomicBool,
    lookups: AtomicUsize,
    requests: Mutex<Vec<Vec<Content>>>,
    summaries: AtomicUsize,
    rejections: AtomicUsize,
}

impl Scripted {
    fn new(window: Option<u64>, steps: usize) -> Arc<Self> {
        Self::with_limit(window, None, steps)
    }

    fn with_limit(window: Option<u64>, hard_limit: Option<u64>, steps: usize) -> Arc<Self> {
        Arc::new(Self {
            window,
            hard_limit,
            steps,
            hold_after: None,
            held: std::sync::atomic::AtomicBool::new(false),
            lookups: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            summaries: AtomicUsize::new(0),
            rejections: AtomicUsize::new(0),
        })
    }

    fn requests(&self) -> Vec<Vec<Content>> {
        self.requests.lock().unwrap().clone()
    }

    fn sizes(&self) -> Vec<u64> {
        self.requests().iter().map(|contents| estimate_content_tokens(contents, 4)).collect()
    }
}

fn respond(content: Content) -> Result<LlmResponseStream> {
    let response = LlmResponse {
        content: Some(content),
        turn_complete: true,
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    };
    Ok(Box::pin(futures::stream::iter([Ok(response)])))
}

#[async_trait]
impl Llm for Scripted {
    fn name(&self) -> &str {
        "scripted"
    }

    fn context_window(&self) -> Option<u64> {
        self.window
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let summary_request = req.contents.len() == 1
            && req.contents[0].parts.iter().any(|part| {
                part.text().is_some_and(|text| text.contains("Please summarize the conversation"))
            });
        if summary_request {
            let count = self.summaries.fetch_add(1, Ordering::SeqCst) + 1;
            return respond(Content::new("model").with_text(format!("summary {count}")));
        }
        let tokens = estimate_content_tokens(&req.contents, 4);
        if let Some(limit) = self.hard_limit
            && tokens > limit
        {
            self.rejections.fetch_add(1, Ordering::SeqCst);
            let error = AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.scripted.api_error",
                format!("400: prompt is too long: {tokens} tokens > {limit} maximum"),
            );
            return Ok(Box::pin(futures::stream::iter([Err(error)])));
        }
        self.requests.lock().unwrap().push(req.contents);
        // A provider takes time to answer, so its turn is recorded after the tool result
        // before it, as a session store can tell apart.
        tokio::time::sleep(Duration::from_millis(2)).await;
        let made = self.lookups.load(Ordering::SeqCst);
        if self.hold_after == Some(made) && !self.held.swap(true, Ordering::SeqCst) {
            return respond(Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "transfer".to_string(),
                    args: json!({ "amount": 10 }),
                    id: Some("call-hold".to_string()),
                    thought_signature: None,
                }],
            });
        }
        if made >= self.steps {
            return respond(Content::new("model").with_text(format!("done after {made} lookups")));
        }
        let call = self.lookups.fetch_add(1, Ordering::SeqCst) + 1;
        respond(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "lookup".to_string(),
                args: json!({ "page": call }),
                id: Some(format!("call-{call}")),
                thought_signature: None,
            }],
        })
    }
}

/// A `lookup` tool whose every result is about 250 estimated tokens.
fn lookup() -> Arc<FunctionTool> {
    Arc::new(FunctionTool::new(
        "lookup",
        "reads one page of the record",
        |_ctx: Arc<dyn ToolContext>, args: Value| async move {
            Ok(json!({ "page": args["page"], "text": "record line ".repeat(80) }))
        },
    ))
}

/// How a test configures the runner's compaction.
#[derive(Default)]
struct Setup {
    intra: Option<IntraCompactionConfig>,
    summarizer: Option<Arc<dyn adk_core::BaseEventsSummarizer>>,
    strategy: Option<CompactionConfig>,
}

impl Setup {
    fn intra(config: IntraCompactionConfig) -> Self {
        Self { intra: Some(config), ..Self::default() }
    }

    fn off() -> Self {
        Self::intra(IntraCompactionConfig::disabled())
    }
}

async fn runner(model: Arc<Scripted>, setup: Setup) -> (Runner, Arc<InMemorySessionService>) {
    let transfer = FunctionTool::new(
        "transfer",
        "moves money",
        |_ctx: Arc<dyn ToolContext>, _args: Value| async move { Ok(json!({ "ok": true })) },
    );
    let agent = LlmAgentBuilder::new("reader")
        .instruction("Read every page, then answer.")
        .model(model as Arc<dyn Llm>)
        .tool(lookup())
        .tool(Arc::new(transfer))
        .require_tool_confirmation("transfer")
        .max_iterations(200)
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: "u".to_string(),
            session_id: Some("s".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let mut builder = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>);
    if let Some(config) = setup.intra {
        builder = builder.intra_compaction_config(config);
    }
    if let Some(summarizer) = setup.summarizer {
        builder = builder.intra_compaction_summarizer(summarizer);
    }
    if let Some(strategy) = setup.strategy {
        builder = builder.context_compaction(strategy);
    }
    (builder.build().unwrap(), sessions)
}

async fn run(runner: &Runner, text: &str, config: Option<RunConfig>) -> Vec<Result<Event>> {
    runner
        .run_with_config(
            UserId::try_from("u").unwrap(),
            SessionId::try_from("s").unwrap(),
            Content::new("user").with_text(text),
            config,
        )
        .await
        .unwrap()
        .collect()
        .await
}

fn compactions(events: &[Result<Event>]) -> usize {
    events
        .iter()
        .filter(|event| event.as_ref().is_ok_and(|event| event.actions.compaction.is_some()))
        .count()
}

fn final_text(events: &[Result<Event>]) -> Option<String> {
    events.iter().rev().filter_map(|event| event.as_ref().ok()).find_map(|event| {
        let content = event.content()?;
        let text: String = content.parts.iter().filter_map(Part::text).collect();
        text.starts_with("done").then_some(text)
    })
}

fn errors(events: &[Result<Event>]) -> Vec<String> {
    events.iter().filter_map(|event| event.as_ref().err()).map(ToString::to_string).collect()
}

/// Every function response in a request answers a call made earlier in it.
fn balanced(contents: &[Content]) -> bool {
    let mut called = HashSet::new();
    contents.iter().flat_map(|content| &content.parts).all(|part| match part {
        Part::FunctionCall { id, .. } => {
            called.insert(id.clone());
            true
        }
        Part::FunctionResponse { id, .. } => called.contains(id),
        _ => true,
    })
}

fn is_summary(content: &Content) -> bool {
    content
        .parts
        .iter()
        .any(|part| part.text().is_some_and(|t| t.contains(COMPACTION_SUMMARY_HEADING)))
}

#[tokio::test]
async fn a_long_tool_loop_compacts_between_model_calls_and_completes() {
    // A 2,000-token window: compaction at 1,600 estimated tokens, about six lookups in.
    let model = Scripted::new(Some(2_000), 20);
    let (runner, sessions) = runner(model.clone(), Setup::default()).await;

    let events = run(&runner, "read the record", None).await;

    assert_eq!(errors(&events), Vec::<String>::new());
    assert_eq!(final_text(&events).as_deref(), Some("done after 20 lookups"));
    let compacted = compactions(&events);
    assert!(compacted >= 2, "compacted {compacted} times within one run");
    assert_eq!(model.summaries.load(Ordering::SeqCst), compacted);
    // No request reaches the window, and every request keeps each response with its call.
    let sizes = model.sizes();
    assert!(sizes.iter().all(|size| *size < 2_000), "request sizes {sizes:?}");
    assert!(model.requests().iter().all(|contents| balanced(contents)));
    // The latest request starts from a summary, after the instructions.
    let last = model.requests().pop().unwrap();
    assert!(is_summary(&last[1]), "{last:?}");

    // The session records each compaction, so the next run starts from the latest summary.
    let session = sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: "u".to_string(),
            session_id: "s".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    let recorded = session.events().all().iter().filter(|e| e.actions.compaction.is_some()).count();
    assert_eq!(recorded, compacted);
    let before = model.requests().len();
    let events = run(&runner, "and the summary?", None).await;
    assert_eq!(errors(&events), Vec::<String>::new());
    let next = &model.requests()[before];
    assert!(is_summary(&next[1]), "the next run starts from the summary: {next:?}");
    assert!(estimate_content_tokens(next, 4) < 1_600);
}

#[tokio::test]
async fn without_auto_compaction_the_history_grows_every_call() {
    let model = Scripted::new(Some(2_000), 12);
    let (runner, _) = runner(model.clone(), Setup::off()).await;

    let events = run(&runner, "read the record", None).await;

    assert_eq!(compactions(&events), 0);
    assert_eq!(model.summaries.load(Ordering::SeqCst), 0);
    let sizes = model.sizes();
    assert!(sizes.windows(2).all(|pair| pair[1] > pair[0]), "sizes {sizes:?}");
    assert!(*sizes.last().unwrap() > 2_000);
}

#[tokio::test]
async fn a_run_config_turns_compaction_off_for_one_run() {
    let model = Scripted::new(Some(2_000), 12);
    let (runner, _) = runner(model.clone(), Setup::default()).await;
    let config = RunConfig::builder().intra_compaction(IntraCompactionConfig::disabled()).build();

    let events = run(&runner, "read the record", Some(config)).await;

    assert_eq!(compactions(&events), 0);
    assert_eq!(model.summaries.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_fixed_threshold_takes_precedence_over_the_window() {
    // The window would allow 80,000 tokens; the fixed threshold compacts at 1,000.
    let model = Scripted::new(Some(100_000), 12);
    let (runner, _) =
        runner(model.clone(), Setup::intra(IntraCompactionConfig::at_tokens(1_000))).await;

    let events = run(&runner, "read the record", None).await;

    assert!(compactions(&events) >= 1);
    assert!(model.sizes().iter().all(|size| *size < 1_400), "sizes {:?}", model.sizes());
}

/// Counts its calls and keeps the first entry and the four most recent.
struct CountingTruncation(Arc<AtomicUsize>);

#[async_trait]
impl CompactionStrategy for CountingTruncation {
    async fn compact(
        &self,
        events: Vec<Event>,
        budget: usize,
    ) -> std::result::Result<Vec<Event>, AdkError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        TruncationCompaction { preserve_recent: 4 }.compact(events, budget).await
    }
}

#[tokio::test]
async fn a_compaction_strategy_takes_the_place_of_the_summary() {
    let model = Scripted::new(Some(2_000), 12);
    let calls = Arc::new(AtomicUsize::new(0));
    let strategy = CountingTruncation(calls.clone());
    let setup = Setup {
        strategy: Some(CompactionConfig::new(Box::new(strategy), 1_200)),
        ..Setup::default()
    };
    let (runner, _) = runner(model.clone(), setup).await;

    let events = run(&runner, "read the record", None).await;

    assert_eq!(errors(&events), Vec::<String>::new());
    assert!(calls.load(Ordering::SeqCst) >= 1, "the strategy ran");
    assert_eq!(model.summaries.load(Ordering::SeqCst), 0, "no summary was asked for");
    assert!(model.sizes().iter().all(|size| *size < 1_600), "sizes {:?}", model.sizes());
    assert!(model.requests().iter().all(|contents| balanced(contents)));
}

/// Summarizes with a fixed text, counting its calls.
struct FixedSummary(Arc<AtomicUsize>);

#[async_trait]
impl adk_core::BaseEventsSummarizer for FixedSummary {
    async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let mut event = Event::new("summary");
        event.set_content(Content::new("model").with_text(format!("{} entries", events.len())));
        Ok(Some(event))
    }
}

#[tokio::test]
async fn a_configured_summarizer_writes_the_summary() {
    let model = Scripted::new(Some(2_000), 12);
    let calls = Arc::new(AtomicUsize::new(0));
    let summarizer = Arc::new(FixedSummary(calls.clone()));
    let setup = Setup { summarizer: Some(summarizer), ..Setup::default() };
    let (runner, _) = runner(model.clone(), setup).await;

    let events = run(&runner, "read the record", None).await;

    assert!(compactions(&events) >= 1);
    assert_eq!(calls.load(Ordering::SeqCst), compactions(&events));
    assert_eq!(model.summaries.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_request_rejected_as_too_long_is_compacted_and_sent_once_more() {
    // The threshold is far above the model's real limit, so only the rejection compacts.
    let model = Scripted::with_limit(None, Some(1_500), 12);
    let (runner, _) =
        runner(model.clone(), Setup::intra(IntraCompactionConfig::at_tokens(1_000_000))).await;

    let events = run(&runner, "read the record", None).await;

    assert_eq!(errors(&events), Vec::<String>::new());
    assert_eq!(final_text(&events).as_deref(), Some("done after 12 lookups"));
    let rejections = model.rejections.load(Ordering::SeqCst);
    assert!(rejections >= 1);
    // Each rejection is answered by exactly one compaction and one resend.
    assert_eq!(compactions(&events), rejections);
    assert_eq!(model.summaries.load(Ordering::SeqCst), rejections);
}

#[tokio::test]
async fn a_request_rejected_again_after_compaction_fails_the_run() {
    // Every request is too long, the instructions alone included.
    let model = Scripted::with_limit(None, Some(1), 12);
    let (runner, _) = runner(model.clone(), Setup::default()).await;

    let events = run(&runner, "read the record", None).await;

    let errors = errors(&events);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("prompt is too long"), "{errors:?}");
    // The first rejection is compacted for (nothing to compact) and fails at once.
    assert_eq!(model.rejections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn without_a_compactor_a_rejected_request_fails_the_run() {
    let model = Scripted::with_limit(None, Some(1_500), 12);
    let (runner, _) = runner(model.clone(), Setup::off()).await;

    let events = run(&runner, "read the record", None).await;

    let errors = errors(&events);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(model.rejections.load(Ordering::SeqCst), 1);
    assert_eq!(model.summaries.load(Ordering::SeqCst), 0);
}

fn holds_the_call(contents: &[Content]) -> bool {
    let parts = || contents.iter().flat_map(|content| &content.parts);
    parts().any(|part| {
        matches!(part, Part::FunctionCall { id: Some(id), .. } if id == "call-hold")
    }) && parts().any(|part| {
        matches!(
            part,
            Part::FunctionResponse { function_response, id: Some(id), .. }
                if id == "call-hold" && adk_core::is_confirmation_pending(&function_response.response)
        )
    })
}

#[tokio::test]
async fn a_call_held_for_confirmation_stays_in_every_compacted_request() {
    let model = Arc::new(Scripted {
        hold_after: Some(6),
        ..Arc::into_inner(Scripted::new(Some(2_000), 10)).unwrap()
    });
    let (runner, _) = runner(model.clone(), Setup::default()).await;

    // The run ends with `transfer` held for a decision.
    let first = run(&runner, "read the record", None).await;
    assert_eq!(errors(&first), Vec::<String>::new());
    assert!(model.held.load(Ordering::SeqCst));
    let held_at = model.requests().len();

    // The next run, with no decision, compacts what came before the held call only.
    let second = run(&runner, "carry on", None).await;
    assert_eq!(errors(&second), Vec::<String>::new());
    assert!(compactions(&second) >= 1, "the second run compacted");
    let requests = model.requests();
    assert!(requests[held_at..].iter().all(|contents| holds_the_call(contents)));
    assert!(requests[held_at..].iter().all(|contents| balanced(contents)));
}
