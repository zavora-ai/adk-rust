//! Session archive and prewarm against a live model: a real conversation on `claude-haiku-5-5`
//! is archived, refused, exported, unarchived and continued from its history; and the first
//! turn of a fresh agent is timed cold and after `Runner::prewarm`.
//!
//! Requires `ANTHROPIC_API_KEY`; run with `--run-ignored only`. Timings print as a table.

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_core::{AdkIdentity, AppName, Content, Event, Llm, SessionId, UserId};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_runner::{Prewarm, Runner};
use adk_session::{
    CreateRequest, InMemorySessionService, ListRequest, SessionBundle, SessionService,
    SqliteSessionService,
};
use futures::StreamExt;

const MODEL: &str = "claude-haiku-5-5";
const APP: &str = "live";
const USER: &str = "ana";

fn model() -> Arc<dyn Llm> {
    dotenvy::dotenv().ok();
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    Arc::new(AnthropicClient::new(AnthropicConfig::new(key, MODEL)).unwrap())
}

fn runner(model: Arc<dyn Llm>, sessions: Arc<dyn SessionService>) -> Runner {
    let agent = LlmAgentBuilder::new("assistant")
        .instruction(
            "You are a brief assistant. Remember what the user tells you and answer in one short \
             sentence.",
        )
        .model(model)
        .build()
        .unwrap();
    Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()
        .unwrap()
}

async fn create(sessions: &dyn SessionService, id: &str) -> AdkIdentity {
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(id.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    AdkIdentity::new(
        AppName::try_from(APP).unwrap(),
        UserId::try_from(USER).unwrap(),
        SessionId::try_from(id).unwrap(),
    )
}

/// What one turn produced: its text, or its error code, and when the first content arrived.
struct Turn {
    text: String,
    error: Option<&'static str>,
    first_content: Option<Duration>,
    total: Duration,
}

async fn turn(runner: &Runner, session: &str, text: &str) -> Turn {
    let started = Instant::now();
    let mut stream = runner
        .run(
            UserId::new(USER).unwrap(),
            SessionId::new(session).unwrap(),
            Content::new("user").with_text(text),
        )
        .await
        .unwrap();
    let mut out =
        Turn { text: String::new(), error: None, first_content: None, total: Duration::ZERO };
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                let event: Event = event;
                let Some(content) = event.content() else { continue };
                let words: String = content.parts.iter().filter_map(|part| part.text()).collect();
                if words.trim().is_empty() {
                    continue;
                }
                out.first_content.get_or_insert(started.elapsed());
                if !event.llm_response.partial {
                    out.text = words;
                }
            }
            Err(error) => {
                println!("turn error: {error}");
                out.error = Some(error.code);
            }
        }
    }
    out.total = started.elapsed();
    out
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort();
    values[values.len() / 2]
}

fn ms(duration: Duration) -> String {
    format!("{:.0} ms", duration.as_secs_f64() * 1000.0)
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn a_live_conversation_is_archived_refused_exported_and_continued() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("sessions.db").display());
    let store = SqliteSessionService::new(&url).await.unwrap();
    store.migrate().await.unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(store);
    let runner = runner(model(), sessions.clone());
    let identity = create(sessions.as_ref(), "lyon-trip").await;
    create(sessions.as_ref(), "other-trip").await;

    let first = turn(
        &runner,
        "lyon-trip",
        "Note this down: my train to Lyon leaves at 09:12. Reply with OK.",
    )
    .await;
    assert_eq!(first.error, None);
    println!("turn 1 answer: {:?}", first.text);

    let mark = sessions.archive(&identity, Some("trip booked".into())).await.unwrap();
    println!("archived at {} ({:?})", mark.archived_at, mark.reason);
    let listing =
        ListRequest { app_name: APP.into(), user_id: USER.into(), limit: None, offset: None };
    let active: Vec<String> = sessions
        .list_active(listing.clone())
        .await
        .unwrap()
        .iter()
        .map(|session| session.id().to_string())
        .collect();
    assert_eq!(active, ["other-trip"]);

    let refused = turn(&runner, "lyon-trip", "When does my train leave?").await;
    assert_eq!(refused.error, Some("session.archived"));
    println!("turn on the archived session: {:?}", refused.error);

    let bundle = sessions.export_session(&identity).await.unwrap();
    let json = serde_json::to_string_pretty(&bundle).unwrap();
    let read: SessionBundle = serde_json::from_str(&json).unwrap();
    let texts: Vec<String> = read
        .events
        .iter()
        .filter_map(|event| event.content())
        .flat_map(|content| content.parts.iter().filter_map(|part| part.text().map(str::to_string)))
        .collect();
    println!("bundle: {} bytes, {} events, texts {texts:?}", json.len(), read.events.len());
    assert!(texts.iter().any(|text| text.contains("09:12")), "the user's message is in the bundle");
    assert!(texts.contains(&first.text), "the model's answer is in the bundle");
    assert_eq!(read.archived, Some(mark));

    sessions.unarchive(&identity).await.unwrap();
    let second = turn(
        &runner,
        "lyon-trip",
        "What time did I say my train leaves? Reply with the time only.",
    )
    .await;
    assert_eq!(second.error, None);
    println!("turn 2 answer after unarchive: {:?}", second.text);
    assert!(second.text.contains("9:12"), "the continued turn read the history: {:?}", second.text);
    assert_eq!(sessions.list_active(listing).await.unwrap().len(), 2);
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn prewarm_against_a_cold_first_turn() {
    const RUNS: usize = 7;
    let mut store_open = Vec::new();
    let mut build = Vec::new();
    let mut prewarm = Vec::new();
    let mut cold_first = Vec::new();
    let mut cold_total = Vec::new();
    let mut warm_first = Vec::new();
    let mut warm_total = Vec::new();
    for run in 0..RUNS * 2 {
        let warm = run % 2 == 1;
        // A fresh SQLite store, client and agent each time: nothing is pooled from before.
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("sessions.db").display());
        let opened = Instant::now();
        let store = SqliteSessionService::new(&url).await.unwrap();
        store.migrate().await.unwrap();
        store_open.push(opened.elapsed());
        let sessions: Arc<dyn SessionService> = Arc::new(store);
        let built = Instant::now();
        let runner = runner(model(), sessions.clone());
        build.push(built.elapsed());
        create(sessions.as_ref(), "s").await;
        if warm {
            let report = runner
                .prewarm(
                    Prewarm::new()
                        .session(UserId::new(USER).unwrap(), SessionId::new("s").unwrap()),
                )
                .await
                .unwrap();
            prewarm.push(report.elapsed);
        }
        let result = turn(&runner, "s", "Say OK.").await;
        assert_eq!(result.error, None);
        let first = result.first_content.expect("the turn answered");
        if warm {
            warm_first.push(first);
            warm_total.push(result.total);
        } else {
            cold_first.push(first);
            cold_total.push(result.total);
        }
    }
    let in_memory = Instant::now();
    let _ = InMemorySessionService::new();
    let in_memory = in_memory.elapsed();
    println!("| Component ({RUNS} runs each, median) | Time |");
    println!("|---|---|");
    println!("| SQLite store open + migrate (new file) | {} |", ms(median(store_open)));
    println!("| In-memory store open | {in_memory:?} |");
    println!("| Agent + runner build | {:?} |", median(build));
    println!("| `Runner::prewarm` (session read + model connection) | {} |", ms(median(prewarm)));
    println!("| First turn, cold: first content | {} |", ms(median(cold_first.clone())));
    println!("| First turn, prewarmed: first content | {} |", ms(median(warm_first.clone())));
    println!("| First turn, cold: complete | {} |", ms(median(cold_total)));
    println!("| First turn, prewarmed: complete | {} |", ms(median(warm_total)));
    println!(
        "cold first-content runs: {:?}",
        cold_first.iter().map(|d| d.as_millis()).collect::<Vec<_>>()
    );
    println!(
        "warm first-content runs: {:?}",
        warm_first.iter().map(|d| d.as_millis()).collect::<Vec<_>>()
    );
}
