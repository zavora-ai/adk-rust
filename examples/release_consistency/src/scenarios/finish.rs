//! Item 4: an unknown finish reason maps to `FinishReason::Other`, not `Stop`.

use std::sync::Arc;

use adk_core::{FinishReason, Llm};
use adk_model::{DeepSeekClient, DeepSeekConfig, GroqClient, GroqConfig, RetryConfig};
use serde_json::{Value, json};

use crate::common::{Mode, Report, Verdict, agent_turn, check, describe, direct_call, fail};
use crate::upstream::{Reply, Upstream, sse};

/// DeepSeek documents this finish reason; Groq's is a value the client does not know.
const DEEPSEEK_REASON: &str = "insufficient_system_resource";
const GROQ_REASON: &str = "model_context_window_exceeded";

fn completion(finish_reason: &str) -> Value {
    json!({
        "id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": "scripted",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Partial answer"},
                     "finish_reason": finish_reason}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
    })
}

fn chunks(finish_reason: &str) -> Vec<String> {
    vec![
        sse(&json!({
            "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "scripted",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Partial answer"}, "finish_reason": null}]
        })),
        sse(&json!({
            "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "scripted",
            "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
        })),
        "data: [DONE]\n\n".to_string(),
    ]
}

fn groq(base: &str) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(
        GroqClient::new(
            GroqConfig::new("test-key", "llama-3.3-70b-versatile").with_base_url(base),
        )?
        .with_retry_config(RetryConfig::disabled()),
    ))
}

fn deepseek(base: &str) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(
        DeepSeekClient::new(
            DeepSeekConfig::new("test-key", "deepseek-v4-flash").with_base_url(base),
        )?
        .with_retry_config(RetryConfig::disabled()),
    ))
}

pub async fn run(report: &mut Report) {
    report.record_result(
        "finish-groq-complete",
        4,
        Mode::Scripted,
        complete(groq, GROQ_REASON).await,
    );
    report.record_result("finish-deepseek-stream", 4, Mode::Scripted, streamed().await);
    report.record_result(
        "finish-deepseek-complete",
        4,
        Mode::Scripted,
        complete(deepseek, DEEPSEEK_REASON).await,
    );
}

fn verdict(reason: Option<FinishReason>, sent: &str) -> Verdict {
    check(reason == Some(FinishReason::Other), format!("finish_reason {sent:?} -> {reason:?}"))
}

/// A complete (non-streamed) response, through `Llm::generate_content`.
async fn complete(
    build: fn(&str) -> anyhow::Result<Arc<dyn Llm>>,
    reason: &str,
) -> anyhow::Result<Verdict> {
    let upstream = Upstream::start().await?;
    upstream.push(Reply::Json(200, completion(reason)));
    let model = build(&upstream.base)?;
    let (responses, error) = direct_call(model.as_ref(), false).await;
    if let Some(error) = error {
        return Ok(fail(format!("call failed: {}", describe(&error))));
    }
    let finish = responses.iter().rev().find_map(|response| response.finish_reason);
    Ok(verdict(finish, reason))
}

/// A streamed DeepSeek response, through `LlmAgent`.
async fn streamed() -> anyhow::Result<Verdict> {
    let upstream = Upstream::start().await?;
    upstream.push(Reply::Sse(chunks(DEEPSEEK_REASON)));
    let turn = agent_turn(deepseek(&upstream.base)?, None, "Say hi.").await?;
    if let Some(error) = &turn.error {
        return Ok(fail(format!("turn failed: {}", describe(error))));
    }
    let finish = turn.events.iter().rev().find_map(|event| event.llm_response.finish_reason);
    Ok(verdict(finish, DEEPSEEK_REASON))
}
