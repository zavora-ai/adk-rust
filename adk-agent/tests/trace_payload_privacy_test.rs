//! Request, response, and tool payloads stay out of spans and debug logs unless
//! `RunConfig::record_payloads` is set.
//!
//! `call_llm` recorded the first 2 KB of the serialized request and response, and
//! tool calls logged their arguments and results at DEBUG, with `record_payloads`
//! off, so short prompts and tool data reached every trace backend in full.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result, RunConfig,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use futures::StreamExt;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

const SECRET: &str = "acct-7731-secret";

/// Every span field and event field value seen, as text.
#[derive(Clone, Default)]
struct Recorded(Arc<Mutex<Vec<String>>>);

struct Values<'a>(&'a mut Vec<String>);

impl Visit for Values<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{}={value:?}", field.name()));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push(format!("{}={value}", field.name()));
    }
}

impl<S> Layer<S> for Recorded
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        attrs.record(&mut Values(&mut self.0.lock().unwrap()));
    }

    fn on_record(&self, _id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        values.record(&mut Values(&mut self.0.lock().unwrap()));
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        event.record(&mut Values(&mut self.0.lock().unwrap()));
    }
}

/// Calls `lookup` with the secret, then answers with it.
struct ScriptedModel {
    calls: AtomicUsize,
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let part = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Part::FunctionCall {
                name: "lookup".to_string(),
                args: serde_json::json!({ "account": SECRET }),
                id: Some("call-1".to_string()),
                thought_signature: None,
            }
        } else {
            Part::Text { text: format!("Your account is {SECRET}.") }
        };
        let mut response =
            LlmResponse::new(Content { role: "model".to_string(), parts: vec![part] });
        response.finish_reason = Some(FinishReason::Stop);
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// Runs one turn whose prompt, tool arguments, tool result, and answer all hold the
/// secret, and returns everything the tracing layer saw.
async fn run_turn(run_config: RunConfig) -> Vec<String> {
    let lookup = FunctionTool::new("lookup", "looks up an account", |_ctx, args| async move {
        Ok(serde_json::json!({ "owner": "ada", "echo": args["account"] }))
    });
    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(ScriptedModel { calls: AtomicUsize::new(0) }))
        .tool(Arc::new(lookup))
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: "app".to_string(),
            user_id: "user".to_string(),
            session_id: None,
            state: Default::default(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("app")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .run_config(run_config)
        .build()
        .unwrap();

    let recorded = Recorded::default();
    let subscriber = tracing_subscriber::registry().with(recorded.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut stream = runner
        .run_str("user", session.id(), Content::new("user").with_text(format!("Is {SECRET} mine?")))
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    drop(_guard);

    recorded.0.lock().unwrap().clone()
}

#[tokio::test]
async fn payloads_are_omitted_by_default() {
    let values = run_turn(RunConfig::default()).await;

    let leaks: Vec<&String> = values.iter().filter(|value| value.contains(SECRET)).collect();
    assert!(leaks.is_empty(), "payload content reached tracing: {leaks:#?}");
    assert!(
        values.iter().any(|value| value.starts_with("gcp.vertex.agent.llm_request=[omitted")),
        "the call_llm span must say the request was omitted: {values:#?}"
    );
}

#[tokio::test]
async fn payloads_are_recorded_when_enabled() {
    let values = run_turn(RunConfig { record_payloads: true, ..RunConfig::default() }).await;

    for field in [
        "gcp.vertex.agent.llm_request=",
        "gcp.vertex.agent.llm_response=",
        "tool.args=",
        "tool.result=",
    ] {
        assert!(
            values.iter().any(|value| value.starts_with(field) && value.contains(SECRET)),
            "{field} must carry the payload when recording is enabled: {values:#?}"
        );
    }
}
