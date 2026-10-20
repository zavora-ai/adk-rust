//! `GeminiModel` sends every `GenerateContentConfig` field Gemini's `generationConfig` has,
//! and returns requested log probabilities in `provider_metadata["logprobs"]`.

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, LlmResponse};
use adk_model::GeminiModel;
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const MODEL: &str = "gemini-3.5-flash-lite";

async fn generate(server: &MockServer, config: GenerateContentConfig) -> LlmResponse {
    let model =
        GeminiModel::new_with_base_url("test-key", MODEL, format!("{}/v1beta/", server.uri()))
            .unwrap();
    let mut request = LlmRequest::new(MODEL, vec![Content::new("user").with_text("Say hi.")]);
    request.config = Some(config);
    let mut stream = model.generate_content(request, false).await.unwrap();
    let mut last = None;
    while let Some(response) = stream.next().await {
        last = Some(response.unwrap());
    }
    last.expect("one response")
}

async fn sent_generation_config(server: &MockServer) -> Value {
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    body["generationConfig"].clone()
}

#[tokio::test]
async fn stop_sequences_seed_penalties_and_logprobs_reach_generation_config() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "Hi"}]},
                "finishReason": "STOP",
                "logprobsResult": {
                    "topCandidates": [{"candidates": [
                        {"token": "Hi", "tokenId": 17, "logProbability": -0.01},
                        {"token": "Hello", "tokenId": 18, "logProbability": -4.6}
                    ]}],
                    "chosenCandidates": [{"token": "Hi", "tokenId": 17, "logProbability": -0.01}],
                    "logProbabilitySum": -0.01
                },
                "avgLogprobs": -0.01
            }],
            "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 1, "totalTokenCount": 4}
        })))
        .mount(&server)
        .await;

    let response = generate(
        &server,
        GenerateContentConfig {
            stop_sequences: vec!["END".to_string()],
            seed: Some(7),
            presence_penalty: Some(0.25),
            frequency_penalty: Some(0.5),
            top_logprobs: Some(2),
            ..Default::default()
        },
    )
    .await;

    assert_eq!(
        sent_generation_config(&server).await,
        json!({
            "stopSequences": ["END"],
            "seed": 7,
            "presencePenalty": 0.25,
            "frequencyPenalty": 0.5,
            "responseLogprobs": true,
            "logprobs": 2
        })
    );
    assert_eq!(
        response.provider_metadata.unwrap()["logprobs"],
        json!({
            "topCandidates": [{"candidates": [
                {"token": "Hi", "tokenId": 17, "logProbability": -0.01},
                {"token": "Hello", "tokenId": 18, "logProbability": -4.6}
            ]}],
            "chosenCandidates": [{"token": "Hi", "tokenId": 17, "logProbability": -0.01}],
            "logProbabilitySum": -0.01
        })
    );
}

#[tokio::test]
async fn unset_fields_are_left_out_and_logprobs_sit_beside_grounding() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "Hi"}]},
                "finishReason": "STOP",
                "groundingMetadata": {"webSearchQueries": ["greeting"]},
                "logprobsResult": {"chosenCandidates": [{"token": "Hi", "logProbability": -0.01}]}
            }]
        })))
        .mount(&server)
        .await;

    let response =
        generate(&server, GenerateContentConfig { temperature: Some(0.2), ..Default::default() })
            .await;

    assert_eq!(sent_generation_config(&server).await, json!({"temperature": 0.2}));
    let metadata = response.provider_metadata.unwrap();
    assert_eq!(metadata["webSearchQueries"], json!(["greeting"]));
    assert_eq!(metadata["logprobs"]["chosenCandidates"][0]["token"], "Hi");
}

#[tokio::test]
async fn response_modalities_reach_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{"content": {"role": "model", "parts": [
                {"text": "Here is the image"},
                {"inlineData": {"mimeType": "image/png", "data": "aW1hZ2U="}}
            ]}, "finishReason": "STOP"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let response = generate(
        &server,
        GenerateContentConfig {
            response_modalities: Some(vec!["TEXT".into(), "IMAGE".into()]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        sent_generation_config(&server).await["responseModalities"],
        json!(["TEXT", "IMAGE"])
    );
    let parts = response.content.unwrap().parts;
    assert!(matches!(&parts[0], adk_core::Part::Text { text, .. } if text == "Here is the image"));
    assert!(
        matches!(&parts[1], adk_core::Part::InlineData { mime_type, data, .. } if mime_type == "image/png" && data == b"image")
    );
}
