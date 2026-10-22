//! The voice surface (spec 3.1 §7.17, §5 H5): the same harness over a realtime session,
//! the same policy, spoken approvals, one audit trail.
//!
//! The provider is `ScriptedRealtimeModel`, so the model's turns, its tool call and the
//! user's transcribed "yes" are scripted; the harness, its policy, the approval store, the
//! session and the audit trail are real. On `main` the voice pack is refused as
//! `not yet implemented`.

#![cfg(feature = "voice")]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_devtools::Workspace;
use adk_harness::{
    DecisionSource, HarnessAgent, HarnessMode, Pack, SpokenDecision, VOICE_AUDIT_DECIDED,
    VOICE_AUDIT_HELD, VOICE_AUDIT_RESUMED, VoiceConfig, VoiceEvent, VoiceSurface,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_realtime::RealtimeModel;
use adk_realtime::audio::AudioChunk;
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use serde_json::{Value, json};

const APP: &str = "adk-rust";
const USER: &str = "ana";
const SESSION: &str = "voice-1";

fn harness(dir: &Path, mode: HarnessMode, audit: &InMemoryAuditRecorder) -> Arc<HarnessAgent> {
    Arc::new(
        HarnessAgent::builder("harness")
            .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
            .workspace(Workspace::new(dir).allow_bash(true))
            .mode(mode)
            .pack(Pack::Voice)
            .audit_log(Arc::new(audit.clone()))
            .build()
            .expect("the harness builds with the voice pack"),
    )
}

async fn sessions() -> Arc<InMemorySessionService> {
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
    sessions
}

async fn session_events(sessions: &InMemorySessionService) -> Vec<adk_core::Event> {
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
}

/// The script of H5: the model calls `bash`, is told the call waits, hears the answer, and
/// closes with a spoken result.
fn h5_script(answer: &str) -> ScriptedRealtimeModel {
    ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(scripted::calls(
            "call-1",
            "bash",
            json!({ "command": "printf ran > ran.txt" }),
        ))
        .on_text(),
        ScriptedReply::new(scripted::spoken("That command needs your approval. Say yes or no."))
            .on_tool_output(),
        ScriptedReply::new(vec![scripted::input_transcript(answer)]).on_audio(),
        ScriptedReply::new(scripted::spoken("Done.")).on_tool_output(),
    ])
    .with_model_id("gemini-3.1-flash-live-preview")
    .with_idle_timeout(Duration::from_millis(150))
}

/// Drives the surface to the end of the session, answering the first hold with audio.
async fn drive(surface: &VoiceSurface) -> Vec<VoiceEvent> {
    let mut events = Vec::new();
    while let Some(event) = surface.next_event().await {
        let event = event.expect("the voice session runs without error");
        if matches!(event, VoiceEvent::Held { .. }) {
            // The pre-recorded answer: one chunk of audio the provider transcribes.
            surface.send_audio(&AudioChunk::pcm16_16khz(vec![0; 640])).await.unwrap();
        }
        events.push(event);
    }
    events
}

fn records_of(audit: &InMemoryAuditRecorder, action: &str) -> Vec<Value> {
    audit
        .records()
        .into_iter()
        .filter(|record| record.action == action)
        .map(|record| Value::Object(record.ext.into_iter().collect()))
        .collect()
}

#[tokio::test]
async fn a_spoken_yes_approves_the_held_bash_in_one_audit_trail() {
    let dir = tempfile::tempdir().unwrap();
    let audit = InMemoryAuditRecorder::new();
    let harness = harness(dir.path(), HarnessMode::Ask, &audit);
    let model = h5_script("yes");
    let sessions = sessions().await;
    let surface = VoiceSurface::open(
        harness,
        Arc::new(model.clone()),
        sessions.clone(),
        USER,
        SESSION,
        VoiceConfig::new(APP),
    )
    .unwrap();
    surface.connect().await.unwrap();
    surface.say("what's failing in the build?").await.unwrap();
    let events = drive(&surface).await;

    // The call was held — not run, not denied — and surfaced to the user.
    let held: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            VoiceEvent::Held { call_id, request } => Some((call_id.clone(), request.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(held.len(), 1, "{events:?}");
    assert_eq!(held[0].1.tool_name, "bash");
    // Held as the harness's own call: the acting agent in its fingerprint is the harness.
    let acting = held[0].1.acting.as_ref().map(|acting| acting.agent.id.as_str().to_string());
    assert!(acting.as_deref().is_some_and(|id| id.ends_with("/harness")), "{acting:?}");
    assert!(
        events.iter().any(|event| matches!(
            event,
            VoiceEvent::Decided {
                decision: SpokenDecision::Approve,
                by: DecisionSource::Voice,
                ..
            }
        )),
        "the spoken yes decided the call: {events:?}"
    );
    let resumed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            VoiceEvent::Resumed(call) => Some(call.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].call_id, "call-1");
    assert!(resumed[0].result.get("error").is_none(), "{}", resumed[0].result);

    // The command ran exactly once, after the approval.
    assert_eq!(std::fs::read_to_string(dir.path().join("ran.txt")).unwrap(), "ran");
    let outputs = model.log().tool_outputs();
    assert_eq!(outputs.len(), 2, "pending answer, then the result");
    assert!(adk_core::is_confirmation_pending(&outputs[0].output));
    assert!(outputs[1].output.get("error").is_none());
    assert!(surface.held_calls().is_empty());

    // The transcript and the tool call are in the same session record...
    let recorded = session_events(&sessions).await;
    let user_turns: Vec<String> = recorded
        .iter()
        .filter(|event| event.author == "user")
        .filter_map(|event| event.content().and_then(|c| c.parts[0].text().map(str::to_string)))
        .collect();
    assert_eq!(user_turns, vec!["what's failing in the build?".to_string(), "yes".to_string()]);
    assert!(recorded.iter().any(|event| event.author == "tool"), "{recorded:?}");

    // ...and in the same audit trail: the hold, the spoken decision with the transcript's
    // digest, the resumed call, and the shell's own records.
    let held_records = records_of(&audit, VOICE_AUDIT_HELD);
    assert_eq!(held_records.len(), 1);
    assert_eq!(held_records[0]["tool"], "bash");
    let decided = records_of(&audit, VOICE_AUDIT_DECIDED);
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!(decided[0]["decision"], "approve");
    assert_eq!(decided[0]["by"], "voice");
    assert!(decided[0]["transcriptDigest"].as_str().is_some_and(|d| d.len() == 64));
    let resumed_records = records_of(&audit, VOICE_AUDIT_RESUMED);
    assert_eq!(resumed_records.len(), 1);
    assert_eq!(resumed_records[0]["executed"], true);
    let actions: Vec<String> = audit.records().into_iter().map(|record| record.action).collect();
    assert!(actions.iter().any(|action| action == "harness.bash.begin"), "{actions:?}");
    assert!(actions.iter().any(|action| action == "harness.bash.end"), "{actions:?}");
    surface.close().await;
}

/// A realtime model hears "yes" before the surface applies it and may repeat the waiting call
/// (Gemini Live does): the repeat's hold supersedes the identical first one, the answer runs
/// the command once, and nothing is left held.
#[tokio::test]
async fn a_call_repeated_while_the_user_answers_runs_once() {
    let dir = tempfile::tempdir().unwrap();
    let audit = InMemoryAuditRecorder::new();
    let harness = harness(dir.path(), HarnessMode::Ask, &audit);
    let command = json!({ "command": "printf ran >> ran.txt" });
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(scripted::calls("call-1", "bash", command.clone())).on_text(),
        ScriptedReply::new(scripted::spoken("That needs your approval. Say yes or no."))
            .on_tool_output(),
        ScriptedReply::new(scripted::calls("call-2", "bash", command)).on_audio(),
        ScriptedReply::new(vec![scripted::input_transcript("yes")]).on_tool_output(),
        ScriptedReply::new(scripted::spoken("Done.")).on_tool_output(),
    ])
    .with_idle_timeout(Duration::from_millis(150));
    let surface = VoiceSurface::open(
        harness,
        Arc::new(model.clone()),
        sessions().await,
        USER,
        SESSION,
        VoiceConfig::new(APP),
    )
    .unwrap();
    surface.connect().await.unwrap();
    surface.say("run it").await.unwrap();
    let events = drive(&surface).await;

    assert_eq!(std::fs::read_to_string(dir.path().join("ran.txt")).unwrap(), "ran");
    assert_eq!(records_of(&audit, "harness.bash.end").len(), 1);
    let resumed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            VoiceEvent::Resumed(call) => Some(call.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(resumed.len(), 1, "{events:?}");
    assert_eq!(resumed[0].call_id, "call-2", "the repeat superseded the first hold");
    assert!(resumed[0].result.get("error").is_none(), "{}", resumed[0].result);
    assert!(events.iter().any(|event| matches!(
        event,
        VoiceEvent::Decided { call_id, decision: SpokenDecision::Approve, .. } if call_id == "call-2"
    )));
    assert!(surface.held_calls().is_empty(), "{:?}", surface.held_calls());
    assert!(surface.runner().held_calls().await.unwrap().is_empty(), "nothing is left held");
    assert_eq!(records_of(&audit, VOICE_AUDIT_DECIDED).len(), 1);
    surface.close().await;
}

#[tokio::test]
async fn a_spoken_no_denies_and_nothing_runs() {
    let dir = tempfile::tempdir().unwrap();
    let audit = InMemoryAuditRecorder::new();
    let harness = harness(dir.path(), HarnessMode::Ask, &audit);
    let model = h5_script("no, don't");
    let surface = VoiceSurface::open(
        harness,
        Arc::new(model.clone()),
        sessions().await,
        USER,
        SESSION,
        VoiceConfig::new(APP),
    )
    .unwrap();
    surface.connect().await.unwrap();
    surface.say("run it").await.unwrap();
    let events = drive(&surface).await;
    assert!(events.iter().any(|event| matches!(
        event,
        VoiceEvent::Decided { decision: SpokenDecision::Deny, by: DecisionSource::Voice, .. }
    )));
    let resumed = events.iter().find_map(|event| match event {
        VoiceEvent::Resumed(call) => Some(call.clone()),
        _ => None,
    });
    assert!(resumed.is_some_and(|call| call.result.get("error").is_some()));
    assert!(!dir.path().join("ran.txt").exists(), "a denied command never runs");
    assert_eq!(records_of(&audit, VOICE_AUDIT_RESUMED)[0]["executed"], false);
    surface.close().await;
}

#[tokio::test]
async fn an_unclear_answer_leaves_the_call_held() {
    let dir = tempfile::tempdir().unwrap();
    let audit = InMemoryAuditRecorder::new();
    let harness = harness(dir.path(), HarnessMode::Ask, &audit);
    let model = h5_script("what would that do?");
    let surface = VoiceSurface::open(
        harness,
        Arc::new(model),
        sessions().await,
        USER,
        SESSION,
        VoiceConfig::new(APP),
    )
    .unwrap();
    surface.connect().await.unwrap();
    surface.say("run it").await.unwrap();
    let events = drive(&surface).await;
    assert!(!events.iter().any(|event| matches!(event, VoiceEvent::Decided { .. })));
    assert_eq!(surface.held_calls().len(), 1, "still held");
    assert!(!dir.path().join("ran.txt").exists());
    assert!(records_of(&audit, VOICE_AUDIT_DECIDED).is_empty());
    surface.close().await;
}

#[tokio::test]
async fn the_inbox_decides_a_held_call_too() {
    let dir = tempfile::tempdir().unwrap();
    let audit = InMemoryAuditRecorder::new();
    let harness = harness(dir.path(), HarnessMode::Ask, &audit);
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(scripted::calls(
            "call-1",
            "bash",
            json!({ "command": "printf ran > ran.txt" }),
        ))
        .on_text(),
        ScriptedReply::new(scripted::spoken("Waiting for your approval.")).on_tool_output(),
    ])
    .with_idle_timeout(Duration::from_millis(150));
    let surface = VoiceSurface::open(
        harness,
        Arc::new(model.clone()),
        sessions().await,
        USER,
        SESSION,
        VoiceConfig::new(APP),
    )
    .unwrap();
    surface.connect().await.unwrap();
    surface.say("run it").await.unwrap();
    let mut fingerprint = None;
    while let Some(event) = surface.next_event().await {
        if let VoiceEvent::Held { request, .. } = event.unwrap() {
            fingerprint = Some(request.fingerprint());
        }
    }
    let fingerprint = fingerprint.expect("the call was held");
    // Decided from outside the voice session, as the approvals inbox does.
    let resumed = surface.decide(&fingerprint, SpokenDecision::Approve).await.unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(std::fs::read_to_string(dir.path().join("ran.txt")).unwrap(), "ran");
    assert_eq!(records_of(&audit, VOICE_AUDIT_DECIDED)[0]["by"], "inbox");
    assert!(surface.held_calls().is_empty());
    // Decided already: the fingerprint is no longer held.
    let again = surface.decide(&fingerprint, SpokenDecision::Approve).await.unwrap_err();
    assert_eq!(again.code, "harness.voice.not_held");
    surface.close().await;
}

/// The harness classifies its tools by the agent they belong to; the surface governs calls as
/// the harness's own, so `write_todos` (class Todo) runs in Ask without a prompt, as it does
/// in the standard loop, rather than being decided as an unregistered tool that needs one.
#[tokio::test]
async fn harness_tools_keep_their_classes_on_the_voice_surface() {
    let dir = tempfile::tempdir().unwrap();
    let audit = InMemoryAuditRecorder::new();
    let harness = harness(dir.path(), HarnessMode::Ask, &audit);
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(scripted::calls(
            "call-1",
            "write_todos",
            json!({ "todos": [{ "content": "fix the parser" }] }),
        ))
        .on_text(),
        ScriptedReply::new(scripted::spoken("Planned.")).on_tool_output(),
    ])
    .with_idle_timeout(Duration::from_millis(150));
    let surface = VoiceSurface::open(
        harness,
        Arc::new(model.clone()),
        sessions().await,
        USER,
        SESSION,
        VoiceConfig::new(APP),
    )
    .unwrap();
    surface.connect().await.unwrap();
    surface.say("plan the fix").await.unwrap();
    let events = drive(&surface).await;
    assert!(!events.iter().any(|event| matches!(event, VoiceEvent::Held { .. })), "{events:?}");
    let outputs = model.log().tool_outputs();
    assert_eq!(outputs.len(), 1);
    assert_eq!(
        outputs[0].output["todos"][0]["content"], "fix the parser",
        "{:?}",
        outputs[0].output
    );
    surface.close().await;
}

#[tokio::test]
async fn read_only_mode_denies_the_command_without_a_hold() {
    let dir = tempfile::tempdir().unwrap();
    let audit = InMemoryAuditRecorder::new();
    let harness = harness(dir.path(), HarnessMode::ReadOnly, &audit);
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(scripted::calls(
            "call-1",
            "bash",
            json!({ "command": "printf ran > ran.txt" }),
        ))
        .on_text(),
        ScriptedReply::new(scripted::spoken("I cannot run commands in read-only mode."))
            .on_tool_output(),
    ])
    .with_idle_timeout(Duration::from_millis(150));
    let surface = VoiceSurface::open(
        harness,
        Arc::new(model.clone()),
        sessions().await,
        USER,
        SESSION,
        VoiceConfig::new(APP),
    )
    .unwrap();
    surface.connect().await.unwrap();
    surface.say("run it").await.unwrap();
    let events = drive(&surface).await;
    assert!(!events.iter().any(|event| matches!(event, VoiceEvent::Held { .. })), "{events:?}");
    let outputs = model.log().tool_outputs();
    assert_eq!(outputs.len(), 1);
    assert!(outputs[0].output.get("error").is_some(), "{:?}", outputs[0].output);
    assert!(!adk_core::is_confirmation_pending(&outputs[0].output));
    assert!(!dir.path().join("ran.txt").exists());
    surface.close().await;
}

#[tokio::test]
async fn the_surface_follows_the_harness_memory_policy_and_speaks_the_mode() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "Use pnpm, not npm.\n").unwrap();
    let audit = InMemoryAuditRecorder::new();
    let harness = harness(dir.path(), HarnessMode::Ask, &audit);
    let model = ScriptedRealtimeModel::new(vec![]).with_idle_timeout(Duration::from_millis(50));
    let surface = VoiceSurface::open(
        harness,
        Arc::new(model.clone()),
        sessions().await,
        USER,
        SESSION,
        VoiceConfig::new(APP).with_voice("Kore"),
    )
    .unwrap();
    let config = surface.runner().integration_config();
    assert!(config.persist_transcripts, "transcripts are session events");
    assert!(!config.store_to_memory, "memory is written only through the memory tool (I33)");
    assert!(
        !config.inject_memory_context,
        "recall enters only through the harness instruction (I40)"
    );
    surface.connect().await.unwrap();
    let configs = model.log().configs();
    let instruction = configs[0].instruction.clone().unwrap_or_default();
    assert!(instruction.contains("Harness mode is ask"), "{instruction}");
    assert!(instruction.contains("say yes or no"), "{instruction}");
    // The workspace's instruction files reach the realtime instruction as the harness renders
    // them: an untrusted workspace's files are listed, never inlined (spec 3.1 §7.7, I31).
    assert!(instruction.contains("AGENTS.md"), "{instruction}");
    assert!(!instruction.contains("Use pnpm"), "{instruction}");
    assert_eq!(configs[0].voice.as_deref(), Some("Kore"));
    assert!(configs[0].input_audio_transcription.is_some(), "transcripts are requested");
    let tools: Vec<String> =
        configs[0].tools.iter().flatten().map(|tool| tool.name.clone()).collect();
    assert!(tools.contains(&"bash".to_string()), "{tools:?}");
    assert!(tools.contains(&"read_file".to_string()), "{tools:?}");
    assert_eq!(model.provider(), "scripted");
    surface.close().await;
}

#[tokio::test]
async fn a_harness_without_the_voice_pack_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Arc::new(
        HarnessAgent::builder("harness")
            .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
            .workspace(Workspace::new(dir.path()).allow_bash(true))
            .build()
            .unwrap(),
    );
    let error = VoiceSurface::open(
        harness,
        Arc::new(ScriptedRealtimeModel::new(vec![])),
        sessions().await,
        USER,
        SESSION,
        VoiceConfig::new(APP),
    )
    .expect_err("refused");
    assert_eq!(error.code, "harness.voice.unmarked");
}
