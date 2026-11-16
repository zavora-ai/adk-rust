//! Auto-compaction against a live model: an `LlmAgent` on Anthropic reads a record one page
//! per tool call, with a low threshold so the run is cheap, and compacts between its calls.
//!
//! Requires `ANTHROPIC_API_KEY`; run with `--run-ignored only`.

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Event, IntraCompactionConfig, Llm, SessionId, ToolContext, UserId};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

const PAGES: u64 = 12;

/// One page of a ledger: about 600 tokens of entries, and the page's total.
fn page(number: u64) -> Value {
    let entries: Vec<String> = (1..=40)
        .map(|line| format!("entry {number}.{line}: account {} moved {} units", line * 7, line * number))
        .collect();
    json!({ "page": number, "of": PAGES, "entries": entries, "page_total": number * 820 })
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn a_live_tool_loop_compacts_mid_run_and_completes() {
    dotenvy::dotenv().ok();
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let model = adk_model::anthropic::AnthropicClient::new(
        adk_model::anthropic::AnthropicConfig::new(key, "claude-sonnet-5-5").with_max_tokens(2_048),
    )
    .unwrap();
    let read_page = FunctionTool::new(
        "read_page",
        "Reads one page of the ledger. Pages are numbered from 1.",
        |_ctx: Arc<dyn ToolContext>, args: Value| async move {
            Ok(page(args["page"].as_u64().unwrap_or(1)))
        },
    )
    .with_parameters_schema::<PageArgs>();
    let agent = LlmAgentBuilder::new("auditor")
        .instruction(format!(
            "You audit a ledger of {PAGES} pages. Call read_page once per page, one page per \
             call, from page 1 to page {PAGES}, and keep a running sum of each page's \
             page_total. After the last page, reply with only the sum."
        ))
        .model(Arc::new(model) as Arc<dyn Llm>)
        .tool(Arc::new(read_page))
        .max_iterations(40)
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "live".to_string(),
            user_id: "u".to_string(),
            session_id: Some("s".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("live")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        // COMPACTION: a low threshold keeps the run cheap.
        .intra_compaction_config(IntraCompactionConfig::at_tokens(5_000).with_overlap_event_count(4))
        .build()
        .unwrap();

    let events: Vec<_> = runner
        .run_with_config(
            UserId::try_from("u").unwrap(),
            SessionId::try_from("s").unwrap(),
            Content::new("user").with_text("Audit the ledger."),
            None,
        )
        .await
        .unwrap()
        .collect()
        .await;

    let mut calls = 0;
    let mut compactions = 0;
    let mut answer = String::new();
    println!("step | event | prompt tokens");
    for event in &events {
        let event: &Event = event.as_ref().expect("no run error");
        if event.actions.compaction.is_some() {
            compactions += 1;
            println!("     | compaction recorded |");
        }
        if event.llm_response.partial {
            continue;
        }
        if let Some(usage) = &event.llm_response.usage_metadata {
            calls += 1;
            let calls_tool = event.content().is_some_and(|content| {
                content.parts.iter().any(|part| matches!(part, adk_core::Part::FunctionCall { .. }))
            });
            let kind = if calls_tool { "model call (tool)" } else { "model call (answer)" };
            println!("{calls:>4} | {kind} | {}", usage.prompt_token_count);
        }
        if let Some(content) = event.content() {
            let text: String = content.parts.iter().filter_map(|part| part.text()).collect();
            if !text.trim().is_empty() {
                answer = text;
            }
        }
    }
    let expected: u64 = (1..=PAGES).map(|number| number * 820).sum();
    println!("model calls: {calls}, compactions: {compactions}, answer: {answer:?}, expected sum: {expected}");
    assert!(compactions >= 1, "the run compacted between its model calls");
    assert!(!answer.trim().is_empty(), "the run completed with an answer");
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct PageArgs {
    /// The page to read, from 1.
    page: u64,
}
