//! Provider-executed built-in tools are governed before the request is sent and
//! metered after the response arrives, for Gemini, OpenAI Responses and Anthropic.
//!
//! Each test drives an `LlmAgent` through the `Runner` against a mock provider
//! endpoint and inspects the request body the provider received.

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_core::{
    AdkError, Agent, ApprovalScope, ApprovalStore, BUILTIN_TOOLS_WITHHELD_KEY,
    BuiltinToolWithholding, Content, DeclarativePolicy, ErrorCategory, Event, GovernanceControl,
    InMemoryApprovalStore, Llm, Result, RunBudget, RunConfig, Tool, ToolApproval, ToolContext,
    UsageMetadata, WithheldBuiltinTool,
};
use adk_model::pricing::PricingCatalog;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug, Clone, Copy)]
enum Provider {
    Gemini,
    OpenAI,
    Anthropic,
}

const PROVIDERS: [Provider; 3] = [Provider::Gemini, Provider::OpenAI, Provider::Anthropic];

impl Provider {
    fn model_id(self) -> &'static str {
        match self {
            Self::Gemini => "gemini-3.7-flash",
            Self::OpenAI => "gpt-5.6-luna",
            Self::Anthropic => "claude-sonnet-4-6",
        }
    }

    fn builtin(self) -> Arc<dyn Tool> {
        match self {
            Self::Gemini => Arc::new(adk_tool::GoogleSearchTool::new()),
            Self::OpenAI => Arc::new(adk_tool::OpenAIWebSearchTool::new()),
            Self::Anthropic => Arc::new(adk_tool::WebSearchTool::new().with_max_uses(3)),
        }
    }

    fn tool_name(self) -> &'static str {
        match self {
            Self::Gemini => "google_search",
            Self::OpenAI => "openai_web_search",
            Self::Anthropic => "web_search",
        }
    }

    /// The tool kind the provider reports, and the fee for two uses.
    fn reported(self) -> (&'static str, f64) {
        match self {
            Self::Gemini => ("google_search", 2.0 * 0.014),
            Self::OpenAI | Self::Anthropic => ("web_search", 2.0 * 0.010),
        }
    }

    /// Whether a received request body declares the built-in tool.
    fn declares_builtin(self, body: &Value) -> bool {
        let tools = body["tools"].as_array().cloned().unwrap_or_default();
        tools.iter().any(|tool| match self {
            Self::Gemini => tool.get("google_search").is_some(),
            Self::OpenAI => tool["type"] == "web_search",
            Self::Anthropic => tool["type"] == "web_search_20250305",
        })
    }

    /// A streamed answer reporting `searches` provider-run searches.
    fn sse_answer(self, searches: u32) -> String {
        match self {
            Self::Gemini => {
                let queries: Vec<String> = (0..searches).map(|i| format!("rust {i}")).collect();
                let chunk = json!({
                    "candidates": [{
                        "content": {"role": "model", "parts": [{"text": "Rust 1.95"}]},
                        "finishReason": "STOP",
                        "groundingMetadata": {"webSearchQueries": queries}
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 100, "candidatesTokenCount": 10, "totalTokenCount": 110
                    },
                    "modelVersion": "gemini-3.7-flash"
                });
                format!("data: {chunk}\n\n")
            }
            Self::OpenAI => {
                let mut output: Vec<Value> = (0..searches)
                    .map(|i| {
                        json!({"type": "web_search_call", "id": format!("ws_{i}"), "status": "completed",
                            "action": {"type": "search", "query": "rust"}})
                    })
                    .collect();
                output.push(json!({
                    "type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": "Rust 1.95", "annotations": []}]
                }));
                let event = json!({
                    "type": "response.completed", "sequence_number": 1,
                    "response": {
                        "id": "resp_1", "object": "response", "created_at": 0,
                        "model": "gpt-5.6-luna", "status": "completed", "output": output,
                        "usage": {
                            "input_tokens": 100, "input_tokens_details": {"cached_tokens": 0},
                            "output_tokens": 10, "output_tokens_details": {"reasoning_tokens": 0},
                            "total_tokens": 110
                        }
                    }
                });
                format!("data: {event}\n\n")
            }
            Self::Anthropic => {
                let start = json!({"type": "message_start", "message": {
                    "id": "msg_1", "type": "message", "role": "assistant", "content": [],
                    "model": "claude-sonnet-4-6", "stop_reason": null, "stop_sequence": null,
                    "usage": {"input_tokens": 100, "output_tokens": 1}}});
                let block = json!({"type": "content_block_start", "index": 0,
                    "content_block": {"type": "text", "text": "Rust 1.95"}});
                let delta = json!({"type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                    "usage": {"output_tokens": 10, "server_tool_use": {"web_search_requests": searches}}});
                format!(
                    "event: message_start\ndata: {start}\n\n\
                     event: content_block_start\ndata: {block}\n\n\
                     event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
                     event: message_delta\ndata: {delta}\n\n\
                     event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
                )
            }
        }
    }

    async fn model(self, searches: u32) -> (Arc<dyn Llm>, MockServer) {
        let server = MockServer::start().await;
        let sse =
            ResponseTemplate::new(200).set_body_raw(self.sse_answer(searches), "text/event-stream");
        if let Self::OpenAI = self {
            // The Responses client sends requests with hosted tools unstreamed.
            let event: Value =
                serde_json::from_str(self.sse_answer(searches).trim_start_matches("data: ").trim())
                    .unwrap();
            Mock::given(method("POST"))
                .and(body_partial_json(json!({ "stream": true })))
                .respond_with(sse)
                .with_priority(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(&event["response"]))
                .mount(&server)
                .await;
        } else {
            Mock::given(method("POST")).respond_with(sse).mount(&server).await;
        }
        let model: Arc<dyn Llm> = match self {
            Self::Gemini => Arc::new(
                adk_model::GeminiModel::new_with_base_url(
                    "test-key",
                    self.model_id(),
                    format!("{}/v1beta/", server.uri()),
                )
                .unwrap(),
            ),
            Self::OpenAI => Arc::new(
                adk_model::openai::OpenAIResponsesClient::new(
                    adk_model::openai::OpenAIResponsesConfig::new("test-key", self.model_id())
                        .with_base_url(server.uri()),
                )
                .unwrap(),
            ),
            Self::Anthropic => Arc::new(
                adk_model::anthropic::AnthropicClient::new(
                    adk_model::anthropic::AnthropicConfig::new("test-key", self.model_id())
                        .with_base_url(server.uri()),
                )
                .unwrap(),
            ),
        };
        (model, server)
    }

    fn agent(self, name: &str, model: Arc<dyn Llm>) -> Arc<dyn Agent> {
        Arc::new(
            LlmAgentBuilder::new(name)
                .model(model)
                .tool(self.builtin())
                .tool(Arc::new(Lookup))
                .build()
                .unwrap(),
        )
    }
}

/// A local function tool that stays attached whatever happens to built-ins.
struct Lookup;

#[async_trait]
impl Tool for Lookup {
    fn name(&self) -> &str {
        "lookup"
    }
    fn description(&self) -> &str {
        "Looks up a record"
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(json!({ "ok": true }))
    }
}

async fn run(
    agent: Arc<dyn Agent>,
    config: RunConfig,
) -> Vec<std::result::Result<Event, AdkError>> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "app".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("app")
        .agent(agent)
        .session_service(sessions)
        .run_config(config)
        .build()
        .unwrap();
    runner
        .run_with_config(
            "user".try_into().unwrap(),
            "session".try_into().unwrap(),
            Content::new("user").with_text("What is the latest Rust release?"),
            None,
        )
        .await
        .unwrap()
        .collect()
        .await
}

async fn received(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

fn withheld(results: &[std::result::Result<Event, AdkError>]) -> Vec<WithheldBuiltinTool> {
    results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .find_map(|event| event.provider_metadata.get(BUILTIN_TOOLS_WITHHELD_KEY))
        .map(|json| serde_json::from_str(json).unwrap())
        .unwrap_or_default()
}

fn final_usage(results: &[std::result::Result<Event, AdkError>]) -> UsageMetadata {
    results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .filter(|event| !event.llm_response.partial)
        .find_map(|event| event.llm_response.usage_metadata.clone())
        .expect("a final event carries usage")
}

fn policy(rules: DeclarativePolicy) -> RunConfig {
    RunConfig::builder().tool_policy(Arc::new(rules)).build()
}

#[tokio::test]
async fn without_a_policy_builtins_attach_and_their_uses_are_metered_and_priced() {
    for provider in PROVIDERS {
        let (model, server) = provider.model(2).await;
        let results = run(provider.agent("searcher", model), RunConfig::default()).await;

        let bodies = received(&server).await;
        assert_eq!(bodies.len(), 1, "{provider:?}");
        assert!(provider.declares_builtin(&bodies[0]), "{provider:?}: {}", bodies[0]);
        assert!(withheld(&results).is_empty());

        let usage = final_usage(&results);
        let (kind, fee) = provider.reported();
        assert_eq!(usage.builtin_tool_uses, vec![adk_core::BuiltinToolUse::new(kind, 2)]);
        let tokens_only = UsageMetadata { builtin_tool_uses: Vec::new(), ..usage.clone() };
        let token_cost = PricingCatalog::standard()
            .cost_usd(Some(provider_id(provider)), provider.model_id(), &tokens_only)
            .unwrap();
        let cost = usage.cost.expect("priced");
        assert!(
            (cost - token_cost - fee).abs() < 1e-9,
            "{provider:?}: {cost} vs {token_cost} + {fee}"
        );
    }
}

fn provider_id(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "gemini",
        Provider::OpenAI => "openai",
        Provider::Anthropic => "anthropic",
    }
}

#[tokio::test]
async fn a_policy_that_denies_the_builtin_strips_it_from_the_request() {
    for provider in PROVIDERS {
        let (model, server) = provider.model(0).await;
        // Default deny: only `lookup` is permitted.
        let config = policy(DeclarativePolicy::builder().allow("lookup").build());
        let results = run(provider.agent("searcher", model), config).await;

        let bodies = received(&server).await;
        assert_eq!(bodies.len(), 1, "{provider:?}");
        assert!(!provider.declares_builtin(&bodies[0]), "{provider:?}: {}", bodies[0]);
        assert!(bodies[0].to_string().contains("lookup"), "{provider:?}: local tool kept");
        let withheld = withheld(&results);
        assert_eq!(withheld.len(), 1);
        assert_eq!(withheld[0].tool_name, provider.tool_name());
        assert_eq!(withheld[0].cause, BuiltinToolWithholding::PolicyDenied);
    }
}

#[tokio::test]
async fn a_policy_that_allows_the_builtin_keeps_it() {
    for provider in PROVIDERS {
        let (model, server) = provider.model(0).await;
        let config = policy(DeclarativePolicy::builder().allow_read_only().allow("lookup").build());
        let results = run(provider.agent("searcher", model), config).await;

        assert!(provider.declares_builtin(&received(&server).await[0]), "{provider:?}");
        assert!(withheld(&results).is_empty());
    }
}

#[tokio::test]
async fn require_approval_without_a_decision_strips_and_records_pending_then_approval_attaches() {
    for provider in PROVIDERS {
        let store = Arc::new(InMemoryApprovalStore::new());
        let rules = DeclarativePolicy::builder()
            .require_approval(provider.tool_name(), "web access needs sign-off")
            .allow("lookup")
            .build();
        let config =
            RunConfig::builder().tool_policy(Arc::new(rules)).approval_store(store.clone()).build();

        let (model, server) = provider.model(0).await;
        let results = run(provider.agent("searcher", model.clone()), config.clone()).await;
        assert!(!provider.declares_builtin(&received(&server).await[0]), "{provider:?}");
        assert_eq!(withheld(&results)[0].cause, BuiltinToolWithholding::ApprovalRequired);

        let scope = ApprovalScope::new("app", "user", "session");
        let pending = store.pending(&scope).await.unwrap();
        assert_eq!(pending.len(), 1, "{provider:?}");
        assert_eq!(pending[0].request.tool_name, provider.tool_name());
        store.decide(&scope, &pending[0].fingerprint, ToolApproval::approve()).await.unwrap();

        let results = run(provider.agent("searcher", model), config).await;
        let bodies = received(&server).await;
        assert!(provider.declares_builtin(&bodies[1]), "{provider:?}");
        assert!(withheld(&results).is_empty());
    }
}

#[tokio::test]
async fn a_frozen_run_sends_no_request() {
    for provider in PROVIDERS {
        let (model, server) = provider.model(0).await;
        let control = GovernanceControl::new();
        control.freeze("incident drill");
        let config = RunConfig::builder().governance(control).build();
        let results = run(provider.agent("searcher", model), config).await;

        assert!(received(&server).await.is_empty(), "{provider:?}");
        let error = results.iter().find_map(|result| result.as_ref().err()).expect("error");
        assert_eq!(error.code, "governance.frozen");
    }
}

#[tokio::test]
async fn builtin_uses_count_against_the_tool_call_budget() {
    for provider in PROVIDERS {
        // Two searches reach a two-call limit: the next agent's request goes out
        // without the built-in tool.
        let (model, server) = provider.model(2).await;
        let pipeline = Arc::new(SequentialAgent::new(
            "pipeline",
            vec![provider.agent("searcher", model.clone()), provider.agent("follower", model)],
        ));
        let config = RunConfig::builder().budget(RunBudget::new().max_tool_calls(2)).build();
        let results = run(pipeline, config).await;
        let bodies = received(&server).await;
        assert_eq!(bodies.len(), 2, "{provider:?}");
        assert!(provider.declares_builtin(&bodies[0]));
        assert!(!provider.declares_builtin(&bodies[1]), "{provider:?}: {}", bodies[1]);
        assert_eq!(withheld(&results)[0].cause, BuiltinToolWithholding::BudgetExhausted);

        // Three searches exceed it: the next model turn is refused.
        let (model, server) = provider.model(3).await;
        let pipeline = Arc::new(SequentialAgent::new(
            "pipeline",
            vec![provider.agent("searcher", model.clone()), provider.agent("follower", model)],
        ));
        let config = RunConfig::builder().budget(RunBudget::new().max_tool_calls(2)).build();
        let results = run(pipeline, config).await;
        assert_eq!(received(&server).await.len(), 1, "{provider:?}");
        let error = results.iter().find_map(|result| result.as_ref().err()).expect("error");
        assert_eq!(error.category, ErrorCategory::ResourceExhausted, "{error}");
        assert_eq!(error.code, "budget.tool_calls");
    }
}
