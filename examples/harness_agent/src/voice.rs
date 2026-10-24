//! The voice surface (spec 3.1 §5 H5, §7.17): talk to the harness; a spoken "yes" approves
//! the held call; the transcript and the tool call are in the same audit trail.
//!
//! The scripted scenario runs the same harness over `ScriptedRealtimeModel`, so the model's
//! turns, its `bash` call and the user's transcribed answer are scripted while the harness,
//! its policy, the approval store, the session and the audit trail are real. The live
//! scenario runs Gemini Live (`gemini-3.1-flash-live-preview`) or the OpenAI Realtime API
//! and answers the held call with a recording of the word "yes" (`fixtures/yes-16k.wav`,
//! or `yes-24k.wav` for OpenAI). The 3.0 `CodingAgent` has no voice surface, which is what
//! the baseline column shows.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_core::Llm;
use adk_devtools::Workspace;
use adk_harness::{
    DecisionSource, HarnessAgent, HarnessMode, Pack, SpokenDecision, VOICE_AUDIT_DECIDED, VOICE_AUDIT_HELD,
    VOICE_AUDIT_RESUMED, VoiceConfig, VoiceEvent, VoiceSurface,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_realtime::audio::{AudioChunk, AudioFormat};
use adk_realtime::model::BoxedModel;
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_realtime::{RealtimeModel, ServerEvent};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use anyhow::Result;
use serde_json::{Value, json};

use crate::{Checks, Subject, write_crate};

pub const SCRIPTED: &[&str] = &["voice_spoken_approval"];

pub const LIVE: &[&str] = &["live_voice_approval"];

const APP: &str = "harness";
const USER: &str = "ana";
const SESSION: &str = "voice-1";

/// A recording of the word "yes", mono PCM16 at the rate each provider takes: Gemini Live
/// 16 kHz, the OpenAI Realtime API 24 kHz.
const YES_16K: &[u8] = include_bytes!("../fixtures/yes-16k.wav");
const YES_24K: &[u8] = include_bytes!("../fixtures/yes-24k.wav");

/// How long the live scenario waits for the whole exchange.
const LIVE_DEADLINE: Duration = Duration::from_secs(120);

/// Runs scripted scenario `name`, if it is one of these.
pub async fn run_scripted(name: &str, subject: Subject) -> Option<Result<Checks>> {
    match (name, subject) {
        ("voice_spoken_approval", Subject::Baseline) => {
            let mut checks = Checks::default();
            checks.check("the 3.0 CodingAgent has no voice surface: nothing to say yes to", false);
            Some(Ok(checks))
        }
        ("voice_spoken_approval", Subject::Harness) => Some(voice_spoken_approval().await),
        _ => None,
    }
}

/// Runs live scenario `name` on `model`'s provider, if it is one of these.
pub async fn run_live(name: &str, model_name: &str, model: Arc<dyn Llm>) -> Option<Result<Checks>> {
    match name {
        "live_voice_approval" => Some(live_voice_approval(model_name, model).await),
        _ => None,
    }
}

async fn sessions() -> Result<Arc<InMemorySessionService>> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await?;
    Ok(sessions)
}

fn harness(dir: &Path, model: Arc<dyn Llm>, audit: &InMemoryAuditRecorder) -> Result<Arc<HarnessAgent>> {
    Ok(Arc::new(
        HarnessAgent::builder(APP)
            .model(model)
            .workspace(Workspace::new(dir).allow_bash(true))
            .mode(HarnessMode::Ask)
            .pack(Pack::Voice)
            .audit_log(Arc::new(audit.clone()))
            .build()?,
    ))
}

fn records(audit: &InMemoryAuditRecorder, action: &str) -> Vec<Value> {
    audit
        .records()
        .into_iter()
        .filter(|record| record.action == action)
        .map(|record| Value::Object(record.ext.into_iter().collect()))
        .collect()
}

/// Sends the recording as the user's turn the way a microphone does: 100 ms chunks at the
/// rate they are spoken, then 1.5 s of silence, which is what ends the turn for a provider
/// whose voice activity detection runs server-side.
async fn say_yes(surface: &VoiceSurface, rate: u32) -> Result<()> {
    let (wav, format) = if rate == 24_000 { (YES_24K, AudioFormat::pcm16_24khz()) } else { (YES_16K, AudioFormat::pcm16_16khz()) };
    let chunk_bytes = rate as usize / 10 * 2;
    let silence = vec![0u8; rate as usize * 3];
    for chunk in wav[44..].chunks(chunk_bytes).chain(silence.chunks(chunk_bytes)) {
        surface.send_audio(&AudioChunk::new(chunk.to_vec(), format.clone())).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    surface.commit_audio().await?;
    Ok(())
}

/// What one voice exchange produced.
#[derive(Default)]
struct Exchange {
    held: Vec<(String, adk_core::ToolConfirmationRequest)>,
    decided: Vec<(SpokenDecision, DecisionSource)>,
    resumed: Vec<adk_realtime::integration::ResumedToolCall>,
    user_turns: Vec<String>,
    spoken: Vec<String>,
}

/// Drives the surface until the exchange ends: the recording answers the first hold once the
/// model has asked about it (its first spoken response after the hold), and the response
/// after the resumed call — or a plain answer with nothing held — ends it.
async fn exchange(surface: &VoiceSurface, deadline: Duration, rate: u32) -> Result<Exchange> {
    let mut exchange = Exchange::default();
    let mut answered = false;
    let mut saw_call = false;
    let mut transcript = String::new();
    let started = std::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(started.elapsed());
        let next = match tokio::time::timeout(remaining, surface.next_event()).await {
            Ok(next) => next,
            Err(_) => break,
        };
        let Some(event) = next else {
            break;
        };
        match event? {
            VoiceEvent::Held { call_id, request } => exchange.held.push((call_id, request)),
            VoiceEvent::Decided { decision, by, .. } => exchange.decided.push((decision, by)),
            VoiceEvent::Resumed(call) => exchange.resumed.push(call),
            VoiceEvent::Provider(event) => match event {
                ServerEvent::InputTranscriptCompleted { transcript, .. } => exchange.user_turns.push(transcript),
                ServerEvent::TranscriptDelta { delta, .. } | ServerEvent::TextDelta { delta, .. } => {
                    transcript.push_str(&delta);
                }
                ServerEvent::FunctionCallDone { .. } => saw_call = true,
                ServerEvent::ResponseDone { .. } => {
                    let said = std::mem::take(&mut transcript);
                    let called = std::mem::take(&mut saw_call);
                    let held = !surface.held_calls().is_empty();
                    if !said.trim().is_empty() {
                        exchange.spoken.push(said);
                        if held && !answered {
                            say_yes(surface, rate).await?;
                            answered = true;
                            continue;
                        }
                    }
                    if called {
                        continue;
                    }
                    if !exchange.resumed.is_empty() || !held || !answered {
                        break;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(exchange)
}

fn check_exchange(checks: &mut Checks, exchange: &Exchange, audit: &InMemoryAuditRecorder, session_events: &[adk_core::Event]) {
    let held_bash = exchange
        .held
        .iter()
        .find(|(_, request)| request.tool_name == "bash")
        .map(|(_, request)| request.args["command"].as_str().unwrap_or("").to_string());
    checks.check(
        format!("a bash call was held for the user's approval ({held_bash:?})"),
        held_bash.as_deref().is_some_and(|command| command.contains("cargo test")),
    );
    checks.check(
        "the spoken yes decided the held call",
        exchange.decided.iter().any(|(decision, by)| *decision == SpokenDecision::Approve && *by == DecisionSource::Voice),
    );
    let resumed = exchange.resumed.iter().find(|call| call.name == "bash");
    checks.check(
        format!("the approved call ran ({:?})", resumed.map(|call| call.result.to_string())),
        resumed.is_some_and(|call| call.result.get("error").is_none()),
    );
    let runs = records(audit, "harness.bash.end").len();
    checks.check(format!("bash ran exactly once ({runs})"), runs == 1);
    let user_turns: Vec<String> = session_events
        .iter()
        .filter(|event| event.author == "user")
        .filter_map(|event| event.content().and_then(|c| c.parts.first()).and_then(|p| p.text().map(str::to_string)))
        .collect();
    checks.check(
        format!("the transcript of the answer is a user turn in the session ({user_turns:?})"),
        user_turns.iter().any(|turn| turn.to_lowercase().contains("yes")),
    );
    checks.check(
        "the tool call is in the same session record",
        session_events.iter().any(|event| {
            event.content().is_some_and(|content| {
                content.parts.iter().any(|part| matches!(part, adk_core::Part::FunctionResponse { function_response, .. } if function_response.name == "bash"))
            })
        }),
    );
    let decided = records(audit, VOICE_AUDIT_DECIDED);
    checks.check(
        "harness.voice.held, .decided (with the transcript's digest) and .resumed are in the audit trail",
        !records(audit, VOICE_AUDIT_HELD).is_empty()
            && decided.iter().any(|record| record["by"] == "voice" && record["transcriptDigest"].is_string())
            && records(audit, VOICE_AUDIT_RESUMED).iter().any(|record| record["executed"] == true),
    );
}

async fn session_events(sessions: &InMemorySessionService) -> Result<Vec<adk_core::Event>> {
    Ok(sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: SESSION.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?
        .events()
        .all())
}

/// H5 scripted: the model calls `bash`, is told the call waits, hears "yes", and closes
/// with a spoken result; the command ran once, after the approval.
async fn voice_spoken_approval() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let audit = InMemoryAuditRecorder::new();
    let chat: Arc<dyn Llm> = Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let harness = harness(dir.path(), chat, &audit)?;
    let realtime = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(scripted::calls("call-1", "bash", json!({ "command": "cargo test --quiet > ran.txt; printf ran >> ran.txt" }))).on_text(),
        ScriptedReply::new(scripted::spoken("Running the tests needs your approval. Say yes or no.")).on_tool_output(),
        ScriptedReply::new(vec![scripted::input_transcript("yes")]).on_audio(),
        ScriptedReply::new(scripted::spoken("Done.")).on_tool_output(),
    ])
    .with_model_id("gemini-3.1-flash-live-preview")
    .with_idle_timeout(Duration::from_millis(200));
    let sessions = sessions().await?;
    let surface = VoiceSurface::open(harness, Arc::new(realtime.clone()), sessions.clone(), USER, SESSION, VoiceConfig::new(APP))?;
    surface.connect().await?;
    surface.say("what's failing in the build?").await?;
    let exchange = exchange(&surface, Duration::from_secs(30), 16_000).await?;
    surface.close().await;

    check_exchange(&mut checks, &exchange, &audit, &session_events(&sessions).await?);
    checks.check("the command's output file exists once", std::fs::read_to_string(dir.path().join("ran.txt")).is_ok_and(|text| text.ends_with("ran")));
    let outputs = realtime.log().tool_outputs();
    checks.check(
        format!("the model was told the call waits, then given its result ({} outputs)", outputs.len()),
        outputs.len() == 2 && adk_core::is_confirmation_pending(&outputs[0].output) && outputs[1].output.get("error").is_none(),
    );
    Ok(checks)
}

/// The realtime model of `model_name`'s provider and the input sample rate it takes, when
/// the provider has one.
fn live_realtime(model_name: &str) -> Result<Option<(String, BoxedModel, u32)>> {
    let key = |name: &str| std::env::var(name).ok().filter(|key| !key.is_empty());
    if model_name.starts_with("gemini") {
        let Some(key) = key("GOOGLE_API_KEY") else { return Ok(None) };
        let live = adk_realtime::gemini::GeminiRealtimeModel::new(adk_realtime::gemini::GeminiLiveBackend::studio(key), adk_realtime::gemini::DEFAULT_MODEL);
        return Ok(Some((live.model_id().to_string(), Arc::new(live), 16_000)));
    }
    if model_name.starts_with("gpt") {
        let Some(key) = key("OPENAI_API_KEY") else { return Ok(None) };
        let live = adk_realtime::openai::OpenAIRealtimeModel::with_default_model(key);
        return Ok(Some((live.model_id().to_string(), Arc::new(live), 24_000)));
    }
    Ok(None)
}

/// H5 live: a live realtime model runs `cargo test --quiet` on request; the recording of
/// "yes" approves the held call; the transcript and the call share one audit trail.
async fn live_voice_approval(model_name: &str, model: Arc<dyn Llm>) -> Result<Checks> {
    let Some((live_id, realtime, rate)) = live_realtime(model_name)? else {
        return Ok(Checks::skipped(&format!("{model_name}'s provider has no realtime API, or no key is set")));
    };
    println!("    realtime model: {live_id}");
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    write_crate(dir.path())?;
    let audit = InMemoryAuditRecorder::new();
    let harness = harness(dir.path(), model, &audit)?;
    let sessions = sessions().await?;
    let surface = VoiceSurface::open(harness, realtime, sessions.clone(), USER, SESSION, VoiceConfig::new(APP))?;
    surface.connect().await?;
    surface
        .say(
            "Run exactly `cargo test --quiet` in this workspace with the bash tool, without any other flags, pipes or \
             redirections, and tell me whether the tests pass. Do not change any file. When the harness answers \
             that the call is waiting for my confirmation, ask me to say yes or no and wait for my answer.",
        )
        .await?;
    let exchange = exchange(&surface, LIVE_DEADLINE, rate).await?;
    surface.close().await;
    for turn in &exchange.spoken {
        println!("    [model] {}", adk_harness::escape_control(turn));
    }
    for turn in &exchange.user_turns {
        println!("    [user] {}", adk_harness::escape_control(turn));
    }
    check_exchange(&mut checks, &exchange, &audit, &session_events(&sessions).await?);
    Ok(checks)
}
