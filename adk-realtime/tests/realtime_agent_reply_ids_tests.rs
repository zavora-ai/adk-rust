//! `RealtimeAgent` saves each of the model's replies under its own event id, also on a provider
//! that streams replies without an item id (Gemini Live), so a session backend that keys events
//! by id — SQLite here — stores every reply.

#![cfg(feature = "scripted")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{Content, Event, Part};
use adk_realtime::RealtimeAgent;
use adk_realtime::ServerEvent;
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, SessionService, SqliteSessionService};
use futures::StreamExt;
use serde_json::json;

const APP: &str = "voice-app";
const USER: &str = "ana";
const SESSION: &str = "s1";

/// A piece of the model's spoken reply as `GeminiRealtimeSession` translates it: no item id.
fn said(text: &str) -> ServerEvent {
    ServerEvent::TranscriptDelta {
        event_id: format!("evt-{}", uuid::Uuid::new_v4()),
        response_id: String::new(),
        item_id: String::new(),
        output_index: 0,
        content_index: 0,
        delta: text.to_string(),
    }
}

/// A piece of the user's speech as Gemini Live transcribes it: no item id.
fn heard(text: &str) -> ServerEvent {
    ServerEvent::InputTranscriptDelta {
        item_id: String::new(),
        content_index: 0,
        delta: text.to_string(),
    }
}

fn turn_complete() -> ServerEvent {
    scripted::response_done(json!({ "serverContent": { "turnComplete": true } }))
}

fn reply_text(event: &Event) -> Option<String> {
    let content = event.llm_response.content.as_ref()?;
    let text: String = content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then_some(text)
}

/// Two replies in one run, the second to a follow-up the user spoke: on `main` both final
/// transcripts were saved as `{invocation}:realtime-transcript:`, so SQLite refused the
/// second insert and the run ended with `insert failed: UNIQUE constraint failed`.
#[tokio::test]
async fn each_reply_without_an_item_id_is_saved_under_its_own_id() {
    let sessions = Arc::new(SqliteSessionService::new("sqlite::memory:").await.unwrap());
    sessions.migrate().await.unwrap();
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();

    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(vec![
            said("Two plus two"),
            said(" is four."),
            turn_complete(),
            heard("And three plus three?"),
            said("Six."),
            turn_complete(),
        ])
        .on_text(),
    ])
    .with_idle_timeout(Duration::from_millis(100));
    let agent = RealtimeAgent::builder("voice").model(Arc::new(model)).build().unwrap();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();

    let results: Vec<_> = runner
        .run_str(USER, SESSION, Content::new("user").with_text("What is two plus two?"))
        .await
        .unwrap()
        .collect()
        .await;
    assert!(results.iter().all(Result::is_ok), "{results:?}");

    let saved = sessions
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
        .all();
    let replies: Vec<&Event> = saved
        .iter()
        .filter(|event| {
            event.author == "voice"
                && event.llm_response.content.as_ref().is_some_and(|c| c.role == "model")
                && reply_text(event).is_some()
        })
        .collect();
    assert_eq!(
        replies.iter().filter_map(|event| reply_text(event)).collect::<Vec<_>>(),
        vec!["Two plus two is four.".to_string(), "Six.".to_string()]
    );
    assert_ne!(replies[0].id, replies[1].id);

    let turns: Vec<(String, String)> = saved
        .iter()
        .filter(|event| event.llm_response.content.as_ref().is_some_and(|c| c.role != "system"))
        .filter_map(|event| Some((event.author.clone(), reply_text(event)?)))
        .collect();
    assert_eq!(
        turns,
        [
            ("user", "What is two plus two?"),
            ("voice", "Two plus two is four."),
            ("user", "And three plus three?"),
            ("voice", "Six."),
        ]
        .map(|(author, text)| (author.to_string(), text.to_string()))
    );
}
