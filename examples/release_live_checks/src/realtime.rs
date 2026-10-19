//! Items 2 and 3: OpenAI Realtime over WebRTC, and Gemini Live connect.
//!
//! Neither scenario installs a rustls crypto provider: connecting must work out of the box.

use std::time::Duration;

use adk_realtime::{BoxedModel, RealtimeConfig, ServerEvent};

use crate::report::{Mode, Report, Verdict, check, fail, key, skip};

pub async fn run_gemini(report: &mut Report) {
    let verdict = match key("GEMINI_API_KEY").or_else(|| key("GOOGLE_API_KEY")) {
        Some(api_key) => {
            let model: BoxedModel = std::sync::Arc::new(
                adk_realtime::gemini::GeminiRealtimeModel::new(
                    adk_realtime::gemini::GeminiLiveBackend::studio(api_key),
                    "models/gemini-3.1-flash-live-preview",
                ),
            );
            round_trip(model).await
        }
        None => Ok(skip("GEMINI_API_KEY is not set")),
    };
    report.record_result("gemini_live_connect", 3, Mode::Live, verdict);
}

pub async fn run_openai_webrtc(report: &mut Report) {
    #[cfg(feature = "webrtc")]
    let verdict = match key("OPENAI_API_KEY") {
        Some(api_key) => {
            let model: BoxedModel = std::sync::Arc::new(
                adk_realtime::openai::OpenAIRealtimeModel::new(api_key, "gpt-realtime-2.1")
                    .with_transport(adk_realtime::openai::OpenAITransport::WebRTC),
            );
            round_trip(model).await
        }
        None => Ok(skip("OPENAI_API_KEY is not set")),
    };
    #[cfg(not(feature = "webrtc"))]
    let verdict: anyhow::Result<Verdict> =
        Ok(skip("built without the `webrtc` feature (needs cmake)"));
    report.record_result("openai_webrtc_round_trip", 2, Mode::Live, verdict);
}

/// Connects, asks one question by text, and waits for the answer to finish.
///
/// Runs on its own task so a panic inside `connect` is reported instead of ending the run.
async fn round_trip(model: BoxedModel) -> anyhow::Result<Verdict> {
    let task = tokio::spawn(async move {
        let config = RealtimeConfig::default()
            .with_instruction("Answer in one short sentence.");
        let session = model.connect(config).await.map_err(|error| format!("connect: {error}"))?;
        session
            .send_text("What is the capital of France?")
            .await
            .map_err(|error| format!("send_text: {error}"))?;
        session.create_response().await.map_err(|error| format!("create_response: {error}"))?;
        let mut transcript = String::new();
        let mut audio_bytes = 0usize;
        let outcome = loop {
            let next = tokio::time::timeout(Duration::from_secs(30), session.next_event()).await;
            match next {
                Ok(Some(Ok(ServerEvent::ResponseDone { .. }))) => break Ok(()),
                Ok(Some(Ok(ServerEvent::TranscriptDelta { delta, .. })))
                | Ok(Some(Ok(ServerEvent::TextDelta { delta, .. }))) => {
                    transcript.push_str(&delta);
                }
                Ok(Some(Ok(ServerEvent::AudioDelta { delta, .. }))) => audio_bytes += delta.len(),
                Ok(Some(Ok(ServerEvent::Error { error, .. }))) => {
                    break Err(format!("server error: {}", error.message));
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(error))) => break Err(format!("event: {error}")),
                Ok(None) => break Err("the session ended before the response finished".into()),
                Err(_) => break Err("no response within 30s".into()),
            }
        };
        let _ = session.close().await;
        outcome.map(|()| (transcript, audio_bytes))
    });
    Ok(match task.await {
        Ok(Ok((transcript, audio_bytes))) => check(
            audio_bytes > 0 || !transcript.trim().is_empty(),
            format!("{audio_bytes} audio bytes, transcript {transcript:?}"),
        ),
        Ok(Err(error)) => fail(error),
        Err(error) if error.is_panic() => {
            let payload = error.into_panic();
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                .unwrap_or_default();
            fail(format!("connect panicked: {message}"))
        }
        Err(error) => fail(format!("task failed: {error}")),
    })
}
