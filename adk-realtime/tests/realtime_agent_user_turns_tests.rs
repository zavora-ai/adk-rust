//! `RealtimeAgent` under `Runner` saves the user's spoken turns to the session as user events,
//! in conversation order beside the model's replies, so history and memory see both sides.

#![cfg(feature = "scripted")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{Content, Event, Part};
use adk_realtime::RealtimeAgent;
use adk_realtime::ServerEvent;
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::json;

const APP: &str = "voice-app";
const USER: &str = "alice";
const SESSION: &str = "s1";

/// The model's spoken reply under its own item, as a provider streams it.
fn said(item_id: &str, text: &str) -> ServerEvent {
    ServerEvent::TranscriptDelta {
        event_id: format!("evt-{item_id}"),
        response_id: format!("resp-{item_id}"),
        item_id: item_id.to_string(),
        output_index: 0,
        content_index: 0,
        delta: text.to_string(),
    }
}

/// A piece of the user's input transcript under `item_id` (Gemini Live sends none).
fn heard(item_id: &str, text: &str) -> ServerEvent {
    ServerEvent::InputTranscriptDelta {
        item_id: item_id.to_string(),
        content_index: 0,
        delta: text.to_string(),
    }
}

fn done() -> ServerEvent {
    scripted::response_done(json!({}))
}

/// Runs `agent` once under `Runner` on a fresh in-memory session; returns the session's events.
async fn run(agent: RealtimeAgent) -> Vec<Event> {
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
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();

    let events: Vec<_> = runner
        .run_str(USER, SESSION, Content::new("user").with_text("Hello"))
        .await
        .unwrap()
        .collect()
        .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let session = sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: SESSION.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    session.events().all()
}

/// The session's text turns after one run whose provider answers the text with `script`, as
/// `(author, text)`.
async fn turns_after(script: Vec<ServerEvent>) -> Vec<(String, String)> {
    let model = ScriptedRealtimeModel::new(vec![ScriptedReply::new(script).on_text()])
        .with_idle_timeout(Duration::from_millis(100));
    let agent = RealtimeAgent::builder("voice").model(Arc::new(model)).build().unwrap();
    run(agent).await.iter().filter_map(text_turn).collect()
}

fn text_turn(event: &Event) -> Option<(String, String)> {
    let content = event.llm_response.content.as_ref()?;
    if content.role == "system" {
        return None;
    }
    let text: String = content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then(|| (event.author.clone(), text))
}

fn turns(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected.iter().map(|(author, text)| (author.to_string(), text.to_string())).collect()
}

/// Gemini Live streams the user's transcript without an item and never completes it: each
/// utterance is saved where the model's reply to it starts, and the last one at the end of
/// the session.
#[tokio::test]
async fn streamed_transcripts_are_saved_before_the_reply() {
    let saved = turns_after(vec![
        said("r1", "Hi, how can I help?"),
        done(),
        heard("", "What's the "),
        heard("", "weather?"),
        said("r2", "Sunny."),
        done(),
        heard("", "Thanks, bye."),
    ])
    .await;

    assert_eq!(
        saved,
        turns(&[
            ("user", "Hello"),
            ("voice", "Hi, how can I help?"),
            ("user", "What's the weather?"),
            ("voice", "Sunny."),
            ("user", "Thanks, bye."),
        ])
    );
}

/// OpenAI names the item and completes its transcript, possibly after the model's response
/// has started: the completed transcript is saved once, before the reply's text.
#[tokio::test]
async fn completed_transcripts_are_saved_once() {
    let saved = turns_after(vec![
        heard("item-1", "And tomo"),
        scripted::response_created(),
        ServerEvent::InputTranscriptCompleted {
            item_id: "item-1".to_string(),
            content_index: 0,
            transcript: "And tomorrow?".to_string(),
        },
        said("r1", "Rain."),
        done(),
        scripted::input_transcript("Got it."),
    ])
    .await;

    assert_eq!(
        saved,
        turns(&[
            ("user", "Hello"),
            ("user", "And tomorrow?"),
            ("voice", "Rain."),
            ("user", "Got it.")
        ])
    );
}

/// `transcription()` asks the provider to transcribe the user, which is what produces the
/// spoken turns saved above; without it the session config asks for none, as before.
#[tokio::test]
async fn transcription_is_requested_only_when_asked() {
    for transcribe in [true, false] {
        let model =
            ScriptedRealtimeModel::new(Vec::new()).with_idle_timeout(Duration::from_millis(20));
        let builder = RealtimeAgent::builder("voice").model(Arc::new(model.clone()));
        let builder = if transcribe { builder.transcription() } else { builder };
        run(builder.build().unwrap()).await;

        let configs = model.log().configs();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].input_audio_transcription.is_some(), transcribe);
    }
}
