//! Auto-compaction against a live model: an `LlmAgent` on Anthropic reads a record one page
//! per tool call, with a low threshold so the run is cheap, and compacts between its calls.
//! Claude writes the summary itself (on-demand compaction), and the signed block it returns
//! opens every later request.
//!
//! Requires `ANTHROPIC_API_KEY`; run with `--run-ignored only`.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    BudgetTracker, Content, Event, IntraCompactionConfig, Llm, RunBudget, RunConfig, SessionId,
    ToolContext, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const PAGES: u64 = 12;

/// The cursor that unlocks page `number`: each page names the next one's, so pages are read
/// one call at a time.
fn cursor(number: u64) -> String {
    if number == 1 { "start".to_string() } else { format!("c{}", number * 7_919 % 10_007) }
}

/// One page of a ledger: about 600 tokens of entries, the page's total, and the next cursor.
fn page(number: u64, given: &str) -> Value {
    if given != cursor(number) {
        return json!({ "error": format!("wrong cursor for page {number}; use the cursor the previous page gave") });
    }
    let entries: Vec<String> = (1..=40)
        .map(|line| {
            format!("entry {number}.{line}: account {} moved {} units", line * 7, line * number)
        })
        .collect();
    let next =
        (number < PAGES).then(|| json!({ "page": number + 1, "cursor": cursor(number + 1) }));
    json!({ "page": number, "of": PAGES, "entries": entries, "page_total": number * 820, "next": next })
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
        "Reads one page of the ledger. Page 1 takes the cursor \"start\"; each page gives the \
         cursor of the next.",
        |_ctx: Arc<dyn ToolContext>, args: Value| async move {
            Ok(page(args["page"].as_u64().unwrap_or(1), args["cursor"].as_str().unwrap_or("")))
        },
    )
    .with_parameters_schema::<PageArgs>();
    let agent = LlmAgentBuilder::new("auditor")
        .instruction(format!(
            "You audit a ledger of {PAGES} pages. Read them in order with read_page, starting \
             at page 1 with cursor \"start\"; each page gives the cursor of the next. Keep a \
             running sum of each page's page_total. After the last page, reply with only the sum."
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
        .intra_compaction_config(
            IntraCompactionConfig::at_tokens(5_000).with_overlap_event_count(4),
        )
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
        if let Some(compaction) = &event.actions.compaction {
            compactions += 1;
            // Claude Sonnet 5.5 compacts on demand, so Claude writes the summary.
            assert!(
                adk_core::intra_compaction::is_native_compaction(&compaction.compacted_content),
                "the provider wrote the summary"
            );
            println!("     | compaction recorded (native) |");
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
    println!(
        "model calls: {calls}, compactions: {compactions}, answer: {answer:?}, expected sum: {expected}"
    );
    assert!(compactions >= 1, "the run compacted between its model calls");
    assert!(!answer.trim().is_empty(), "the run completed with an answer");
}

/// What one run of the ledger audit came to.
struct Audit {
    events: Vec<Event>,
    answer: String,
}

async fn audit(runner: &Runner, text: &str, config: RunConfig) -> Audit {
    let events: Vec<Event> = runner
        .run_with_config(
            UserId::try_from("u").unwrap(),
            SessionId::try_from("s").unwrap(),
            Content::new("user").with_text(text),
            Some(config),
        )
        .await
        .unwrap()
        .map(|event| event.expect("no run error"))
        .collect()
        .await;
    let answer = events
        .iter()
        .rev()
        .filter(|event| !event.llm_response.partial)
        .filter_map(|event| event.content())
        .map(|content| content.parts.iter().filter_map(|part| part.text()).collect::<String>())
        .find(|text| !text.trim().is_empty())
        .unwrap_or_default();
    Audit { events, answer }
}

/// Whether a captured Messages request body opens with a compaction block.
fn opens_with_compaction(body: &Value) -> bool {
    body["messages"][0]["content"][0]["type"] == "compaction"
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn a_live_native_compaction_keeps_the_run_and_its_thinking() {
    dotenvy::dotenv().ok();
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    const MODEL: &str = "claude-haiku-5-5";
    // Every request body and its headers, as sent.
    let sent: Arc<Mutex<Vec<(Value, reqwest::header::HeaderMap)>>> = Arc::default();
    let capture = sent.clone();
    let model = adk_model::anthropic::AnthropicClient::new(
        // Adaptive thinking at high effort, so turns carry thinking for the kept-thinking check.
        adk_model::anthropic::AnthropicConfig::new(key.clone(), MODEL)
            .with_max_tokens(4_096)
            .with_thinking_mode(adk_model::anthropic::ThinkingMode::Adaptive)
            .with_effort(adk_model::anthropic::Effort::High),
    )
    .unwrap()
    .with_request_adapter(Arc::new(move |body, headers| {
        capture.lock().unwrap().push((body.clone(), headers.clone()));
        Ok(())
    }));
    assert!(model.native_compaction().is_some(), "{MODEL} compacts on demand");
    let read_page = FunctionTool::new(
        "read_page",
        "Reads one page of the ledger. Page 1 takes the cursor \"start\"; each page gives the \
         cursor of the next.",
        |_ctx: Arc<dyn ToolContext>, args: Value| async move {
            Ok(page(args["page"].as_u64().unwrap_or(1), args["cursor"].as_str().unwrap_or("")))
        },
    )
    .with_parameters_schema::<PageArgs>();
    let agent = LlmAgentBuilder::new("auditor")
        .instruction(format!(
            "You audit a ledger of {PAGES} pages. Read them in order with read_page, starting \
             at page 1 with cursor \"start\"; each page gives the cursor of the next. Keep a \
             running sum of each page's page_total. After the last page, reply with only the sum."
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
        // COMPACTION: a low threshold keeps the run cheap; the model's provider writes the summary.
        .intra_compaction_config(
            IntraCompactionConfig::at_tokens(5_000).with_overlap_event_count(4),
        )
        .build()
        .unwrap();
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let config = RunConfig { budget_tracker: Some(tracker.clone()), ..Default::default() };

    let first = audit(&runner, "Audit the ledger.", config.clone()).await;

    let expected: u64 = (1..=PAGES).map(|number| number * 820).sum();
    let compactions: Vec<&Event> =
        first.events.iter().filter(|event| event.actions.compaction.is_some()).collect();
    let bodies: Vec<Value> = sent.lock().unwrap().iter().map(|(body, _)| body.clone()).collect();
    let asked = bodies.iter().filter(|body| body.get("compaction").is_some()).count();
    let replayed = bodies.iter().filter(|body| opens_with_compaction(body)).count();
    let agent_calls = bodies.len() - asked;
    let thinking_blocks = bodies
        .iter()
        .filter_map(|body| body["messages"].as_array())
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "thinking")
        .count();
    println!("thinking blocks replayed across all requests: {thinking_blocks}");
    println!(
        "requests: {} ({agent_calls} agent calls, {asked} compaction requests, {replayed} opening \
         with the signed block); compactions recorded: {}; budget model calls: {}; answer: {:?}; \
         expected: {expected}",
        bodies.len(),
        compactions.len(),
        tracker.usage().model_calls,
        first.answer
    );
    assert!(!compactions.is_empty(), "the run compacted natively");
    for event in &compactions {
        let content = &event.actions.compaction.as_ref().unwrap().compacted_content;
        assert!(adk_core::intra_compaction::is_native_compaction(content), "{content:?}");
        assert_eq!(
            adk_core::history::ProviderOrigin::of_response(
                event.llm_response.provider_metadata.as_ref()
            ),
            Some(adk_core::history::ProviderOrigin::new("anthropic", MODEL))
        );
    }
    assert_eq!(asked, compactions.len(), "every compaction was the provider's");
    assert!(replayed >= 1, "later requests open with the signed block");
    assert_eq!(tracker.usage().model_calls, bodies.len() as u64, "compaction calls are metered");
    assert!(
        first.answer.contains(&expected.to_string()),
        "the run kept working: {:?}",
        first.answer
    );

    // The kept turns' thinking holds: the first request after a swap that replays thinking is
    // sent again with the binding check set to fail on any mismatch.
    let headers = sent.lock().unwrap()[0].1.clone();
    check_kept_thinking(&bodies, &headers, &key).await;

    // The next run starts from the signed block the session recorded and still answers.
    let before = sent.lock().unwrap().len();
    let second = audit(
        &runner,
        "Which page had the largest page_total, and what was it? Answer in one line.",
        config,
    )
    .await;
    let next = sent.lock().unwrap()[before].0.clone();
    println!("second run answer: {:?}", second.answer);
    assert!(opens_with_compaction(&next), "the next run opens with the signed block");
    assert!(
        second.answer.contains("12")
            || second.answer.contains("9840")
            || second.answer.contains("9,840")
    );
}

/// Sends the first request that opens with a compaction block and replays thinking after it
/// again, with the binding check set to fail on any mismatch: a success with no dropped block
/// shows the kept turns' thinking is still bound to the conversation.
async fn check_kept_thinking(bodies: &[Value], headers: &reqwest::header::HeaderMap, key: &str) {
    let with_thinking = bodies
        .iter()
        .find(|body| {
            opens_with_compaction(body)
                && body["messages"].as_array().unwrap().iter().skip(1).any(|message| {
                    message["content"].as_array().is_some_and(|blocks| {
                        blocks.iter().any(|block| block["type"] == "thinking")
                    })
                })
        })
        .cloned();
    let Some(mut body) = with_thinking else {
        println!("kept-thinking check: no request after a swap replayed thinking");
        return;
    };
    body["thinking"] = json!({
        "type": "adaptive",
        "block_binding": {"prefix_mismatch_behavior": "error"}
    });
    body["max_tokens"] = json!(1_024);
    body["stream"] = json!(false);
    let response = reqwest::Client::new()
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", key)
        .header("anthropic-version", headers.get("anthropic-version").unwrap().to_str().unwrap())
        .header("anthropic-beta", "compact-2026-09-04,thinking-binding-controls-2026-08-01")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let reply: Value = response.json().await.unwrap();
    println!(
        "kept-thinking check: status {status}, input_transformations {}",
        reply.get("input_transformations").cloned().unwrap_or(Value::Null)
    );
    assert!(status.is_success(), "kept thinking failed the binding check: {reply}");
    assert!(
        reply["input_transformations"].as_array().is_none_or(Vec::is_empty),
        "no thinking block was dropped: {reply}"
    );
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn a_live_background_compaction_started_in_one_turn_opens_the_next() {
    dotenvy::dotenv().ok();
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    const MODEL: &str = "claude-haiku-5-5";
    // Every request body and its headers, with when it was sent.
    type Sent = (Value, reqwest::header::HeaderMap, std::time::Instant);
    let sent: Arc<Mutex<Vec<Sent>>> = Arc::default();
    let capture = sent.clone();
    let model = adk_model::anthropic::AnthropicClient::new(
        adk_model::anthropic::AnthropicConfig::new(key.clone(), MODEL)
            .with_max_tokens(4_096)
            .with_thinking_mode(adk_model::anthropic::ThinkingMode::Adaptive)
            .with_effort(adk_model::anthropic::Effort::High),
    )
    .unwrap()
    .with_request_adapter(Arc::new(move |body, headers| {
        capture.lock().unwrap().push((body.clone(), headers.clone(), std::time::Instant::now()));
        Ok(())
    }));
    let read_page = FunctionTool::new(
        "read_page",
        "Reads one page of the ledger. Page 1 takes the cursor \"start\"; each page gives the \
         cursor of the next.",
        |_ctx: Arc<dyn ToolContext>, args: Value| async move {
            Ok(page(args["page"].as_u64().unwrap_or(1), args["cursor"].as_str().unwrap_or("")))
        },
    )
    .with_parameters_schema::<PageArgs>();
    let agent = LlmAgentBuilder::new("auditor")
        .instruction(format!(
            "You audit a ledger of {PAGES} pages. Read the pages the user asks for, in order, \
             with read_page: page 1 takes the cursor \"start\" and each page gives the cursor \
             of the next. Reply with only the number asked for."
        ))
        .model(Arc::new(model) as Arc<dyn Llm>)
        .tool(Arc::new(read_page))
        .max_iterations(20)
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
        // COMPACTION: summaries start in the background at 50% of a 5,000-token threshold.
        .intra_compaction_config(
            IntraCompactionConfig::at_tokens(5_000).with_overlap_event_count(4).in_background(0.5),
        )
        .build()
        .unwrap();
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let config = RunConfig { budget_tracker: Some(tracker.clone()), ..Default::default() };

    let first = audit(
        &runner,
        "Read pages 1 through 4, then reply with the sum of their page_total values.",
        config.clone(),
    )
    .await;
    let first_turn = sent.lock().unwrap().len();
    let started: Vec<std::time::Instant> = sent.lock().unwrap()[..first_turn]
        .iter()
        .filter(|(body, _, _)| body.get("compaction").is_some())
        .map(|(_, _, at)| *at)
        .collect();
    let first_compactions =
        first.events.iter().filter(|event| event.actions.compaction.is_some()).count();
    println!(
        "first turn: {first_turn} requests, {} compaction requests started, {first_compactions} \
         swapped in; answer {:?}",
        started.len(),
        first.answer
    );
    assert!(!started.is_empty(), "a summary started in the background during the first turn");
    assert!(first.answer.contains(&(820 * 10).to_string()), "{:?}", first.answer);

    // The person takes a moment to reply; the summary is written meanwhile.
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    let second = audit(
        &runner,
        "Now read pages 5 and 6, then reply with the sum of page_total over pages 1 through 6.",
        config,
    )
    .await;

    let all: Vec<Sent> = sent.lock().unwrap().clone();
    let opening = &all[first_turn];
    let second_compactions =
        second.events.iter().filter(|event| event.actions.compaction.is_some()).count();
    println!(
        "second turn: first request opens with the signed block: {}; {} compaction requests in the \
         second turn; {second_compactions} swapped in; answer {:?}; budget model calls {}",
        opens_with_compaction(&opening.0),
        all[first_turn..].iter().filter(|(body, _, _)| body.get("compaction").is_some()).count(),
        second.answer,
        tracker.usage().model_calls,
    );
    assert!(opening.0.get("compaction").is_none(), "the second turn wrote no summary first");
    assert!(opens_with_compaction(&opening.0), "the second turn opens with the summary");
    assert!(second_compactions >= 1, "the swap is recorded in the session");
    assert!(second.answer.contains(&(820 * 21).to_string()), "{:?}", second.answer);
    assert_eq!(tracker.usage().model_calls, all.len() as u64, "every call is metered");
    let bodies: Vec<Value> = all.iter().map(|(body, _, _)| body.clone()).collect();
    check_kept_thinking(&bodies[first_turn..], &opening.1, &key).await;
}

#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct PageArgs {
    /// The page to read, from 1.
    page: u64,
    /// The cursor the previous page gave; "start" for page 1.
    cursor: String,
}
