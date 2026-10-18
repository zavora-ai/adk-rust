//! ML13: an Anthropic assistant turn with no usable blocks (Claude sometimes ends a turn with
//! only whitespace) must not make every later request fail.
//!
//! Turn 1 replays such a response from the scripted upstream; turn 2 goes to the live API on
//! the same session.

use std::collections::HashMap;
use std::sync::Arc;

use adk_model::AnthropicClient;
use adk_model::anthropic::AnthropicConfig;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use serde_json::Value;

use crate::common::{
    AgentSpec, Keys, Mode, Outcome, USER, Verdict, build_runner, fail, pass, run_turn, short,
};
use crate::upstream::{Proxy, Reply, Upstream, anthropic};

const FINDING: &str = "ML13";
const MODEL: &str = "claude-sonnet-4-6";

/// Text blocks in an Anthropic request whose text is empty or whitespace.
fn blank_text_blocks(body: &Value) -> usize {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|message| match &message["content"] {
            Value::Array(blocks) => blocks.clone(),
            Value::String(text) => vec![serde_json::json!({"type": "text", "text": text})],
            _ => Vec::new(),
        })
        .filter(|block| block["type"] == "text")
        .filter(|block| block["text"].as_str().is_some_and(|text| text.trim().is_empty()))
        .count()
}

async fn after_blank_turn(key: &str, blocks: &'static [&'static str]) -> anyhow::Result<Verdict> {
    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: "ml13".into(),
            user_id: USER.into(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let session_id = session.id().to_string();
    let spec = AgentSpec::new("You are a concise assistant.");

    let upstream = Upstream::start().await?;
    upstream.push(Reply::Sse(anthropic::turn(blocks)));
    let scripted = AnthropicClient::new(
        AnthropicConfig::new("test-key", MODEL).with_base_url(&upstream.base),
    )?;
    let first = run_turn(
        &build_runner("ml13", Arc::new(scripted), &spec, sessions.clone())?,
        &session_id,
        "Hello.",
        None,
    )
    .await;
    // The blank answer may end turn 1 with an error; what matters is whether it is replayed.
    let first_error = first.error.map(|error| short(&error, 60)).unwrap_or_else(|| "none".into());

    let proxy = Proxy::start("https://api.anthropic.com").await?;
    let live = AnthropicClient::new(AnthropicConfig::new(key, MODEL).with_base_url(&proxy.base))?;
    let runner = build_runner("ml13", Arc::new(live), &spec, sessions)?;
    let second =
        run_turn(&runner, &session_id, "What is 2 + 2? Reply with the number only.", None).await;
    let blank: usize =
        proxy.captured().iter().map(|request| blank_text_blocks(&request.body)).sum();
    if let Some(error) = second.error {
        return Ok(fail(format!(
            "live turn after the blank turn failed ({blank} blank blocks sent): {}",
            short(&error, 200)
        )));
    }
    if blank > 0 {
        return Ok(fail(format!("{blank} blank text blocks were sent")));
    }
    Ok(pass(format!(
        "turn 1 error: {first_error}; live turn answered {:?}",
        short(&second.answer(), 20)
    )))
}

pub async fn run(keys: &Keys, outcomes: &mut Vec<Outcome>) {
    for (id, blocks) in [
        ("ml13-anthropic-whitespace", &["\n\n"] as &'static [&'static str]),
        ("ml13-anthropic-empty-text", &[""]),
    ] {
        let verdict = match &keys.anthropic {
            None => Verdict::Skip("ANTHROPIC_API_KEY not set".into()),
            Some(key) => after_blank_turn(key, blocks)
                .await
                .unwrap_or_else(|error| fail(format!("setup: {error}"))),
        };
        outcomes.push(Outcome { id, finding: FINDING, mode: Mode::Live, verdict });
    }
}
