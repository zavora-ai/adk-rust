//! The OpenTelemetry GenAI spans of a `Runner` and `LlmAgent` run with a tool call, as an
//! exporter receives them: `invoke_agent`, the provider's inference span, and `execute_tool`.

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Llm, SessionId, ToolContext, UserId};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use opentelemetry::Value;
use opentelemetry::trace::{SpanKind, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;

#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct CityArgs {
    /// The city to report on.
    city: String,
}

fn attr(span: &SpanData, key: &str) -> Option<String> {
    span.attributes.iter().find(|kv| kv.key.as_str() == key).map(|kv| match &kv.value {
        Value::String(text) => text.to_string(),
        other => other.to_string(),
    })
}

async fn run(model: Arc<dyn Llm>) -> Vec<SpanData> {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder().with_simple_exporter(exporter.clone()).build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("adk-live")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let weather = FunctionTool::new(
        "get_weather",
        "Current weather for a city.",
        |_ctx: Arc<dyn ToolContext>, args: Json| async move {
            Ok(json!({ "city": args["city"], "conditions": "sunny", "celsius": 24 }))
        },
    )
    .with_parameters_schema::<CityArgs>();
    let agent = LlmAgentBuilder::new("weather_agent")
        .description("Answers weather questions with the get_weather tool.")
        .instruction("Use get_weather for any weather question, then answer in one sentence.")
        .model(model)
        .tool(Arc::new(weather))
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "genai".to_string(),
            user_id: "u".to_string(),
            session_id: Some("s".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("genai")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let events: Vec<_> = runner
        .run_with_config(
            UserId::try_from("u").unwrap(),
            SessionId::try_from("s").unwrap(),
            Content::new("user").with_text("What is the weather in Nairobi?"),
            None,
        )
        .await
        .unwrap()
        .collect()
        .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    provider.force_flush().unwrap();
    exporter.get_finished_spans().unwrap()
}

fn print(spans: &[SpanData]) {
    for span in spans {
        let mut attributes: Vec<String> = span
            .attributes
            .iter()
            .filter(|kv| kv.key.as_str().starts_with("gen_ai.") || kv.key.as_str() == "error.type")
            .map(|kv| format!("{}={}", kv.key, kv.value))
            .collect();
        attributes.sort();
        println!("{} [{:?}]\n  {}", span.name, span.span_kind, attributes.join("\n  "));
    }
}

fn check(spans: &[SpanData], model: &str, provider: &str) {
    let agent = spans.iter().find(|s| s.name == "invoke_agent weather_agent").expect("agent span");
    assert_eq!(agent.span_kind, SpanKind::Internal);
    assert_eq!(attr(agent, "gen_ai.operation.name").as_deref(), Some("invoke_agent"));
    assert_eq!(attr(agent, "gen_ai.agent.name").as_deref(), Some("weather_agent"));
    assert_eq!(attr(agent, "gen_ai.conversation.id").as_deref(), Some("s"));
    assert!(attr(agent, "gen_ai.agent.description").is_some());

    let inference: Vec<_> =
        spans.iter().filter(|s| s.name == format!("chat {model}").as_str()).collect();
    assert!(inference.len() >= 2, "a call that asks for the tool and one that answers");
    for span in &inference {
        assert_eq!(span.span_kind, SpanKind::Client);
        assert_eq!(attr(span, "gen_ai.operation.name").as_deref(), Some("chat"));
        assert_eq!(attr(span, "gen_ai.provider.name").as_deref(), Some(provider));
        assert_eq!(attr(span, "gen_ai.request.model").as_deref(), Some(model));
        assert!(attr(span, "gen_ai.usage.input_tokens").is_some(), "{:?}", span.attributes);
        assert!(attr(span, "gen_ai.usage.output_tokens").is_some());
        assert!(attr(span, "gen_ai.response.finish_reasons").is_some());
        assert!(attr(span, "gen_ai.system").is_none());
    }
    assert!(
        inference.iter().any(|span| {
            attr(span, "gen_ai.response.finish_reasons").is_some_and(|r| r.contains("tool_call"))
        }),
        "the first call stops on the tool call"
    );

    let tool = spans.iter().find(|s| s.name == "execute_tool get_weather").expect("tool span");
    assert_eq!(tool.span_kind, SpanKind::Internal);
    assert_eq!(attr(tool, "gen_ai.operation.name").as_deref(), Some("execute_tool"));
    assert_eq!(attr(tool, "gen_ai.tool.name").as_deref(), Some("get_weather"));
    assert_eq!(attr(tool, "gen_ai.tool.type").as_deref(), Some("function"));
    assert_eq!(attr(tool, "gen_ai.agent.name").as_deref(), Some("weather_agent"));
    assert!(attr(tool, "gen_ai.tool.call.id").is_some(), "the provider gave the call an id");
    assert!(attr(tool, "gen_ai.tool.call.arguments").is_none(), "content capture is off");
    let trace = agent.span_context.trace_id();
    assert!(
        inference.iter().chain([&tool]).all(|span| span.span_context.trace_id() == trace),
        "one trace"
    );
}

/// A scripted Anthropic-shaped run: no network, the same spans.
#[tokio::test(flavor = "multi_thread")]
async fn a_scripted_run_exports_invoke_agent_inference_and_execute_tool_spans() {
    use adk_model::{ScriptedLlm, ScriptedResponse};
    // The scripted model opens no provider span, so this checks the agent and tool spans.
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("get_weather", json!({ "city": "Nairobi" })),
            ScriptedResponse::text("It is sunny and 24 C in Nairobi."),
        ],
    ));
    let spans = run(model).await;
    let agent = spans.iter().find(|s| s.name == "invoke_agent weather_agent").expect("agent span");
    assert_eq!(attr(agent, "gen_ai.operation.name").as_deref(), Some("invoke_agent"));
    let tool = spans.iter().find(|s| s.name == "execute_tool get_weather").expect("tool span");
    assert_eq!(attr(tool, "gen_ai.tool.type").as_deref(), Some("function"));
    assert_eq!(tool.span_context.trace_id(), agent.span_context.trace_id());
}

/// The same run on Anthropic `claude-sonnet-5-5`.
///
/// ```text
/// cargo nextest run -p adk-agent --run-ignored only -E 'test(live_genai_spans)' --no-capture
/// ```
#[tokio::test(flavor = "multi_thread")]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn live_genai_spans_on_anthropic() {
    let env = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    let _ = dotenvy::from_path_override(&env);
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let model = adk_model::anthropic::AnthropicClient::new(
        adk_model::anthropic::AnthropicConfig::new(key, "claude-sonnet-5-5").with_max_tokens(1_024),
    )
    .unwrap();
    let spans = run(Arc::new(model)).await;
    print(&spans);
    check(&spans, "claude-sonnet-5-5", "anthropic");
}
