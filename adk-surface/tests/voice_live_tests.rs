//! A spoken "allow once" on Gemini Live (spec 3.3 §5.6): the person's recorded words are
//! transcribed by the live model, bound to the decision prompted on screen, and allowed once
//! after the cancel window, with the transcript's digest on record and never its words.
//!
//! Needs `GOOGLE_API_KEY` (or `GEMINI_API_KEY`), read from the repository's `.env` or the file
//! `ADK_ENV_FILE` names, and a 16 kHz mono WAV of someone saying "allow once" in
//! `ADK_VOICE_WAV` (on macOS: `say -r 150 -o allow-once.wav --file-format=WAVE
//! --data-format=LEI16@16000 "Allow, once, please."`):
//!
//! ```text
//! ADK_VOICE_WAV=allow-once.wav cargo test -p adk-surface --features web,voice-live \
//!     --test voice_live_tests -- --ignored --nocapture
//! ```

#![cfg(feature = "voice-live")]

mod voice_common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ActingContext, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalState,
    ApproverSelector, SubjectKind, ToolConfirmationRequest,
};
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_session::CreateRequest;
use adk_surface::voice::{VoiceInput, VoiceOutput, VoiceUpdate};
use serde_json::json;
use voice_common::*;

const MODEL: &str = "gemini-3.1-flash-live-preview";

fn key() -> String {
    let env = std::env::var_os("ADK_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env"));
    if env.exists() {
        dotenvy::from_path_override(&env).expect("the .env file loads");
    }
    std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY")
}

/// The PCM16 samples of a 16 kHz mono WAV.
fn pcm(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("ADK_VOICE_WAV is readable");
    let data = bytes.windows(4).position(|window| window == b"data").expect("a data chunk") + 8;
    bytes[data..].to_vec()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "calls Gemini Live"]
async fn a_spoken_allow_once_on_gemini_live() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let wav = PathBuf::from(std::env::var("ADK_VOICE_WAV").expect("ADK_VOICE_WAV"));
    let live = Arc::new(GeminiRealtimeModel::new(GeminiLiveBackend::studio(key()), MODEL));
    let voice = voice_with(Vec::new(), move |_| -> adk_realtime::BoxedModel { live }).await;

    // Finance's call waits in the inbox, as in the scenario of §12 phase 5.
    voice
        .sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: Some("finance".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let call = ToolConfirmationRequest {
        tool_name: "send_email".into(),
        function_call_id: Some("finance-1".into()),
        args: json!({ "to": "[CLIENT]", "subject": "October invoice" }),
        origin: None,
        acting: None,
    };
    let mut request = ApprovalRequest::new(
        "apr-finance",
        ApprovalScope::new(APP, OWNER, "finance"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    )
    .with_risk(ApprovalRisk::Medium);
    request.acting = Some(ActingContext::derived(APP, "finance", OWNER, SubjectKind::Human));
    let request = voice.service.submit(request).await.unwrap();

    let mut link = voice.hub.open(&web(), "scripted", None).await.unwrap();
    let (id, _) = prompted(&mut link).await;
    assert_eq!(id, request.id);
    tokio::time::sleep(DWELL).await;

    // A second of silence, the recording in 100 ms chunks, then silence, as a microphone
    // sends them: the model's activity detection finds the speech between and ends the turn.
    let mut audio = vec![0u8; 16_000 * 2];
    audio.extend(pcm(&wav));
    audio.extend(std::iter::repeat_n(0u8, 16_000 * 2 * 4));
    for chunk in audio.chunks(3_200) {
        link.input.send(VoiceInput::Audio(chunk.to_vec())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut heard = Vec::new();
    let outcome = loop {
        let next = tokio::time::timeout_at(deadline, link.output.recv())
            .await
            .expect("an outcome within a minute")
            .expect("the conversation stays open");
        match next {
            VoiceOutput::Update(VoiceUpdate::Heard { text }) => {
                println!("heard: {text}");
                heard.push(text);
            }
            VoiceOutput::Update(update @ VoiceUpdate::Pending { .. }) => println!("{update:?}"),
            VoiceOutput::Update(update @ VoiceUpdate::Outcome { .. }) => break update,
            VoiceOutput::Update(VoiceUpdate::Error { code, message }) => {
                println!("error {code}: {message}")
            }
            VoiceOutput::Update(update) => println!("{update:?}"),
            VoiceOutput::Audio(_) => {}
        }
    };
    println!("outcome: {outcome:?}");
    assert_eq!(
        outcome,
        VoiceUpdate::Outcome { decision_id: id.clone(), decided: true, text: None }
    );
    assert_eq!(voice.service.get(&id).await.unwrap().unwrap().state, ApprovalState::Approved);
    let records = decide_records(&voice.audit);
    let said = heard.last().expect("the person was heard");
    for record in &records {
        assert_eq!(record.ext["transcriptDigest"], json!(digest(said)), "{record:?}");
        assert!(!format!("{record:?}").contains(said.as_str()), "a record holds the words");
    }
    let outcomes: Vec<_> = records.iter().map(|record| record.ext["outcome"].clone()).collect();
    assert_eq!(outcomes, [json!("pending"), json!("decided")]);
    let card = voice.engine.decision(&web(), &id).await.unwrap();
    println!("card: {}", card.decided.unwrap().text);
    drop(link);
}
