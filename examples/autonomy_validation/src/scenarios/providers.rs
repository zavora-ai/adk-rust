//! Provider fixes (Phase 1): GPT-5.6 tool calls on the Chat Completions client are routed
//! to the Responses API, and Anthropic thinking blocks are replayed across tool-use turns.

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Llm, Part, Tool};
use adk_model::anthropic::ThinkingMode;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use serde_json::json;

use crate::common::{
    Config, CountingLlm, CountingTool, Provider, Verdict, brief, fail, pair_calls, run_turn,
    session_events,
};

const USER: &str = "user-providers";

fn weather_tool() -> Arc<CountingTool> {
    CountingTool::new(
        "get_weather",
        "Returns the current weather for a city.",
        json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
        |_, args| Ok(json!({ "city": args["city"], "conditions": "rain", "temperature_c": 4 })),
    )
}

async fn weather_runner(
    model: Arc<CountingLlm>,
    weather: &Arc<CountingTool>,
    app: &str,
    session_id: &str,
) -> anyhow::Result<(Runner, Arc<InMemorySessionService>)> {
    let agent = LlmAgentBuilder::new("weather_agent")
        .model(model as Arc<dyn Llm>)
        .instruction("Answer weather questions by calling get_weather, then reply in one sentence.")
        .tool(Arc::clone(weather) as Arc<dyn Tool>)
        .build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: app.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(app)
        .agent(Arc::new(agent))
        .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
        .build()?;
    Ok((runner, sessions))
}

// ─── openai_tools_routing ───────────────────────────────────────────────────

pub async fn openai_tools_routing(cfg: &Config, provider: Provider) -> Verdict {
    if provider != Provider::OpenAiChat {
        return Verdict::Skip("Chat Completions client only".to_string());
    }
    match routing(cfg).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn routing(cfg: &Config) -> anyhow::Result<Verdict> {
    let model = cfg.openai_chat(&cfg.openai_routed_model)?;
    let weather = weather_tool();
    let app = "autonomy-routing";
    let session_id = "routing";
    let (runner, sessions) = weather_runner(Arc::clone(&model), &weather, app, session_id).await?;
    let turn = run_turn(&runner, USER, session_id, "What is the weather in Oslo right now?").await;
    if let Some(error) = &turn.error {
        return Ok(Verdict::Fail(format!(
            "{} on the Chat Completions client failed: {}",
            cfg.openai_routed_model,
            brief(error)
        )));
    }
    if weather.executions() == 0 {
        return Ok(Verdict::Retry("model never called get_weather".to_string()));
    }
    let events = session_events(sessions.as_ref(), app, USER, session_id).await?;
    let (_, problems) = pair_calls(&events);
    if !problems.is_empty() {
        return Ok(Verdict::Fail(format!("history pairing: {}", problems.join("; "))));
    }
    let answered = events.iter().any(|event| {
        event.author == "weather_agent"
            && event.llm_response.content.as_ref().is_some_and(|content| {
                content
                    .parts
                    .iter()
                    .any(|part| matches!(part, Part::Text { text } if !text.trim().is_empty()))
            })
    });
    if !answered {
        return Ok(Verdict::Fail("no final answer after the tool result".to_string()));
    }
    Ok(Verdict::Pass(format!(
        "{} with function tools on OpenAIClient: get_weather {}x, {} model calls, final answer given",
        cfg.openai_routed_model,
        weather.executions(),
        model.calls()
    )))
}

// ─── anthropic_thinking_replay ──────────────────────────────────────────────

pub async fn anthropic_thinking_replay(cfg: &Config, provider: Provider) -> Verdict {
    if provider != Provider::Anthropic {
        return Verdict::Skip("Anthropic only".to_string());
    }
    match thinking(cfg).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

async fn thinking(cfg: &Config) -> anyhow::Result<Verdict> {
    // Budget thinking thinks on every turn, and the API rejects a tool-use turn replayed
    // without its thinking block, so an accepted second request proves the replay.
    let model = cfg.anthropic(&cfg.anthropic_thinking_model, |config| {
        config.with_thinking_mode(ThinkingMode::Enabled { budget_tokens: 2_048 })
    })?;
    let weather = weather_tool();
    let app = "autonomy-thinking";
    let session_id = "thinking";
    let (runner, sessions) = weather_runner(Arc::clone(&model), &weather, app, session_id).await?;

    let first = run_turn(
        &runner,
        USER,
        session_id,
        "Use get_weather for Paris, then tell me whether I need a coat.",
    )
    .await;
    if let Some(error) = &first.error {
        return Ok(Verdict::Fail(format!("turn 1 failed: {}", brief(error))));
    }
    if weather.executions() == 0 {
        return Ok(Verdict::Retry("model never called get_weather in turn 1".to_string()));
    }
    let second =
        run_turn(&runner, USER, session_id, "Now do the same for Oslo, using get_weather again.")
            .await;
    if let Some(error) = &second.error {
        return Ok(Verdict::Fail(format!(
            "turn 2 replaying the thinking history was rejected: {}",
            brief(error)
        )));
    }
    let events = session_events(sessions.as_ref(), app, USER, session_id).await?;
    let signed = events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter(|part| {
            matches!(part, Part::Thinking { signature: Some(signature), .. } if !signature.is_empty())
        })
        .count();
    if signed == 0 {
        return Ok(Verdict::Fail(
            "no signed thinking block was kept in session history".to_string(),
        ));
    }
    let (_, problems) = pair_calls(&events);
    if !problems.is_empty() {
        return Ok(Verdict::Fail(format!("history pairing: {}", problems.join("; "))));
    }
    if weather.executions() < 2 {
        return Ok(Verdict::Retry("model did not call get_weather in turn 2".to_string()));
    }
    Ok(Verdict::Pass(format!(
        "{} with budget thinking: {signed} signed thinking block(s) in history; {} model calls across 2 tool-use turns accepted; get_weather {}x",
        cfg.anthropic_thinking_model,
        model.calls(),
        weather.executions()
    )))
}
