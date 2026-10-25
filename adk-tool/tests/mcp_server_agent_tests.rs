//! A buyer agent using a site's MCP server (spec 3.5 §7.4, §7.9): an `LlmAgent` whose
//! `McpToolset` connects over Streamable HTTP to an in-process `McpServer`. The scripted run
//! asserts what the model was offered, what ran and what the executor saw; the live run does
//! the same on `claude-sonnet-5-5`.
//!
//! The live run needs `ANTHROPIC_API_KEY`, read from the repository's `.env` when present:
//!
//! ```text
//! cargo test -p adk-tool --features mcp-server,http-transport --test mcp_server_agent_tests \
//!     -- --ignored --nocapture
//! ```

#[path = "common/mcp_server.rs"]
mod fixture;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use adk_core::{Agent, Content, Event, Llm, Part};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::mcp::{McpAuth, McpHttpClientBuilder};
use fixture::*;
use futures::StreamExt;
use serde_json::{Value, json};

const APP: &str = "buyer";

/// Runs one turn of a buyer agent on `model`, connected to `running` with `auth`.
async fn turn(running: &Running, auth: McpAuth, model: Arc<dyn Llm>, text: &str) -> Vec<Event> {
    let toolset = McpHttpClientBuilder::new(running.endpoint()).with_auth(auth).connect().await;
    let agent = adk_agent::LlmAgentBuilder::new("buyer")
        .description("Buys solar installations for its principal")
        .instruction("You buy solar installations. Use the site's tools; never invent prices.")
        .model(model)
        .toolset(Arc::new(toolset.unwrap()))
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: "principal".to_string(),
            session_id: Some("s1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = adk_runner::Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let stream = runner.run_str("principal", "s1", Content::new("user").with_text(text)).await;
    let events: Vec<Event> = stream.unwrap().map(|event| event.unwrap()).collect().await;
    runner.close().await;
    events
}

/// Every function response in `events`, by tool name.
fn responses(events: &[Event]) -> Vec<(String, Value)> {
    events
        .iter()
        .flat_map(|event| event.llm_response.content.iter().flat_map(|content| &content.parts))
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some((function_response.name.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The text of the last model response.
fn final_text(events: &[Event]) -> String {
    events
        .iter()
        .rev()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .find_map(|part| match part {
            Part::Text { text } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_scripted_buyer_quotes_and_books_through_the_mcp_server() {
    let executor = GateExecutor::new();
    let running = start(server(executor.clone()), |router| {
        router.caller_resolver(Arc::new(Tokens::new(false)))
    })
    .await;
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("get_quote", json!({ "kw": 6 })),
            ScriptedResponse::function_call("book_install", json!({ "date": "2026-11-03" })),
            ScriptedResponse::text("Booked for 3 November at 6000."),
        ],
    ));
    let events = turn(
        &running,
        McpAuth::bearer("token-acme"),
        model.clone(),
        "Quote 6 kW, then book 2026-11-03.",
    )
    .await;

    let offered = &model.requests()[0].tools;
    let get_quote = &offered["get_quote"];
    assert_eq!(get_quote["description"], "Quotes a solar installation");
    assert_eq!(get_quote["parameters"]["properties"]["kw"]["type"], "integer");
    assert!(offered.contains_key("admin_report"), "a partner is offered the partner tool");

    let answered = responses(&events);
    let names: Vec<&str> = answered.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["get_quote", "book_install"]);
    assert_eq!(answered[0].1["output"], json!({ "kw": 6, "price": 6000 }));
    assert_eq!(answered[1].1["output"], json!({ "booking": "B-2026-11-03" }));
    assert_eq!(final_text(&events), "Booked for 3 November at 6000.");

    let seen: Vec<(String, String)> =
        executor.seen().into_iter().map(|call| (call.tool, call.caller)).collect();
    assert_eq!(
        seen,
        [
            ("get_quote".to_string(), "partner:acme".to_string()),
            ("book_install".to_string(), "partner:acme".to_string()),
        ]
    );
}

#[tokio::test]
async fn a_scripted_anonymous_buyer_is_refused_the_booking() {
    let executor = GateExecutor::new();
    let running = start(server(executor.clone()), |router| {
        router.caller_resolver(Arc::new(Tokens::new(false)))
    })
    .await;
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("book_install", json!({ "date": "2026-11-03" })),
            ScriptedResponse::text("I could not book."),
        ],
    ));
    let events = turn(&running, McpAuth::None, model.clone(), "Book 2026-11-03.").await;

    assert!(!model.requests()[0].tools.contains_key("admin_report"));
    let answered = responses(&events);
    assert_eq!(answered.len(), 1);
    assert!(answered[0].1.get("error").is_some(), "the refusal reaches the model: {answered:?}");
    assert!(answered[0].1.get("output").is_none(), "nothing was booked");
    assert_eq!(executor.seen().len(), 1, "the executor saw and refused the call");
}

/// The Anthropic key from the environment or the repository's `.env`.
fn anthropic_key() -> Option<String> {
    let env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    if env.exists() {
        let _ = dotenvy::from_path_override(&env);
    }
    std::env::var("ANTHROPIC_API_KEY").ok().filter(|key| !key.is_empty())
}

#[tokio::test]
#[ignore = "calls the Anthropic API with ANTHROPIC_API_KEY"]
async fn live_a_buyer_agent_quotes_through_the_mcp_server() {
    let Some(key) = anthropic_key() else {
        println!("skipped: no ANTHROPIC_API_KEY");
        return;
    };
    let model: Arc<dyn Llm> = Arc::new(
        adk_model::anthropic::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(
            key,
            "claude-sonnet-5-5",
        ))
        .unwrap(),
    );
    let executor = GateExecutor::new();
    let running = start(server(executor.clone()), |router| {
        router.caller_resolver(Arc::new(Tokens::new(false)))
    })
    .await;
    let events = turn(
        &running,
        McpAuth::bearer("token-acme"),
        model,
        "Get a quote for a 6 kW installation with the get_quote tool, then reply with one short \
         sentence that states the price.",
    )
    .await;

    let seen = executor.seen();
    println!("executor saw: {seen:#?}");
    let quotes: Vec<&Seen> = seen.iter().filter(|call| call.tool == "get_quote").collect();
    assert!(!quotes.is_empty(), "the model called get_quote through MCP");
    assert_eq!(quotes[0].arguments, json!({ "kw": 6 }));
    assert_eq!(quotes[0].caller, "partner:acme");
    let text = final_text(&events);
    println!("model replied: {text:?}");
    assert!(text.contains("6000") || text.contains("6,000"), "{text}");
}
