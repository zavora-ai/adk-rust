#![cfg(feature = "openai")]

//! Background mode: a `background: true` submission is answered before the generation
//! finishes, so its `queued` or `in_progress` body is the response the caller polls.

use adk_core::{AdkError, Content, GenerateContentConfig, Llm, LlmRequest, LlmResponse, Part};
use adk_model::{
    openai::{OpenAIResponsesClient, OpenAIResponsesConfig},
    retry::RetryConfig,
};
use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn body(id: &str, status: &str, output: Value) -> Value {
    json!({
        "id": id,
        "object": "response",
        "created_at": 1_760_000_000,
        "model": "fixture-model",
        "status": status,
        "background": true,
        "output": output,
        "usage": null,
        "error": null,
        "incomplete_details": null
    })
}

fn message(text: &str) -> Value {
    json!([{"type":"message", "id":"msg", "role":"assistant", "status":"completed",
        "content":[{"type":"output_text", "text":text, "annotations":[]}]}])
}

fn client(server: &MockServer) -> OpenAIResponsesClient {
    OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("fixture-key", "fixture-model").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled())
}

fn request(background: bool) -> LlmRequest {
    let mut config = GenerateContentConfig::default();
    if background {
        config.extensions.insert("openai".to_string(), json!({"background": true}));
    }
    let mut request =
        LlmRequest::new("fixture-model", vec![Content::new("user").with_text("fixture")]);
    request.config = Some(config);
    request
}

async fn submit(
    reply: ResponseTemplate,
    background: bool,
    stream: bool,
) -> Result<Vec<LlmResponse>, AdkError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(reply)
        .expect(1)
        .mount(&server)
        .await;
    let result = client(&server)
        .generate_content(request(background), stream)
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await;
    let sent: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(sent.get("background").and_then(Value::as_bool), background.then_some(true));
    result
}

fn openai(response: &LlmResponse) -> &Value {
    &response.provider_metadata.as_ref().expect("provider metadata")["openai"]
}

#[tokio::test]
async fn background_submission_returns_the_pending_response() {
    for status in ["queued", "in_progress"] {
        let reply = ResponseTemplate::new(200).set_body_json(body("resp_bg", status, json!([])));
        let responses = submit(reply, true, false).await.unwrap();
        assert_eq!(responses.len(), 1);
        let pending = &responses[0];
        assert_eq!(openai(pending)["response_id"], "resp_bg");
        assert_eq!(openai(pending)["status"], status);
        assert_eq!((pending.error_code.as_deref(), pending.turn_complete), (None, false));
        assert!(pending.content.is_none());
    }
}

#[tokio::test]
async fn background_submission_that_finished_is_validated_as_terminal() {
    let reply =
        ResponseTemplate::new(200).set_body_json(body("resp_bg", "completed", message("done")));
    let responses = submit(reply, true, false).await.unwrap();
    let last = responses.last().unwrap();
    assert_eq!(last.content.as_ref().unwrap().parts, vec![Part::Text { text: "done".into() }]);
    assert_eq!(openai(last)["status"], "completed");
    assert!(last.turn_complete);

    let mut failed = body("resp_bg", "failed", json!([]));
    failed["error"] = json!({"code": "server_error", "message": "upstream failure"});
    let error =
        submit(ResponseTemplate::new(200).set_body_json(failed), true, false).await.unwrap_err();
    assert_eq!(error.code, "model.openai_responses.response_failed");
}

#[tokio::test]
async fn background_submission_requires_an_identity() {
    let reply = ResponseTemplate::new(200).set_body_json(body("", "queued", json!([])));
    let error = submit(reply, true, false).await.unwrap_err();
    assert_eq!(error.code, "model.openai_responses.invalid_identity");
}

#[tokio::test]
async fn foreground_and_streaming_requests_still_require_a_terminal_status() {
    for status in ["queued", "in_progress"] {
        let reply = ResponseTemplate::new(200).set_body_json(body("resp_fg", status, json!([])));
        let error = submit(reply, false, false).await.unwrap_err();
        assert_eq!(error.code, "model.openai_responses.invalid_terminal");
    }
    // A streamed background request still has to reach `response.completed`.
    let created =
        json!({"type":"response.created", "response": body("resp_bg", "queued", json!([]))});
    let reply = ResponseTemplate::new(200)
        .set_body_raw(format!("data: {created}\n\n"), "text/event-stream");
    let error = submit(reply, true, true).await.unwrap_err();
    assert_eq!(error.code, "model.openai_responses.unfinished_stream");
}

#[tokio::test]
async fn submitted_response_polls_to_completion() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body(
            "resp_bg",
            "queued",
            json!([]),
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/responses/resp_bg"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body(
            "resp_bg",
            "completed",
            message("done"),
        )))
        .mount(&server)
        .await;
    let client = client(&server);
    let submitted: Vec<LlmResponse> =
        client.generate_content(request(true), false).await.unwrap().try_collect().await.unwrap();
    let id = openai(&submitted[0])["response_id"].as_str().unwrap().to_owned();
    let polled = client.poll_response(&id).await.unwrap();
    assert_eq!(openai(&polled)["status"], "completed");
    assert_eq!(polled.content.unwrap().parts, vec![Part::Text { text: "done".into() }]);
}
