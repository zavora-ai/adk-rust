//! Request, response, and tool payloads stay out of spans and debug logs unless
//! `RunConfig::record_payloads` is set.
//!
//! `call_llm` recorded the first 2 KB of the serialized request and response, and
//! tool calls logged their arguments and results at DEBUG, with `record_payloads`
//! off, so short prompts and tool data reached every trace backend in full.
//!
//! Error paths follow the same rule: a provider error that echoes the prompt, a toolset
//! error quoting a credential, and a guardrail reason quoting an argument were logged in
//! full with `record_payloads` off.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{
    AdkError, Content, ErrorCategory, ErrorComponent, FinishReason, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, ReadonlyContext, Result, RunConfig, Tool, Toolset,
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
    let (values, error) = record_run(agent, run_config).await;
    assert!(error.is_none(), "the turn failed: {error:?}");
    values
}

/// Runs `agent` for one turn and returns everything the tracing layer saw, with the
/// error the run ended with, if any.
async fn record_run(agent: LlmAgent, run_config: RunConfig) -> (Vec<String>, Option<AdkError>) {
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
    let mut error = None;
    while let Some(event) = stream.next().await {
        if let Err(failure) = event {
            error = Some(failure);
        }
    }
    drop(_guard);

    (recorded.0.lock().unwrap().clone(), error)
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

/// How a scripted model fails.
#[derive(Clone, Copy)]
enum Failure {
    /// `generate_content` returns the error.
    Call,
    /// The stream yields the error.
    Stream,
    /// The final response reports `error_code` and `error_message`.
    Reported,
}

/// A provider 400 that echoes the prompt, as OpenAI and Gemini do for invalid values.
fn echoed_error() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::InvalidInput,
        "model.test.bad_request",
        format!("Invalid value: '{SECRET}'"),
    )
    .with_upstream_status(400)
    .with_provider("test")
}

struct FailingModel(Failure);

#[async_trait]
impl Llm for FailingModel {
    fn name(&self) -> &str {
        "failing"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        match self.0 {
            Failure::Call => Err(echoed_error()),
            Failure::Stream => Ok(Box::pin(futures::stream::iter([Err(echoed_error())]))),
            Failure::Reported => {
                let response = LlmResponse {
                    error_code: Some("invalid_request_error".to_string()),
                    error_message: Some(format!("'{SECRET}' is not valid. {}", "x".repeat(4096))),
                    turn_complete: true,
                    ..LlmResponse::default()
                };
                Ok(Box::pin(futures::stream::iter([Ok(response)])))
            }
        }
    }
}

fn leaks(values: &[String]) -> Vec<&String> {
    values.iter().filter(|value| value.contains(SECRET)).collect()
}

#[tokio::test]
async fn model_errors_are_logged_without_their_message_by_default() {
    for failure in [Failure::Call, Failure::Stream] {
        let agent = LlmAgentBuilder::new("assistant")
            .model(Arc::new(FailingModel(failure)))
            .build()
            .unwrap();
        let (values, error) = record_run(agent, RunConfig::default()).await;

        assert_eq!(error.map(|error| error.code), Some("model.test.bad_request"));
        assert!(leaks(&values).is_empty(), "the provider message reached tracing: {values:#?}");
        assert!(
            values.contains(
                &"error=model.invalid_input (model.test.bad_request); status 400; provider test"
                    .to_string()
            ),
            "the failure is logged with its diagnostics: {values:#?}"
        );
    }
}

#[tokio::test]
async fn model_error_messages_are_logged_when_payloads_are_recorded() {
    for failure in [Failure::Call, Failure::Stream, Failure::Reported] {
        let agent = LlmAgentBuilder::new("assistant")
            .model(Arc::new(FailingModel(failure)))
            .build()
            .unwrap();
        let (values, _) =
            record_run(agent, RunConfig { record_payloads: true, ..RunConfig::default() }).await;

        assert!(
            values
                .iter()
                .any(|value| value.starts_with("error.message=") && value.contains(SECRET)),
            "the message is recorded when payloads are: {values:#?}"
        );
    }
}

#[tokio::test]
async fn reported_errors_are_bounded_and_logged_without_their_message() {
    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(FailingModel(Failure::Reported)))
        .build()
        .unwrap();
    let (values, error) = record_run(agent, RunConfig::default()).await;

    let error = error.expect("the run fails");
    assert_eq!(error.code, "model.provider_error");
    assert!(
        error.message.len() <= "invalid_request_error: ".len() + adk_core::UPSTREAM_EXCERPT_BYTES,
        "the caller gets a bounded excerpt: {} bytes",
        error.message.len()
    );
    assert!(leaks(&values).is_empty(), "the provider message reached tracing: {values:#?}");
    assert!(
        values.iter().any(|value| value
            == "error=model.internal (model.provider_error); provider code invalid_request_error"),
        "the failure is logged with its diagnostics: {values:#?}"
    );
}

/// A toolset whose server cannot be reached, with a credential in its connection string.
struct UnreachableToolset;

#[async_trait]
impl Toolset for UnreachableToolset {
    fn name(&self) -> &str {
        "crm"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Err(AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Unavailable,
            "tool.crm.unreachable",
            format!("GET https://crm.internal/tools?token={SECRET} failed"),
        ))
    }
}

#[tokio::test]
async fn toolset_errors_are_logged_without_their_message_by_default() {
    for record_payloads in [false, true] {
        let agent = LlmAgentBuilder::new("assistant")
            .model(Arc::new(ScriptedModel { calls: AtomicUsize::new(1) }))
            .toolset(Arc::new(UnreachableToolset))
            .build()
            .unwrap();
        let (values, _) =
            record_run(agent, RunConfig { record_payloads, ..RunConfig::default() }).await;
        let logged = values
            .iter()
            .filter(|value| value.starts_with("error.message=") && value.contains(SECRET))
            .count();

        // The answer itself also holds the secret, so only the error fields are counted.
        assert_eq!(logged > 0, record_payloads, "{values:#?}");
        assert!(
            values.contains(&"error=tool.unavailable (tool.crm.unreachable)".to_string()),
            "the failure is logged with its diagnostics: {values:#?}"
        );
    }
}

#[cfg(feature = "guardrails")]
#[tokio::test]
async fn guardrail_denial_reasons_follow_the_payload_rule() {
    use adk_agent::guardrails::{PathAllowList, ToolGuardrailSet};

    struct WritesSecretPath(AtomicUsize);

    #[async_trait]
    impl Llm for WritesSecretPath {
        fn name(&self) -> &str {
            "writes-secret-path"
        }

        async fn generate_content(
            &self,
            _req: LlmRequest,
            _stream: bool,
        ) -> Result<LlmResponseStream> {
            let part = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Part::FunctionCall {
                    name: "write_note".to_string(),
                    args: serde_json::json!({ "path": format!("/tmp/{SECRET}/note.txt") }),
                    id: Some("call-1".to_string()),
                    thought_signature: None,
                }
            } else {
                Part::Text { text: "Done.".to_string() }
            };
            let mut response =
                LlmResponse::new(Content { role: "model".to_string(), parts: vec![part] });
            response.finish_reason = Some(FinishReason::Stop);
            Ok(Box::pin(futures::stream::iter([Ok(response)])))
        }
    }

    let root = tempfile::tempdir().unwrap();
    for record_payloads in [false, true] {
        let write_note = FunctionTool::new("write_note", "writes a note", |_ctx, _args| async {
            Ok(serde_json::json!({ "written": true }))
        });
        let agent = LlmAgentBuilder::new("assistant")
            .model(Arc::new(WritesSecretPath(AtomicUsize::new(0))))
            .tool(Arc::new(write_note))
            .tool_guardrails(ToolGuardrailSet::new().with(
                PathAllowList::new("notes-only", ["path"], [root.path()]).on_tools(["write_note"]),
            ))
            .build()
            .unwrap();
        let (values, error) =
            record_run(agent, RunConfig { record_payloads, ..RunConfig::default() }).await;

        assert!(error.is_none(), "{error:?}");
        assert!(values.contains(&"guardrail=notes-only".to_string()), "{values:#?}");
        assert_eq!(
            values.iter().any(|value| value.starts_with("reason=") && value.contains(SECRET)),
            record_payloads,
            "the denial reason is logged only when payloads are recorded: {values:#?}"
        );
        if !record_payloads {
            assert!(leaks(&values).is_empty(), "the denied path reached tracing: {values:#?}");
        }
    }
}

#[cfg(feature = "guardrails")]
#[tokio::test]
async fn tool_result_withhold_reasons_follow_the_payload_rule() {
    use adk_agent::guardrails::{
        Severity, ToolGuardrail, ToolGuardrailSet, ToolResultGuardrailResult,
    };

    struct CallsLookup(AtomicUsize);

    #[async_trait]
    impl Llm for CallsLookup {
        fn name(&self) -> &str {
            "calls-lookup"
        }

        async fn generate_content(
            &self,
            _req: LlmRequest,
            _stream: bool,
        ) -> Result<LlmResponseStream> {
            let part = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Part::FunctionCall {
                    name: "lookup".to_string(),
                    args: serde_json::json!({}),
                    id: Some("call-1".to_string()),
                    thought_signature: None,
                }
            } else {
                Part::Text { text: "Done.".to_string() }
            };
            let mut response =
                LlmResponse::new(Content { role: "model".to_string(), parts: vec![part] });
            response.finish_reason = Some(FinishReason::Stop);
            Ok(Box::pin(futures::stream::iter([Ok(response)])))
        }
    }

    /// Withholds every result, with a reason that quotes it.
    struct WithholdsAccounts;

    #[async_trait]
    impl ToolGuardrail for WithholdsAccounts {
        fn name(&self) -> &str {
            "withholds-accounts"
        }

        async fn validate_result(
            &self,
            _tool: &str,
            _args: &serde_json::Value,
            result: &serde_json::Value,
        ) -> ToolResultGuardrailResult {
            ToolResultGuardrailResult::withhold(
                format!("the result names {}", result["account"]),
                Severity::High,
            )
        }
    }

    for record_payloads in [false, true] {
        let lookup = FunctionTool::new("lookup", "looks up an account", |_ctx, _args| async {
            Ok(serde_json::json!({ "account": SECRET }))
        });
        let agent = LlmAgentBuilder::new("assistant")
            .model(Arc::new(CallsLookup(AtomicUsize::new(0))))
            .tool(Arc::new(lookup))
            .tool_guardrails(ToolGuardrailSet::new().with(WithholdsAccounts))
            .build()
            .unwrap();
        let (values, error) =
            record_run(agent, RunConfig { record_payloads, ..RunConfig::default() }).await;

        assert!(error.is_none(), "{error:?}");
        assert!(values.contains(&"guardrail=withholds-accounts".to_string()), "{values:#?}");
        assert_eq!(
            values.iter().any(|value| value.starts_with("reason=") && value.contains(SECRET)),
            record_payloads,
            "the withhold reason is logged only when payloads are recorded: {values:#?}"
        );
        if !record_payloads {
            assert!(leaks(&values).is_empty(), "the withheld result reached tracing: {values:#?}");
        }
    }
}
