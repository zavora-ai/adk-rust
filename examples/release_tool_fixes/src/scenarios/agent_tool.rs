//! Item 3: `AgentTool` reports a delegate's failure without its message by default.
//! Item 10: `AgentTool` returns the answer of a delegate that has an `output_key`.

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use adk_tool::AgentTool;
use serde_json::json;

use crate::common::{
    LIVE_MODEL, Mode, Report, Verdict, record_function_responses, run_turn, skip, verdict,
};
use crate::upstream::{Reply, Upstream, tool_messages};

const NO_RESPONSE: &str = "No response from agent";

/// A credential the delegate's upstream quotes in its error body.
const SECRET: &str = "postgres://billing:hunter2@db.internal:5432/prod";

pub async fn run(report: &mut Report) {
    report.record_result("agent-tool-error-is-redacted", 3, Mode::Scripted, redacted().await);
}

pub async fn run_output_key(gemini_key: Option<&str>, report: &mut Report) {
    report.record_result(
        "agent-tool-output-key-answer",
        10,
        Mode::Scripted,
        output_key_scripted().await,
    );
    match gemini_key {
        Some(key) => report.record_result(
            "agent-tool-output-key-live",
            10,
            Mode::Live,
            output_key_live(key).await,
        ),
        None => report.record("agent-tool-output-key-live", 10, Mode::Live, skip("no Gemini key")),
    }
}

/// A delegate with an `output_key` still answers the coordinator through the tool.
async fn output_key_scripted() -> anyhow::Result<Verdict> {
    let delegate_upstream = Upstream::start(vec![Reply::Text("Your balance is 42 EUR.")]).await?;
    let delegate = LlmAgentBuilder::new("billing")
        .description("Answers billing questions.")
        .instruction("Answer billing questions.")
        .model(Arc::new(delegate_upstream.client()?))
        .output_key("billing_answer")
        .build()?;
    let coordinator_upstream = Upstream::start(vec![
        Reply::ToolCall { name: "billing", args: json!({"request": "What is my balance?"}) },
        Reply::Text("Your balance is 42 EUR."),
    ])
    .await?;
    let coordinator = LlmAgentBuilder::new("coordinator")
        .instruction("Delegate billing questions to the billing tool.")
        .model(Arc::new(coordinator_upstream.client()?))
        .tool(Arc::new(AgentTool::new(Arc::new(delegate))))
        .build()?;

    let turn = run_turn(Arc::new(coordinator), "What is my balance?").await?;
    let captured = coordinator_upstream.captured();
    let tool_text = captured.get(1).map(tool_messages).unwrap_or_default();
    Ok(verdict(vec![
        (turn.error.is_none(), format!("run error: {:?}", turn.error)),
        (tool_text.contains("42 EUR"), format!("tool message: {tool_text}")),
    ]))
}

/// The same on live Gemini: the coordinator's model receives the delegate's answer.
async fn output_key_live(key: &str) -> anyhow::Result<Verdict> {
    let delegate = LlmAgentBuilder::new("capitals")
        .description("Knows the capital city of every country.")
        .instruction("Answer with the capital city only.")
        .model(Arc::new(GeminiModel::new(key, LIVE_MODEL)?))
        .output_key("capital")
        .build()?;
    let (callback, seen) = record_function_responses();
    let coordinator = LlmAgentBuilder::new("coordinator")
        .instruction(
            "Ask the capitals tool for the capital of the country the user names, then reply \
             with its answer.",
        )
        .model(Arc::new(GeminiModel::new(key, LIVE_MODEL)?))
        .tool(Arc::new(AgentTool::new(Arc::new(delegate))))
        .before_model_callback(callback)
        .build()?;

    let turn = run_turn(Arc::new(coordinator), "What is the capital of Kenya?").await?;
    let responses: Vec<String> = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|(name, _)| name == "capitals")
        .map(|(_, response)| response.to_string())
        .collect();
    Ok(verdict(vec![
        (turn.error.is_none(), format!("run error: {:?}", turn.error)),
        (!responses.is_empty(), format!("{} capitals responses", responses.len())),
        (
            responses.iter().all(|response| !response.contains(NO_RESPONSE))
                && responses.iter().any(|response| response.contains("Nairobi")),
            format!("tool responses: {responses:?}"),
        ),
        (turn.answer().contains("Nairobi"), format!("answer: {}", turn.answer().trim())),
    ]))
}

/// The coordinator's model never receives the delegate's error message.
async fn redacted() -> anyhow::Result<Verdict> {
    // The delegate's provider rejects the request and echoes a connection string.
    let delegate_upstream = Upstream::start(vec![Reply::Error(
        400,
        json!({"error": {"message": format!("query failed against {SECRET}"),
                         "type": "invalid_request_error"}}),
    )])
    .await?;
    let delegate = LlmAgentBuilder::new("billing")
        .description("Answers billing questions.")
        .instruction("Answer billing questions.")
        .model(Arc::new(delegate_upstream.client()?))
        .build()?;

    let coordinator_upstream = Upstream::start(vec![
        Reply::ToolCall { name: "billing", args: json!({"request": "What is my balance?"}) },
        Reply::Text("The billing agent is unavailable."),
    ])
    .await?;
    let coordinator = LlmAgentBuilder::new("coordinator")
        .instruction("Delegate billing questions to the billing tool.")
        .model(Arc::new(coordinator_upstream.client()?))
        .tool(Arc::new(AgentTool::new(Arc::new(delegate))))
        .build()?;

    let turn = run_turn(Arc::new(coordinator), "What is my balance?").await?;
    let captured = coordinator_upstream.captured();
    let tool_text = captured.get(1).map(tool_messages).unwrap_or_default();
    Ok(verdict(vec![
        (turn.error.is_none(), format!("run error: {:?}", turn.error)),
        (delegate_upstream.captured().len() == 1, "the delegate called its model".to_string()),
        (!tool_text.is_empty(), "the coordinator's model received the tool result".to_string()),
        (!tool_text.contains("hunter2"), format!("tool message: {tool_text}")),
    ]))
}
