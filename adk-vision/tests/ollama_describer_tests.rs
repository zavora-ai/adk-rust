//! A vision model on Ollama as a second local `Describer` route (spec 3.7 §10 X19):
//! `LlmDescriber::local` over adk-model's `OllamaModel` at a loopback address. The image goes
//! out as the message's native `images`, never as text, and nothing passes the egress gate,
//! since the model runs on this machine.
//!
//! `tests/fixtures/ollama/qwen3-vl-4b-shapes.json` is `qwen3-vl:4b`'s `/api/chat` reply on
//! Ollama 0.32.13 (2026-11-22) to the same scene — a red and a blue box on white, 640 × 480 —
//! and question. The live run is ignored by default:
//!
//! ```text
//! ADK_VISION_OLLAMA_MODEL=qwen3-vl:4b cargo nextest run -p adk-vision --features cloud \
//!     --test ollama_describer_tests --run-ignored only --no-capture
//! ```
//!
//! `OLLAMA_HOST` names another server (default `http://127.0.0.1:11434`).

#![cfg(feature = "cloud")]

use std::sync::Arc;

use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_vision::conformance::Fixture;
use adk_vision::{
    DecodeLimits, DecodedImage, DescribeSpec, Describer, LlmDescriber, Locality, VisionCx,
    VisionRuntime,
};
use base64::Engine as _;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const QUESTION: &str = "What coloured shapes are on this image? Answer in one sentence.";

fn describer(host: &str, model: &str) -> LlmDescriber<adk_vision::SentImage> {
    let ollama = OllamaModel::new(OllamaConfig::with_host(host, model)).unwrap();
    // A thinking model reasons before it answers: room for both.
    LlmDescriber::local(Arc::new(ollama)).unwrap().with_max_output_tokens(1_500)
}

#[tokio::test]
async fn an_ollama_model_describes_an_image_it_receives_natively() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            include_str!("fixtures/ollama/qwen3-vl-4b-shapes.json"),
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let describer = describer(&server.uri(), "qwen3-vl:4b");
    assert_eq!(describer.descriptor().locality, Locality::Loopback, "a model on this machine");
    assert_eq!(describer.descriptor().price, None, "never priced");
    let runtime = VisionRuntime::builder().describer(Arc::new(describer)).build().unwrap();
    let fixture = Fixture::shapes();

    let out = runtime
        .describe(&VisionCx::new(), &[&fixture.image], &DescribeSpec::new(QUESTION))
        .await
        .unwrap();
    assert_eq!(
        out.value.text.as_deref(),
        Some("The colored shapes in the image are red and blue.")
    );

    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert_eq!(message["role"], "user");
    let content = message["content"].as_str().unwrap();
    assert!(content.starts_with(QUESTION), "{content}");
    assert!(!content.contains("<attachment"), "the image is not text: {content}");
    let images = message["images"].as_array().unwrap();
    assert_eq!(images.len(), 1, "one native image");
    let bytes =
        base64::engine::general_purpose::STANDARD.decode(images[0].as_str().unwrap()).unwrap();
    let (sent, _) = DecodedImage::decode(&bytes, &DecodeLimits::default()).unwrap();
    assert_eq!(sent.size(), (640, 480));
    assert_eq!(sent.pixel(180, 140), [255, 0, 0, 255], "the red box");
    assert_eq!(sent.pixel(470, 340), [0, 0, 255, 255], "the blue box");
    assert_eq!(body["options"]["num_predict"], 1_500);
}

#[tokio::test]
#[ignore = "needs Ollama with a vision model; see the module docs"]
async fn a_live_ollama_model_names_the_shapes() {
    let host = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
    let model = std::env::var("ADK_VISION_OLLAMA_MODEL").unwrap_or_else(|_| "qwen3-vl:4b".into());
    let runtime =
        VisionRuntime::builder().describer(Arc::new(describer(&host, &model))).build().unwrap();
    let fixture = Fixture::shapes();
    let started = std::time::Instant::now();
    let out = runtime
        .describe(&VisionCx::new(), &[&fixture.image], &DescribeSpec::new(QUESTION))
        .await
        .unwrap();
    let text = out.value.text.unwrap_or_default();
    eprintln!("{model} in {:?}: {text}", started.elapsed());
    let text = text.to_lowercase();
    assert!(text.contains("red") && text.contains("blue"), "{text}");
}
