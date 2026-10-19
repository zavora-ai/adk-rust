//! Item 4: every `GroqConfig` model preset selects a model Groq lists, and answers a turn.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, SessionId, UserId};
use adk_model::groq::{GroqClient, GroqConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

use crate::report::{Mode, Report, Verdict, check, key, skip};

type Preset = fn(String) -> GroqConfig;

const PRESETS: &[(&str, Preset)] = &[
    ("gpt_oss_120b", |key| GroqConfig::gpt_oss_120b(key)),
    ("gpt_oss_20b", |key| GroqConfig::gpt_oss_20b(key)),
];

pub async fn run(report: &mut Report) {
    let Some(api_key) = key("GROQ_API_KEY") else {
        report.record("groq_presets_listed", 4, Mode::Live, skip("GROQ_API_KEY is not set"));
        return;
    };
    let listed = match listed_models(&api_key).await {
        Ok(listed) => listed,
        Err(error) => {
            report.record_result("groq_presets_listed", 4, Mode::Live, Err(error));
            return;
        }
    };
    let unlisted: Vec<String> = PRESETS
        .iter()
        .map(|(name, preset)| (*name, preset(api_key.clone()).model))
        .filter(|(_, model)| !listed.contains(model))
        .map(|(name, model)| format!("{name} -> {model}"))
        .collect();
    report.record(
        "groq_presets_listed",
        4,
        Mode::Live,
        check(
            unlisted.is_empty(),
            if unlisted.is_empty() {
                format!("{} presets, all listed", PRESETS.len())
            } else {
                format!("not listed: {}", unlisted.join(", "))
            },
        ),
    );
    let mut answered = Vec::new();
    let mut failed = Vec::new();
    for (name, preset) in PRESETS {
        match agent_turn(preset(api_key.clone())).await {
            Ok(answer) if !answer.trim().is_empty() => answered.push(*name),
            Ok(_) => failed.push(format!("{name}: empty answer")),
            Err(error) => failed.push(format!("{name}: {error:#}")),
        }
    }
    let verdict: Verdict = check(
        failed.is_empty(),
        if failed.is_empty() {
            format!("answered: {}", answered.join(", "))
        } else {
            failed.join("; ")
        },
    );
    report.record("groq_presets_answer", 4, Mode::Live, verdict);
}

async fn listed_models(api_key: &str) -> anyhow::Result<HashSet<String>> {
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/models", adk_model::groq::GROQ_API_BASE))
        .bearer_auth(api_key)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["id"].as_str().map(str::to_owned))
        .collect())
}

/// One user turn through `LlmAgent` + `Runner`; the first error event or stream error fails it.
async fn agent_turn(config: GroqConfig) -> anyhow::Result<String> {
    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(GroqClient::new(config)?))
        .instruction("Answer in at most five words.")
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
            Content::new("user").with_text("Name one primary color."),
        )
        .await?;
    let mut answer = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(code) = &event.llm_response.error_code {
            anyhow::bail!("{code}: {}", event.llm_response.error_message.unwrap_or_default());
        }
        if !event.llm_response.partial
            && let Some(content) = &event.llm_response.content
        {
            answer = content.parts.iter().filter_map(Part::text).collect();
        }
    }
    Ok(answer)
}
