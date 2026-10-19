//! Item 7: Ollama honours `max_output_tokens` and reports a capped turn as `MaxTokens`.
//!
//! Runs against a local Ollama server (`OLLAMA_HOST`, default `http://localhost:11434`) with a
//! small model (`OLLAMA_MODEL`, default `smollm2:135m`); skipped when none answers.

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, FinishReason, GenerateContentConfig, SessionId, UserId};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

use crate::report::{Mode, Report, Verdict, check, fail, key, skip};

const CAP: i32 = 8;

pub async fn run(report: &mut Report) {
    let host = key("OLLAMA_HOST").unwrap_or_else(|| "http://localhost:11434".to_string());
    let model = key("OLLAMA_MODEL").unwrap_or_else(|| "smollm2:135m".to_string());
    let reachable = reqwest::Client::new()
        .get(format!("{host}/api/tags"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success());
    let verdict = if reachable {
        capped_turn(&host, &model).await
    } else {
        Ok(skip(format!("no Ollama server at {host}")))
    };
    report.record_result("ollama_token_cap", 7, Mode::Live, verdict);
}

async fn capped_turn(host: &str, model: &str) -> anyhow::Result<Verdict> {
    let agent = LlmAgentBuilder::new("storyteller")
        .model(Arc::new(OllamaModel::new(OllamaConfig::with_host(host, model))?))
        .instruction("You write long, detailed stories.")
        .generate_content_config(GenerateContentConfig {
            max_output_tokens: Some(CAP),
            ..Default::default()
        })
        .build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: "release-live-checks".to_string(),
            user_id: "user-1".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("release-live-checks")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .build()?;
    let mut stream = runner
        .run(
            UserId::new("user-1")?,
            SessionId::new(session.id())?,
            Content::new("user").with_text("Tell me a long story about a lighthouse keeper."),
        )
        .await?;
    let mut finish = None;
    let mut output_tokens = None;
    while let Some(event) = stream.next().await {
        let event = match event {
            Ok(event) => event,
            Err(error) => return Ok(fail(format!("{}: {}", error.code, error.message))),
        };
        if !event.llm_response.partial {
            finish = event.llm_response.finish_reason.or(finish);
            output_tokens = event
                .llm_response
                .usage_metadata
                .as_ref()
                .map(|usage| usage.candidates_token_count)
                .or(output_tokens);
        }
    }
    Ok(check(
        finish == Some(FinishReason::MaxTokens) && output_tokens.is_some_and(|n| n <= CAP),
        format!("finish={finish:?} output_tokens={output_tokens:?} (cap {CAP})"),
    ))
}
