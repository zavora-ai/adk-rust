//! A tool call runs with the run's metering scope current, so a media call the tool makes
//! through `adk_core::meter_usage` reserves and commits against the run's spend ledger.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Content, InMemorySpendLedger, Llm, LlmRequest, LlmResponse, LlmResponseStream, MeteredUsage,
    Part, Result, RunConfig, SessionId, SpendKey, SpendLedger, SpendLimits, SpendPeriod, Tool,
    ToolContext, UsageCall, UsageMetadata, UsageUnit, UsageVector, UserId, meter_usage,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Calls `render` on its first turn and answers on every later one, at a known cost.
struct RenderingModel {
    calls: AtomicUsize,
}

#[async_trait]
impl Llm for RenderingModel {
    fn name(&self) -> &str {
        "scripted"
    }

    fn max_cost_micro_usd(&self, _request: &LlmRequest) -> Option<u64> {
        Some(1_000)
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let part = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => Part::FunctionCall {
                name: "render".to_string(),
                args: json!({ "prompt": "a red barn" }),
                id: Some("call-1".to_string()),
                thought_signature: None,
            },
            _ => Part::Text { text: "done".to_string() },
        };
        Ok(Box::pin(futures::stream::iter([Ok(LlmResponse {
            content: Some(Content { role: "model".to_string(), parts: vec![part] }),
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 5,
                total_token_count: 15,
                cost: Some(0.0005),
                ..Default::default()
            }),
            turn_complete: true,
            ..Default::default()
        })])))
    }
}

/// Renders one image billed at $0.01 through the current scope.
struct RenderTool {
    sent: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for RenderTool {
    fn name(&self) -> &str {
        "render"
    }

    fn description(&self) -> &str {
        "Renders an image"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let call = UsageCall::new("muse", "muse-image-1.0").with_max_cost_micro_usd(10_000);
        meter_usage(call, async {
            self.sent.fetch_add(1, Ordering::SeqCst);
            let usage = MeteredUsage::new(UsageVector::new().with(UsageUnit::Images, 1))
                .with_cost_micro_usd(10_000);
            Ok((json!({ "image": "barn.png" }), usage))
        })
        .await
    }
}

async fn run(ledger: Arc<InMemorySpendLedger>, sent: Arc<AtomicUsize>) -> Vec<adk_core::Event> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "studio".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let agent = LlmAgentBuilder::new("designer")
        .model(Arc::new(RenderingModel { calls: AtomicUsize::new(0) }) as Arc<dyn Llm>)
        .tool(Arc::new(RenderTool { sent }) as Arc<dyn Tool>)
        .build()
        .unwrap();
    let runner = Runner::builder()
        .app_name("studio")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(RunConfig::builder().spend_ledger(ledger).build())
        .build()
        .unwrap();
    runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("draw"),
        )
        .await
        .unwrap()
        .map(|event| event.unwrap())
        .collect()
        .await
}

fn tool_response(events: &[adk_core::Event]) -> Value {
    events
        .iter()
        .flat_map(|event| event.llm_response.content.iter().flat_map(|content| &content.parts))
        .find_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .expect("the tool answered")
}

#[tokio::test]
async fn a_media_call_inside_a_tool_is_committed_to_the_run_ledger() {
    let ledger = Arc::new(InMemorySpendLedger::default());
    let sent = Arc::new(AtomicUsize::new(0));
    let events = run(ledger.clone(), sent.clone()).await;
    assert_eq!(tool_response(&events), json!({ "image": "barn.png" }));
    assert_eq!(sent.load(Ordering::SeqCst), 1);

    let agent = SpendKey::org("studio").with_agent("urn:adk:agent:studio/designer");
    let muse = agent.clone().with_vendor("muse");
    assert_eq!(ledger.spent(&muse, SpendPeriod::Day).await.unwrap(), 10_000);
    // Two model turns at 500 micro-USD each, plus the image.
    assert_eq!(ledger.spent(&agent, SpendPeriod::Day).await.unwrap(), 11_000);
}

#[tokio::test]
async fn a_media_call_over_the_run_cap_is_refused_inside_the_tool() {
    let cap = SpendKey::org("studio").with_vendor("muse").per(SpendPeriod::Day);
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(cap, 5_000)));
    let sent = Arc::new(AtomicUsize::new(0));
    let events = run(ledger.clone(), sent.clone()).await;
    assert_eq!(sent.load(Ordering::SeqCst), 0);
    let response = tool_response(&events);
    assert!(response.to_string().contains("spend"), "{response}");
    let muse = SpendKey::org("studio").with_vendor("muse");
    assert_eq!(ledger.spent(&muse, SpendPeriod::Day).await.unwrap(), 0);
}
