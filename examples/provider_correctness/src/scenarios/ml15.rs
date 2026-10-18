//! ML15: an image a tool returns must reach the model, not be dropped.

use std::sync::Arc;

use adk_core::Llm;
use adk_model::AnthropicClient;
use adk_model::anthropic::AnthropicConfig;
use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_model::groq::{GroqClient, GroqConfig};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use serde_json::{Value, json};

use crate::common::{
    AgentSpec, Harness, Keys, Mode, Outcome, Verdict, badge_png, fail, image_tool, pass, short,
};
use crate::upstream::{Proxy, Reply, Upstream, chat, ollama};

const FINDING: &str = "ML15";
const ASK: &str =
    "Render the team badge with the render_badge tool, then tell me its main colour and shape.";

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let bytes = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let value = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(ALPHABET[((value >> (18 - 6 * index)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn spec() -> AgentSpec {
    AgentSpec::new("You describe images that tools return.").tool(image_tool())
}

/// Anthropic `tool_result` blocks whose content holds an image.
fn tool_result_images(body: &Value) -> usize {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "tool_result")
        .filter(|block| {
            block["content"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["type"] == "image"))
        })
        .count()
}

async fn anthropic_live(key: &str) -> anyhow::Result<Verdict> {
    let proxy = Proxy::start("https://api.anthropic.com").await?;
    let client = AnthropicClient::new(
        AnthropicConfig::new(key, "claude-sonnet-4-6").with_base_url(&proxy.base),
    )?;
    let harness = Harness::new("ml15", Arc::new(client), &spec()).await?;
    let turn = harness.turn(ASK).await;
    if let Some(error) = turn.error {
        return Ok(fail(format!("run failed: {}", short(&error, 200))));
    }
    let images: usize =
        proxy.captured().iter().map(|request| tool_result_images(&request.body)).sum();
    let answer = turn.answer().to_lowercase();
    if answer.contains("blue") && answer.contains("circ") {
        Ok(pass(format!("{images} tool_result image blocks; answer {:?}", short(&answer, 70))))
    } else {
        Ok(fail(format!(
            "the model did not see the image ({images} tool_result images): {}",
            short(&answer, 120)
        )))
    }
}

/// The scripted upstream calls `render_badge`, then answers; the second request must carry the
/// PNG in some form.
async fn scripted(
    id: &'static str,
    replies: Vec<Reply>,
    build: impl FnOnce(&str) -> anyhow::Result<Arc<dyn Llm>>,
) -> Outcome {
    let verdict = async {
        let upstream = Upstream::start().await?;
        for reply in replies {
            upstream.push(reply);
        }
        let harness = Harness::new("ml15", build(&upstream.base)?, &spec()).await?;
        let turn = harness.turn(ASK).await;
        let captured = upstream.captured();
        let Some(second) = captured.get(1) else {
            return Ok(fail(format!("no follow-up request (error: {:?})", turn.error)));
        };
        let encoded = base64(&badge_png());
        let sent = second.body.to_string().contains(&encoded);
        Ok::<_, anyhow::Error>(match (sent, turn.error) {
            (false, _) => fail("the follow-up request does not carry the tool's image"),
            (true, Some(error)) => fail(format!("image sent, but the run failed: {error}")),
            (true, None) => pass("the follow-up request carries the tool's image"),
        })
    }
    .await
    .unwrap_or_else(|error| fail(format!("setup: {error}")));
    Outcome { id, finding: FINDING, mode: Mode::Scripted, verdict }
}

fn chat_replies() -> Vec<Reply> {
    vec![
        Reply::Sse(chat::turn(None, &[("call_badge", "render_badge", json!({}))])),
        Reply::Sse(chat::turn(Some("A blue circle."), &[])),
    ]
}

pub async fn run(keys: &Keys, outcomes: &mut Vec<Outcome>) {
    let verdict = match &keys.anthropic {
        None => Verdict::Skip("ANTHROPIC_API_KEY not set".into()),
        Some(key) => {
            anthropic_live(key).await.unwrap_or_else(|error| fail(format!("setup: {error}")))
        }
    };
    outcomes.push(Outcome { id: "ml15-anthropic", finding: FINDING, mode: Mode::Live, verdict });

    outcomes.push(
        scripted("ml15-groq", chat_replies(), |base| {
            Ok(Arc::new(GroqClient::new(
                GroqConfig::new("test-key", "scripted").with_base_url(base),
            )?))
        })
        .await,
    );
    outcomes.push(
        scripted("ml15-deepseek", chat_replies(), |base| {
            Ok(Arc::new(DeepSeekClient::new(
                DeepSeekConfig::new("test-key", "deepseek-chat").with_base_url(base),
            )?))
        })
        .await,
    );
    outcomes.push(
        scripted("ml15-azure-ai", chat_replies(), |base| {
            Ok(Arc::new(AzureAIClient::new(AzureAIConfig::new(base, "test-key", "scripted"))?))
        })
        .await,
    );
    outcomes.push(
        scripted(
            "ml15-ollama",
            vec![
                Reply::Ndjson(ollama::turn("", &[("render_badge", json!({}))])),
                Reply::Ndjson(ollama::turn("A blue circle.", &[])),
            ],
            |base| Ok(Arc::new(OllamaModel::new(OllamaConfig::with_host(base, "scripted"))?)),
        )
        .await,
    );
}
