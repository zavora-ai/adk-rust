//! ML29: DeepSeek-V3 served without a tool-call parser (vLLM, TGI, Ollama) writes its native
//! tool-call tokens into the text; the client must turn them into a function call.

use std::sync::Arc;

use adk_core::Llm;
use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_model::{OpenAICompatible, OpenAICompatibleConfig};

use crate::common::{
    AgentSpec, FnTool, Harness, Keys, Mode, Outcome, Verdict, fail, pass, short, weather_tool,
};
use crate::upstream::{Reply, Upstream, chat, ollama};

const FINDING: &str = "ML29";

/// One DeepSeek-V3 tool call, as its chat template renders it.
const NATIVE_CALL: &str = "<｜tool▁calls▁begin｜><｜tool▁call▁begin｜>function<｜tool▁sep｜>get_weather\n\
    ```json\n{\"city\": \"Paris\"}\n```<｜tool▁call▁end｜><｜tool▁calls▁end｜>";

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
        let tool = weather_tool();
        let spec = AgentSpec::new("Use get_weather for weather questions.")
            .tool(tool.clone() as Arc<dyn adk_core::Tool>);
        let harness = Harness::new("ml29", build(&upstream.base)?, &spec).await?;
        let turn = harness.turn("What is the weather in Paris?").await;
        Ok::<_, anyhow::Error>(verdict_for(&tool, &turn.answer(), turn.error))
    }
    .await
    .unwrap_or_else(|error| fail(format!("setup: {error}")));
    Outcome { id, finding: FINDING, mode: Mode::Scripted, verdict }
}

fn verdict_for(tool: &FnTool, answer: &str, error: Option<String>) -> Verdict {
    if let Some(error) = error {
        return fail(format!("run failed: {}", short(&error, 160)));
    }
    if tool.executions() == 0 {
        return fail(format!("the tool never ran; answer was {:?}", short(answer, 90)));
    }
    if answer.contains("tool▁") {
        return fail(format!("native tokens leaked into the answer: {}", short(answer, 90)));
    }
    pass(format!("get_weather ran {}x; answer {:?}", tool.executions(), short(answer, 50)))
}

pub async fn run(keys: &Keys, outcomes: &mut Vec<Outcome>) {
    outcomes.push(
        scripted(
            "ml29-compatible",
            vec![
                Reply::Sse(chat::turn(Some(NATIVE_CALL), &[])),
                Reply::Sse(chat::turn(Some("It is 17C and clear in Paris."), &[])),
            ],
            |base| {
                let config = OpenAICompatibleConfig::new("test-key", "deepseek-ai/DeepSeek-V3")
                    .with_base_url(format!("{base}/v1"));
                Ok(Arc::new(OpenAICompatible::new(config)?))
            },
        )
        .await,
    );
    outcomes.push(
        scripted(
            "ml29-ollama",
            vec![
                Reply::Ndjson(ollama::turn(NATIVE_CALL, &[])),
                Reply::Ndjson(ollama::turn("It is 17C and clear in Paris.", &[])),
            ],
            |base| Ok(Arc::new(OllamaModel::new(OllamaConfig::with_host(base, "deepseek-v3"))?)),
        )
        .await,
    );

    // Live happy path for the compatible client: a native tool call through Groq's endpoint.
    let verdict = match &keys.groq {
        None => Verdict::Skip("GROQ_API_KEY not set".into()),
        Some(key) => async {
            let config = OpenAICompatibleConfig::new(key, "openai/gpt-oss-120b")
                .with_provider_name("groq")
                .with_base_url("https://api.groq.com/openai/v1");
            let tool = weather_tool();
            let spec = AgentSpec::new("Use get_weather for weather questions.")
                .tool(tool.clone() as Arc<dyn adk_core::Tool>);
            let harness =
                Harness::new("ml29", Arc::new(OpenAICompatible::new(config)?), &spec).await?;
            let turn = harness.turn("What is the weather in Paris?").await;
            Ok::<_, anyhow::Error>(verdict_for(&tool, &turn.answer(), turn.error))
        }
        .await
        .unwrap_or_else(|error| fail(format!("setup: {error}"))),
    };
    outcomes.push(Outcome {
        id: "ml29-compatible-live",
        finding: FINDING,
        mode: Mode::Live,
        verdict,
    });
}
