//! Gemini native video (spec 3.7 X8, X9): a user `FileData` part goes out as `fileData`
//! with its `videoMetadata`, the media resolution reaches `generationConfig`, and the File
//! API uploads once and reports when a video is ready.

use std::time::Duration;

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, Part};
use adk_model::GeminiModel;
use adk_model::gemini::{
    GeminiFileState, GeminiRequestOptions, MediaResolution, VideoMetadata, video_part_annotations,
};
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn answer() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "ok"}]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 1, "totalTokenCount": 4}
    }))
}

async fn sent_body(request: LlmRequest) -> Value {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(answer()).mount(&server).await;
    let model = GeminiModel::new_with_base_url(
        "test-key",
        "gemini-3.8-flash",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap();
    let mut stream = model.generate_content(request, false).await.unwrap();
    while let Some(response) = stream.next().await {
        response.unwrap();
    }
    let received = server.received_requests().await.unwrap();
    serde_json::from_slice(&received[0].body).unwrap()
}

fn user(parts: Vec<Part>) -> LlmRequest {
    LlmRequest::new("gemini-3.8-flash", vec![Content { role: "user".into(), parts }])
}

#[tokio::test]
async fn a_file_part_goes_out_as_file_data_with_its_sampling() {
    let uri = "https://generativelanguage.googleapis.com/v1beta/files/abc123";
    let body = sent_body(user(vec![
        Part::FileData {
            mime_type: "video/mp4".into(),
            file_uri: uri.into(),
            annotations: Some(video_part_annotations(&VideoMetadata::default().with_fps(1.0))),
        },
        Part::Text { text: "When does the forklift enter the aisle?".into() },
    ]))
    .await;
    assert_eq!(
        body["contents"][0]["parts"],
        json!([
            {"fileData": {"mimeType": "video/mp4", "fileUri": uri}, "videoMetadata": {"fps": 1.0}},
            {"text": "When does the forklift enter the aisle?"}
        ])
    );
}

#[tokio::test]
async fn cloud_storage_and_web_uris_are_native_and_others_stay_text() {
    let file = |uri: &str| Part::FileData {
        mime_type: "application/pdf".into(),
        file_uri: uri.into(),
        annotations: None,
    };
    let body = sent_body(user(vec![
        file("gs://bucket/report.pdf"),
        file("artifact://app/u/s/report.pdf#1"),
    ]))
    .await;
    let parts = &body["contents"][0]["parts"];
    assert_eq!(
        parts[0],
        json!({"fileData": {"mimeType": "application/pdf", "fileUri": "gs://bucket/report.pdf"}})
    );
    assert_eq!(
        parts[1],
        json!({"text": "<attachment mime_type=\"application/pdf\" uri=\"artifact://app/u/s/report.pdf#1\" />"})
    );
}

#[tokio::test]
async fn inline_video_carries_its_sampling_and_the_media_resolution_is_sent() {
    let mut config = GenerateContentConfig::default();
    GeminiRequestOptions::default()
        .with_media_resolution(MediaResolution::Low)
        .insert_into(&mut config);
    let request = user(vec![Part::InlineData {
        mime_type: "video/mp4".into(),
        data: vec![0, 0, 0, 24],
        uri: None,
        annotations: Some(video_part_annotations(
            &VideoMetadata::default().with_fps(0.5).with_offsets(Some(10.0), None),
        )),
    }])
    .with_config(config);
    let body = sent_body(request).await;
    assert_eq!(
        body["contents"][0]["parts"][0]["videoMetadata"],
        json!({"fps": 0.5, "startOffset": "10s"})
    );
    assert_eq!(body["generationConfig"]["mediaResolution"], "MEDIA_RESOLUTION_LOW");
}

#[tokio::test]
async fn a_video_is_uploaded_once_and_ready_when_active() {
    let server = MockServer::start().await;
    let file = |state: &str| {
        json!({
            "name": "files/abc123",
            "uri": format!("{}/v1beta/files/abc123", "https://generativelanguage.googleapis.com"),
            "mimeType": "video/mp4",
            "sizeBytes": "4",
            "state": state
        })
    };
    Mock::given(method("POST"))
        .and(path("/upload/v1beta/files"))
        .and(header("x-goog-upload-command", "start"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-goog-upload-url", format!("{}/upload-session/1", server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload-session/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"file": file("PROCESSING")})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1beta/files/abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file("ACTIVE")))
        .mount(&server)
        .await;
    let model = GeminiModel::new_with_base_url(
        "test-key",
        "gemini-3.8-flash",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap();
    let uploaded = model.upload_file(vec![0, 0, 0, 24], "video/mp4", Some("aisle")).await.unwrap();
    assert_eq!(uploaded.state, GeminiFileState::Processing);
    assert_eq!(uploaded.uri, "https://generativelanguage.googleapis.com/v1beta/files/abc123");
    let ready = model
        .wait_for_file(&uploaded.name, Duration::from_millis(10), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(ready.state, GeminiFileState::Active);
    assert_eq!(ready.size_bytes, Some(4));
    assert!(model.upload_file(vec![1], "not a mime", None).await.is_err());
}
