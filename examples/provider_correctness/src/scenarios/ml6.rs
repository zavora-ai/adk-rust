//! ML6: an `output_schema` agent with tools on the OpenAI Responses API must send the schema
//! as `text.format` `json_schema`.

use std::sync::Arc;

use adk_core::Llm;
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_model::{OpenAIClient, OpenAIConfig};
use serde_json::{Value, json};

use crate::common::{
    AgentSpec, Harness, Keys, Mode, Outcome, Verdict, fail, pass, short, weather_tool,
};
use crate::upstream::Proxy;

const FINDING: &str = "ML6";

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "city": {"type": "string"},
            "temperature_c": {"type": "number"},
            "summary": {"type": "string"}
        },
        "required": ["city", "temperature_c", "summary"],
        "additionalProperties": false
    })
}

async fn structured_with_tools(model: Arc<dyn Llm>, proxy: &Proxy) -> anyhow::Result<Verdict> {
    let spec = AgentSpec::new("You report the weather. Always call get_weather first.")
        .tool(weather_tool())
        .output_schema(schema());
    let harness = Harness::new("ml6", model, &spec).await?;
    let turn = harness.turn("What is the weather in Paris?").await;
    let requests: Vec<Value> = proxy
        .captured()
        .into_iter()
        .filter(|request| request.path.ends_with("/responses"))
        .map(|request| request.body)
        .collect();
    if requests.is_empty() {
        return Ok(fail(format!("no Responses request was sent (error: {:?})", turn.error)));
    }
    let unconstrained = requests
        .iter()
        .filter(|body| {
            body.pointer("/text/format/type") != Some(&json!("json_schema"))
                || body.pointer("/text/format/schema/properties/temperature_c").is_none()
        })
        .count();
    if unconstrained > 0 {
        return Ok(fail(format!(
            "{unconstrained} of {} Responses requests carried no text.format json_schema (text: {})",
            requests.len(),
            requests[0].get("text").map(Value::to_string).unwrap_or_else(|| "absent".into())
        )));
    }
    if let Some(error) = turn.error {
        return Ok(fail(format!("run failed: {}", short(&error, 200))));
    }
    let answer = turn.answer();
    let parsed: Value = match serde_json::from_str(answer.trim()) {
        Ok(value) => value,
        Err(error) => {
            return Ok(fail(format!("answer is not JSON ({error}): {}", short(&answer, 120))));
        }
    };
    if parsed["city"].is_string() && parsed["temperature_c"].is_number() {
        Ok(pass(format!("{} requests constrained; answer {}", requests.len(), short(&answer, 80))))
    } else {
        Ok(fail(format!("answer does not match the schema: {}", short(&answer, 120))))
    }
}

pub async fn run(keys: &Keys, outcomes: &mut Vec<Outcome>) {
    let Some(key) = &keys.openai else {
        for id in ["ml6-responses", "ml6-openai-routed"] {
            outcomes.push(Outcome {
                id,
                finding: FINDING,
                mode: Mode::Live,
                verdict: Verdict::Skip("OPENAI_API_KEY not set".into()),
            });
        }
        return;
    };

    let verdict = async {
        let proxy = Proxy::start("https://api.openai.com").await?;
        let config = OpenAIResponsesConfig::new(key, "gpt-5.6-terra")
            .with_base_url(format!("{}/v1", proxy.base));
        structured_with_tools(Arc::new(OpenAIResponsesClient::new(config)?), &proxy).await
    }
    .await
    .unwrap_or_else(|error| fail(format!("setup: {error}")));
    outcomes.push(Outcome { id: "ml6-responses", finding: FINDING, mode: Mode::Live, verdict });

    // OpenAIClient sends a GPT-5.6 request that declares tools through the Responses API.
    let verdict = async {
        let proxy = Proxy::start("https://api.openai.com").await?;
        let mut config = OpenAIConfig::new(key, "gpt-5.6-terra");
        config.base_url = Some(format!("{}/v1", proxy.base));
        structured_with_tools(Arc::new(OpenAIClient::new(config)?), &proxy).await
    }
    .await
    .unwrap_or_else(|error| fail(format!("setup: {error}")));
    outcomes.push(Outcome { id: "ml6-openai-routed", finding: FINDING, mode: Mode::Live, verdict });
}
