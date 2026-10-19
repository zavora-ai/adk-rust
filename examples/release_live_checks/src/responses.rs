//! Item 1: OpenAI Responses background mode.
//!
//! A background submission returns `queued` or `in_progress`; the client must hand back the
//! response ID so the caller can poll, while a foreground request that ends without a terminal
//! status still fails.

use std::time::{Duration, Instant};

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, LlmResponse, Part};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use axum::{Json, Router, routing::post};
use futures::StreamExt;
use serde_json::{Value, json};

use crate::report::{Mode, Report, Verdict, check, fail, key, skip};

const MODEL: &str = "gpt-5.6-terra";

pub async fn run(report: &mut Report) {
    let upstream = scripted_upstream().await;
    match upstream {
        Ok(base_url) => {
            report.record_result(
                "responses_background_pending",
                1,
                Mode::Scripted,
                background_pending(&base_url).await,
            );
            report.record_result(
                "responses_foreground_strict",
                1,
                Mode::Scripted,
                foreground_strict(&base_url).await,
            );
        }
        Err(error) => report.record(
            "responses_background_pending",
            1,
            Mode::Scripted,
            fail(format!("upstream: {error:#}")),
        ),
    }
    let verdict = match key("OPENAI_API_KEY") {
        Some(api_key) => background_live(&api_key).await,
        None => Ok(skip("OPENAI_API_KEY is not set")),
    };
    report.record_result("responses_background_live", 1, Mode::Live, verdict);
}

/// A local `/v1/responses` that answers a background request `queued` and a foreground request
/// with a non-terminal `in_progress` body.
async fn scripted_upstream() -> anyhow::Result<String> {
    async fn create(Json(body): Json<Value>) -> Json<Value> {
        let background = body.get("background").and_then(Value::as_bool) == Some(true);
        let status = if background { "queued" } else { "in_progress" };
        Json(json!({
            "id": "resp_scripted",
            "object": "response",
            "created_at": 1_760_000_000,
            "model": MODEL,
            "status": status,
            "background": background,
            "output": [],
            "usage": null,
            "error": null,
            "incomplete_details": null
        }))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = Router::new().route("/v1/responses", post(create));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{address}/v1"))
}

fn request(background: bool, text: &str) -> LlmRequest {
    let mut config = GenerateContentConfig::default();
    if background {
        config.extensions.insert("openai".to_string(), json!({ "background": true }));
    }
    let mut request = LlmRequest::new(MODEL, vec![Content::new("user").with_text(text)]);
    request.config = Some(config);
    request
}

async fn submit(client: &OpenAIResponsesClient, request: LlmRequest) -> adk_core::Result<LlmResponse> {
    let mut stream = client.generate_content(request, false).await?;
    match stream.next().await {
        Some(item) => item,
        None => Err(adk_core::AdkError::model("the response stream was empty")),
    }
}

fn status(response: &LlmResponse) -> Option<&str> {
    response.provider_metadata.as_ref()?.get("openai")?.get("status")?.as_str()
}

fn response_id(response: &LlmResponse) -> Option<&str> {
    response.provider_metadata.as_ref()?.get("openai")?.get("response_id")?.as_str()
}

fn text(response: &LlmResponse) -> String {
    response
        .content
        .as_ref()
        .map(|content| content.parts.iter().filter_map(Part::text).collect())
        .unwrap_or_default()
}

async fn background_pending(base_url: &str) -> anyhow::Result<Verdict> {
    let client = OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("sk-scripted", MODEL).with_base_url(base_url),
    )?;
    Ok(match submit(&client, request(true, "Say hi.")).await {
        Ok(response) => check(
            status(&response) == Some("queued")
                && response_id(&response) == Some("resp_scripted")
                && response.error_code.is_none()
                && !response.turn_complete,
            format!(
                "status={:?} id={:?} error_code={:?} turn_complete={}",
                status(&response),
                response_id(&response),
                response.error_code,
                response.turn_complete
            ),
        ),
        Err(error) => fail(format!("{}: {}", error.code, error.message)),
    })
}

async fn foreground_strict(base_url: &str) -> anyhow::Result<Verdict> {
    let client = OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("sk-scripted", MODEL).with_base_url(base_url),
    )?;
    Ok(match submit(&client, request(false, "Say hi.")).await {
        Ok(response) => fail(format!(
            "a foreground in_progress body was accepted: status={:?}",
            status(&response)
        )),
        Err(error) => check(
            error.code == "model.openai_responses.invalid_terminal",
            format!("rejected with {}", error.code),
        ),
    })
}

async fn background_live(api_key: &str) -> anyhow::Result<Verdict> {
    let client = OpenAIResponsesClient::new(OpenAIResponsesConfig::new(api_key, MODEL))?;
    let submitted = match submit(&client, request(true, "Reply with exactly: background ok")).await
    {
        Ok(response) => response,
        Err(error) => return Ok(fail(format!("submit: {}: {}", error.code, error.message))),
    };
    let Some(id) = response_id(&submitted).map(str::to_owned) else {
        return Ok(fail("the submission carried no response_id"));
    };
    let first = status(&submitted).unwrap_or("unknown").to_owned();
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut polls = 0;
    let mut latest = submitted;
    while status(&latest) != Some("completed") {
        if Instant::now() > deadline {
            return Ok(fail(format!("still {:?} after {polls} polls", status(&latest))));
        }
        if matches!(status(&latest), Some("failed" | "cancelled" | "incomplete")) {
            return Ok(fail(format!("ended {:?}: {:?}", status(&latest), latest.error_message)));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        polls += 1;
        latest = client.poll_response(&id).await?;
    }
    let answer = text(&latest);
    Ok(check(
        !answer.trim().is_empty(),
        format!("submitted {first}, completed after {polls} polls: {answer:?}"),
    ))
}
