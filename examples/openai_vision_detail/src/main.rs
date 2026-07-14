//! Validation example for issue #395.
//!
//! Background: `async-openai` 0.33 serializes `ImageUrl.detail` even when it is
//! `None`, producing an explicit `"detail": null`. The official OpenAI API
//! tolerates that, but stricter OpenAI-compatible gateways validate `detail`
//! against the literal set `{auto, low, high}` and reject `null` with HTTP 400.
//! The fix makes `adk-model` emit the API default `"auto"` instead.
//!
//! This example proves the fix end-to-end, fully offline:
//!
//! 1. It starts a mock "strict gateway" on localhost that rejects any request
//!    whose `image_url.detail` is `null` (mirroring the real 400) and accepts
//!    `auto`/`low`/`high`.
//! 2. It points the real `OpenAIClient` at that gateway and sends a vision
//!    request containing both an inline image and a URL image.
//! 3. The gateway reports the `detail` values it actually received. With the
//!    fix, they are `"auto"` and the request succeeds; without it, the gateway
//!    would have rejected the `null`.
//!
//! Run with: `cargo run -p openai-vision-detail`

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::OpenAIClient;
use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use futures::StreamExt;

/// Shared record of the `detail` values the gateway observed on image parts.
type SeenDetails = Arc<Mutex<Vec<serde_json::Value>>>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── 1. Start the mock strict gateway ────────────────────────────────
    let seen: SeenDetails = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;

    let app = Router::new()
        // async-openai posts to `{base}/chat/completions`; accept any path.
        .fallback(post(chat_completions))
        .with_state(seen.clone());

    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock gateway serves");
    });

    // ── 2. Point the real OpenAIClient at the mock gateway ──────────────
    let base_url = format!("http://{addr}/v1");
    let client = OpenAIClient::compatible("test-key", base_url, "gpt-4o-mini")
        .map_err(|e| anyhow::anyhow!("failed to build client: {e}"))?;

    // A vision request that exercises both image conversion paths:
    //   - InlineData  -> data: URI image
    //   - FileData    -> https URL image
    let request = LlmRequest {
        model: "gpt-4o-mini".to_string(),
        contents: vec![Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "What is in these images?".to_string() },
                Part::InlineData {
                    mime_type: "image/png".to_string(),
                    data: vec![0x89, 0x50, 0x4E, 0x47],
                },
                Part::FileData {
                    mime_type: "image/jpeg".to_string(),
                    file_uri: "https://example.com/photo.jpg".to_string(),
                },
            ],
        }],
        config: None,
        tools: Default::default(),
        previous_response_id: None,
    };

    // ── 3. Send it and observe the outcome ──────────────────────────────
    println!("sending vision request to strict gateway at {addr} ...\n");
    let mut stream = client
        .generate_content(request, false)
        .await
        .map_err(|e| anyhow::anyhow!("request rejected by gateway: {e}"))?;

    let mut reply = String::new();
    while let Some(chunk) = stream.next().await {
        let resp = chunk.map_err(|e| anyhow::anyhow!("gateway returned an error: {e}"))?;
        if let Some(content) = resp.content {
            for part in content.parts {
                if let Part::Text { text } = part {
                    reply.push_str(&text);
                }
            }
        }
    }

    // ── 4. Report what the gateway saw ──────────────────────────────────
    let details = seen.lock().unwrap().clone();
    println!("gateway accepted the request ✅");
    println!("image_url.detail values received: {details:?}");
    println!("model reply: {reply}\n");

    // Assert the fix: every image carried an explicit, valid detail — never null.
    assert!(!details.is_empty(), "gateway saw no image parts");
    for d in &details {
        assert!(
            d.as_str() == Some("auto"),
            "expected detail \"auto\", got {d:?} (issue #395 regression)"
        );
    }
    println!("PASS: no `\"detail\": null` was sent; strict gateway accepted the vision request.");

    server.abort();
    Ok(())
}

/// Mock strict-gateway handler. Emulates an OpenAI-compatible endpoint that
/// validates `image_url.detail` against `{auto, low, high}` and rejects `null`.
async fn chat_completions(
    State(seen): State<SeenDetails>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")).into_response(),
    };

    // Walk messages[].content[] looking for image parts and record their detail.
    let mut recorded = seen.lock().unwrap();
    if let Some(messages) = payload.get("messages").and_then(|m| m.as_array()) {
        for message in messages {
            let Some(parts) = message.get("content").and_then(|c| c.as_array()) else {
                continue;
            };
            for part in parts {
                let Some(image_url) = part.get("image_url") else {
                    continue;
                };
                let detail = image_url.get("detail").cloned().unwrap_or(serde_json::Value::Null);
                recorded.push(detail.clone());

                // Strict validation: reject an explicit null (the issue #395 bug).
                let valid = matches!(detail.as_str(), Some("auto" | "low" | "high"));
                if !valid {
                    let err = serde_json::json!({
                        "error": {
                            "message": "image_url.detail: Input should be 'auto', 'low' or 'high'",
                            "type": "invalid_request_error",
                            "param": "image_url.detail",
                            "code": "literal_error"
                        }
                    });
                    return (StatusCode::BAD_REQUEST, axum::Json(err)).into_response();
                }
            }
        }
    }

    // Valid request -> return a minimal, well-formed chat completion.
    let response = serde_json::json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "created": 0,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "ok: received 2 image(s)" },
            "finish_reason": "stop"
        }]
    });
    axum::Json(response).into_response()
}
