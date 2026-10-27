//! The scenario "One verb, three origins" (spec 3.3 §12, Experimental): ana snoozes News
//! digest by typing in the terminal, by clicking in the home and by voice, and each leaves the
//! same record but for where and how it was given; text that looks like the command, typed in
//! the home or written by an agent, changes nothing.
//!
//! Finance's leg — its request held whatever its mode and rules, its card on every surface,
//! Allow once on the phone, the twins and Stop all — runs in `verbs_tests.rs` (`held::*`,
//! `chat_card::*`, `card_hints::*`), on the same fixture.
//!
//! ```sh
//! cargo nextest run -p adk-surface --features verb-ui,web,tui,voice --test verbs_scenario_tests
//! ```

#![cfg(all(feature = "verb-ui", feature = "web", feature = "tui", feature = "voice"))]

mod verbs_support;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditDecision, AuditRecordDraft};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_surface::Command;
use adk_surface::tui::{CommandContext, TerminalCommand};
use adk_surface::verbs::SnoozeCommand;
use adk_surface::voice::VoiceUpdate;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Duration as Span;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use verbs_support::voice::{hub, says, speak, until};
use verbs_support::*;

/// `record` without what differs by where and how it was given: seq, time, hashes, surface,
/// origin, device, presence, the transcript's digest and the call's id.
fn comparable(mut record: AuditRecordDraft) -> AuditRecordDraft {
    for key in ["surface", "origin", "device", "presence", "transcriptDigest", "functionCallId"] {
        record.ext.remove(key);
    }
    record
}

/// A `tool_call` record without the invocation and the call's id.
fn comparable_call(mut record: AuditRecordDraft) -> AuditRecordDraft {
    record.actor.invocation_id = None;
    record.ext.remove("functionCallId");
    record.ext.remove("functionCall");
    record
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_verb_three_origins() {
    let home = home(Setup::new(Arc::new(ScriptedLlm::new(
        "scripted",
        (0..4).map(|_| ScriptedResponse::text("Noted.")).collect::<Vec<_>>(),
    ))))
    .await;
    let to = home.at + Span::hours(1);

    // 1. Text: `/snooze News digest` in the terminal, a paired local one.
    let terminal_snooze = SnoozeCommand::new(home.gate.clone(), terminal());
    let said = terminal_snooze.run("News digest", &CommandContext::new("t1")).await.unwrap();
    assert_eq!(said, format!("✓ snoozed News digest → {}", time_text(to)));
    assert_eq!(home.wake_of(DIGEST).await, Some(to));
    home.move_to(DIGEST, home.at).await;

    // 2. Click: Coming up → Snooze → "1 hour later" in the home.
    let api = home.api(Some(owner_context()));
    let body = json!({
        "origin": { "kind": "pointer" },
        "intent": {
            "verb": "snooze",
            "objects": [{ "kind": "comingUp", "id": DIGEST }],
            "args": { "to": to.with_timezone(&offset()).to_rfc3339() },
        },
    });
    let request = Request::post("/api/surface/verbs/call")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    assert_eq!(api.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
    assert_eq!(home.wake_of(DIGEST).await, Some(to));
    home.move_to(DIGEST, home.at).await;

    // 3. Voice: "Snooze news digest", not over the model's audio, after the cancel window.
    let hub = hub(&home, vec![says("Snooze news digest")]).await;
    let mut link = hub.open(&web(), "scripted", None).await.unwrap();
    speak(&link).await;
    until(&mut link, "the pending action", |update| {
        matches!(update, VoiceUpdate::PendingAction { .. })
    })
    .await;
    until(&mut link, "the outcome", |update| {
        matches!(update, VoiceUpdate::ActionOutcome { done: true, .. })
    })
    .await;
    assert_eq!(home.wake_of(DIGEST).await, Some(to));

    // 4. Compare: the same record but for where and how.
    let records = home.verb_records();
    assert_eq!(records.len(), 3, "{records:?}");
    let hows: Vec<(Value, Value)> = records
        .iter()
        .map(|record| (record.ext["surface"].clone(), record.ext["origin"].clone()))
        .collect();
    assert_eq!(
        hows,
        [
            (json!("terminal"), json!({ "kind": "keyboard" })),
            (json!("web"), json!({ "kind": "pointer" })),
            (json!("voice"), json!({ "kind": "voice" })),
        ]
    );
    let digest = format!("{:x}", Sha256::digest("Snooze news digest".as_bytes()));
    assert_eq!(records[2].ext["transcriptDigest"], json!(digest));
    let same: Vec<AuditRecordDraft> = records.into_iter().map(comparable).collect();
    assert_eq!(same[0], same[1]);
    assert_eq!(same[1], same[2]);
    assert_eq!(same[0].ext["verb"], json!("snooze"));
    assert_eq!(same[0].ext["target"], json!(format!("agenda:{DIGEST}")));
    assert_eq!(same[0].ext["effect"], json!("write_local"));
    assert_eq!(same[0].ext["outcome"], json!("done"));
    assert_eq!(same[0].actor.on_behalf_of.as_ref().unwrap().id, OWNER);
    let calls: Vec<AuditRecordDraft> =
        home.tool_records("verb_snooze").into_iter().map(comparable_call).collect();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0], calls[1]);
    assert_eq!(calls[1], calls[2]);
    assert_eq!(calls[0].decision, Some(AuditDecision::Allowed));
    assert_eq!(
        calls[0].actor.agent.as_ref().map(|agent| agent.as_str()),
        Some("urn:adk:agent:adk-rust/surface.verbs")
    );
    let written = format!("{:?}", home.audit.records());
    assert!(!written.to_lowercase().contains("snooze news digest"), "never the words");

    // 6 (c). "/snooze News digest" typed in the home's message box is a message, nothing more.
    home.move_to(DIGEST, home.at).await;
    home.engine
        .execute(
            &web(),
            Command::Send { session_id: "s1".into(), text: "/snooze News digest".into() },
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(home.wake_of(DIGEST).await, Some(home.at));
    assert_eq!(home.verb_records().len(), 3, "no fourth record");
}
