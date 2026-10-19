//! Ollama and OpenRouter send each `GenerateContentConfig` field their API has, return the
//! log probabilities they request, and fail by name on a field they cannot send.
#![cfg(any(feature = "ollama", feature = "openrouter"))]

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, LlmResponse};
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn generate(
    model: &dyn Llm,
    config: GenerateContentConfig,
    stream: bool,
) -> adk_core::Result<Vec<LlmResponse>> {
    let mut request =
        LlmRequest::new(model.name(), vec![Content::new("user").with_text("Say hi.")]);
    request.config = Some(config);
    let mut responses = Vec::new();
    let mut stream = model.generate_content(request, stream).await?;
    while let Some(response) = stream.next().await {
        responses.push(response?);
    }
    Ok(responses)
}

async fn sent_body(server: &MockServer) -> Value {
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    serde_json::from_slice(&received[0].body).unwrap()
}

fn final_logprobs(responses: &[LlmResponse]) -> Value {
    responses
        .iter()
        .rev()
        .find(|response| !response.partial)
        .and_then(|response| response.provider_metadata.as_ref())
        .map(|metadata| metadata["logprobs"].clone())
        .unwrap_or(Value::Null)
}

#[cfg(feature = "ollama")]
mod ollama {
    use super::*;
    use adk_model::ollama::{OllamaConfig, OllamaModel};

    fn every_field() -> GenerateContentConfig {
        GenerateContentConfig {
            temperature: Some(0.5),
            stop_sequences: vec!["END".to_string()],
            seed: Some(1 << 40),
            presence_penalty: Some(0.25),
            frequency_penalty: Some(0.5),
            top_logprobs: Some(2),
            max_output_tokens: Some(64),
            ..Default::default()
        }
    }

    fn ndjson(lines: &[Value]) -> String {
        lines.iter().map(|line| format!("{line}\n")).collect()
    }

    #[tokio::test]
    async fn stop_seed_penalties_and_logprobs_reach_the_request_and_logprobs_return() {
        let server = MockServer::start().await;
        let hi = json!({"token": "Hi", "logprob": -0.01, "bytes": [72, 105],
                        "top_logprobs": [{"token": "Hi", "logprob": -0.01, "bytes": [72, 105]}]});
        // Ollama leaves out empty `bytes`, which `ollama-rs` cannot read.
        let bang = json!({"token": "!", "logprob": -0.2, "top_logprobs": []});
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                ndjson(&[
                    json!({"model": "m", "created_at": "2026-10-19T00:00:00Z",
                           "message": {"role": "assistant", "content": "Hi"}, "done": false,
                           "logprobs": [hi]}),
                    json!({"model": "m", "created_at": "2026-10-19T00:00:00Z",
                           "message": {"role": "assistant", "content": "!"}, "done": false,
                           "logprobs": [bang]}),
                    json!({"model": "m", "created_at": "2026-10-19T00:00:00Z",
                           "message": {"role": "assistant", "content": ""}, "done": true,
                           "done_reason": "stop", "prompt_eval_count": 5, "eval_count": 2}),
                ]),
                "application/x-ndjson",
            ))
            .mount(&server)
            .await;
        let model = OllamaModel::new(OllamaConfig::with_host(server.uri(), "m")).unwrap();

        let responses = generate(&model, every_field(), true).await.unwrap();

        let body = sent_body(&server).await;
        assert_eq!(
            body["options"],
            json!({
                "temperature": 0.5,
                "stop": ["END"],
                "seed": 1_u64 << 40,
                "presence_penalty": 0.25,
                "frequency_penalty": 0.5,
                "num_predict": 64
            })
        );
        assert_eq!((&body["logprobs"], &body["top_logprobs"]), (&json!(true), &json!(2)));
        assert_eq!(final_logprobs(&responses), json!({"content": [hi, bang]}));
    }

    #[tokio::test]
    async fn a_response_cut_at_the_token_cap_finishes_with_max_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "m", "created_at": "2026-10-19T00:00:00Z",
                "message": {"role": "assistant", "content": "Once upon a"}, "done": true,
                "done_reason": "length", "prompt_eval_count": 5, "eval_count": 3
            })))
            .mount(&server)
            .await;
        let model = OllamaModel::new(OllamaConfig::with_host(server.uri(), "m")).unwrap();
        let config = GenerateContentConfig { max_output_tokens: Some(3), ..Default::default() };

        let responses = generate(&model, config, false).await.unwrap();

        assert_eq!(sent_body(&server).await["options"]["num_predict"], json!(3));
        assert_eq!(
            responses.last().and_then(|response| response.finish_reason),
            Some(adk_core::FinishReason::MaxTokens)
        );
    }

    #[tokio::test]
    async fn a_complete_response_returns_its_logprobs() {
        let server = MockServer::start().await;
        let hi = json!({"token": "Hi", "logprob": -0.01, "top_logprobs": []});
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "m", "created_at": "2026-10-19T00:00:00Z",
                "message": {"role": "assistant", "content": "Hi"}, "done": true,
                "done_reason": "stop", "logprobs": [hi]
            })))
            .mount(&server)
            .await;
        let model = OllamaModel::new(OllamaConfig::with_host(server.uri(), "m")).unwrap();

        let responses = generate(
            &model,
            GenerateContentConfig { top_logprobs: Some(1), ..Default::default() },
            false,
        )
        .await
        .unwrap();

        assert_eq!(final_logprobs(&responses), json!({"content": [hi]}));
    }
}

#[cfg(feature = "openrouter")]
mod openrouter {
    use super::*;
    use adk_model::openrouter::{OpenRouterApiMode, OpenRouterClient, OpenRouterConfig};

    fn client(server: &MockServer, mode: OpenRouterApiMode) -> OpenRouterClient {
        OpenRouterClient::new(
            OpenRouterConfig::new("key", "openai/gpt-4.1-mini")
                .with_base_url(format!("{}/api/v1", server.uri()))
                .with_default_api_mode(mode),
        )
        .unwrap()
        .with_retry_config(adk_model::RetryConfig::disabled())
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|event| format!("data: {event}\n\n")).collect::<String>()
            + "data: [DONE]\n\n"
    }

    #[tokio::test]
    async fn responses_fail_on_seed_without_sending() {
        let server = MockServer::start().await;
        let model = client(&server, OpenRouterApiMode::Responses);

        let error =
            generate(&model, GenerateContentConfig { seed: Some(7), ..Default::default() }, true)
                .await
                .unwrap_err();

        assert_eq!(error.code, "model.unsupported_config");
        assert_eq!(error.details.provider.as_deref(), Some("openrouter"));
        assert!(error.message.contains("OpenRouter Responses client does not support `seed`"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn streamed_chat_returns_every_chunks_logprobs() {
        let server = MockServer::start().await;
        let token = |text: &str| json!({"token": text, "logprob": -0.1, "top_logprobs": []});
        let chunk = |content: &str, finish: Value| {
            json!({"id": "gen-1", "object": "chat.completion.chunk", "created": 1,
                   "model": "openai/gpt-4.1-mini",
                   "choices": [{"index": 0, "delta": {"role": "assistant", "content": content},
                                "finish_reason": finish,
                                "logprobs": {"content": [token(content)]}}]})
        };
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                sse(&[chunk("Hi", Value::Null), chunk("!", json!("stop"))]),
                "text/event-stream",
            ))
            .mount(&server)
            .await;
        let model = client(&server, OpenRouterApiMode::ChatCompletions);

        let responses = generate(
            &model,
            GenerateContentConfig { top_logprobs: Some(2), ..Default::default() },
            true,
        )
        .await
        .unwrap();

        assert_eq!(sent_body(&server).await["top_logprobs"], 2);
        assert_eq!(final_logprobs(&responses), json!({"content": [token("Hi"), token("!")]}));
    }

    #[tokio::test]
    async fn responses_return_output_text_logprobs() {
        let server = MockServer::start().await;
        let hi = json!({"token": "Hi", "logprob": -0.01, "bytes": [72, 105], "top_logprobs": []});
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp-1", "object": "response", "status": "completed",
                "model": "openai/gpt-4.1-mini",
                "output": [{"type": "message", "id": "msg-1", "role": "assistant",
                            "status": "completed",
                            "content": [{"type": "output_text", "text": "Hi", "annotations": [],
                                         "logprobs": [hi]}]}]
            })))
            .mount(&server)
            .await;
        let model = client(&server, OpenRouterApiMode::Responses);

        let responses = generate(
            &model,
            GenerateContentConfig { top_logprobs: Some(2), ..Default::default() },
            false,
        )
        .await
        .unwrap();

        assert_eq!(sent_body(&server).await["top_logprobs"], 2);
        assert_eq!(final_logprobs(&responses), json!({"content": [hi]}));
    }
}
