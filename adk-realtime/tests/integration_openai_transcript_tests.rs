//! `IntegratedRealtimeRunner` saves an OpenAI user transcript once, complete, when its partial
//! transcripts arrive before the model's response starts.

#![cfg(all(feature = "integration", feature = "scripted"))]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_realtime::ServerEvent;
use adk_realtime::integration::IntegratedRealtimeRunner;
use adk_realtime::scripted::{ScriptedRealtimeModel, ScriptedReply};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};

const APP: &str = "voice";
const USER: &str = "ana";
const SESSION: &str = "s1";

/// One spoken question on the OpenAI Realtime API (GA event shapes), as the session parses
/// each frame: server VAD commits the user's audio and starts the response while
/// `gpt-4o-transcribe` is still streaming the user's transcript, which completes after
/// `response.created`.
const SPOKEN_QUESTION: &[&str] = &[
    r#"{"type":"input_audio_buffer.speech_started","event_id":"event_C1","audio_start_ms":96,"item_id":"item_in"}"#,
    r#"{"type":"input_audio_buffer.speech_stopped","event_id":"event_C2","audio_end_ms":2208,"item_id":"item_in"}"#,
    r#"{"type":"input_audio_buffer.committed","event_id":"event_C3","previous_item_id":null,"item_id":"item_in"}"#,
    r#"{"type":"conversation.item.input_audio_transcription.delta","event_id":"event_C4","item_id":"item_in","content_index":0,"delta":"What is"}"#,
    r#"{"type":"response.created","event_id":"event_C5","response":{"object":"realtime.response","id":"resp_1","status":"in_progress","output":[]}}"#,
    r#"{"type":"conversation.item.input_audio_transcription.delta","event_id":"event_C6","item_id":"item_in","content_index":0,"delta":" two plus two?"}"#,
    r#"{"type":"conversation.item.input_audio_transcription.completed","event_id":"event_C7","item_id":"item_in","content_index":0,"transcript":"What is two plus two?"}"#,
    r#"{"type":"response.output_audio_transcript.delta","event_id":"event_C8","response_id":"resp_1","item_id":"item_out","output_index":0,"content_index":0,"delta":"Two plus two is four."}"#,
    r#"{"type":"response.output_audio_transcript.done","event_id":"event_C9","response_id":"resp_1","item_id":"item_out","output_index":0,"content_index":0,"transcript":"Two plus two is four."}"#,
    r#"{"type":"response.done","event_id":"event_C10","response":{"object":"realtime.response","id":"resp_1","status":"completed","output":[]}}"#,
];

async fn saved_turns(sessions: &InMemorySessionService) -> Vec<(String, String)> {
    sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: SESSION.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .all()
        .iter()
        .filter_map(|event| {
            let text = event.content()?.parts.first()?.text()?.to_string();
            Some((event.author.clone(), text))
        })
        .collect()
}

/// On `main` the runner saved the partial "What is" when the response started and the
/// completed transcript again when it arrived, and reported two `InputTranscriptCompleted`.
#[tokio::test]
async fn a_user_transcript_completed_after_the_response_starts_is_saved_once() {
    let events: Vec<ServerEvent> =
        SPOKEN_QUESTION.iter().map(|frame| serde_json::from_str(frame).unwrap()).collect();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let model = ScriptedRealtimeModel::new(vec![ScriptedReply::new(events).on_connect()])
        .with_idle_timeout(Duration::from_millis(50));
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model))
        .identity(APP, USER, SESSION)
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();

    runner.connect().await.unwrap();
    let mut completed = Vec::new();
    while let Some(event) = runner.next_event().await {
        if let ServerEvent::InputTranscriptCompleted { transcript, .. } = event.unwrap() {
            completed.push(transcript);
        }
    }
    let _ = runner.close().await;

    assert_eq!(completed, vec!["What is two plus two?".to_string()]);
    assert_eq!(
        saved_turns(&sessions).await,
        vec![
            ("user".to_string(), "What is two plus two?".to_string()),
            ("model".to_string(), "Two plus two is four.".to_string()),
        ]
    );
}
