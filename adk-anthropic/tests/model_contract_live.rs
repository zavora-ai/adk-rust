//! Live checks of Claude Sonnet 5.5's request contract: a forced `tool_choice` and disabled
//! thinking are rejected, and `between_tools` turns thinking off. Require `ANTHROPIC_API_KEY`.

use adk_anthropic::{
    Anthropic, EffortLevel, MessageCreateParams, MessageParam, Model, OutputConfig, ThinkingConfig,
    ToolChoice, ToolUnionParam,
};

fn client() -> Option<Anthropic> {
    std::env::var("ANTHROPIC_API_KEY").ok().map(|key| Anthropic::new(Some(key)).unwrap())
}

fn params() -> MessageCreateParams {
    MessageCreateParams::simple(
        MessageParam::user("Reply with the single word: ok"),
        Model::Custom("claude-sonnet-5-5".to_string()),
    )
}

#[tokio::test]
#[ignore = "calls the Anthropic API; requires ANTHROPIC_API_KEY"]
async fn live_sonnet_5_5_turns_thinking_off_only_between_tools() {
    let Some(client) = client() else { return };
    let weather: ToolUnionParam = serde_json::from_value(serde_json::json!({
        "type": "custom",
        "name": "get_weather",
        "description": "Get the weather for a city",
        "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
    }))
    .unwrap();
    let mut forced = params();
    forced.tools = Some(vec![weather]);
    forced.tool_choice = Some(ToolChoice::any());
    let mut disabled = params();
    disabled.thinking = Some(ThinkingConfig::disabled());
    for (name, request) in [("forced tool_choice", forced), ("disabled thinking", disabled)] {
        let error = client.send(request).await.expect_err(name);
        println!("{name}: {error}");
    }

    let mut between_tools = params();
    between_tools.max_tokens = 32;
    between_tools.thinking = Some(ThinkingConfig::between_tools());
    between_tools.output_config = Some(OutputConfig::with_effort(EffortLevel::Low));
    let message = client.send(between_tools).await.expect("between_tools is accepted");
    println!("between_tools: stop_reason={:?}", message.stop_reason);
}
