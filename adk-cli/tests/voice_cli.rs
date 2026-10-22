//! `adk-rust --voice` end to end with a scripted realtime provider and a scripted model
//! (spec 3.1 §8.1, §7.17, H5): a recorded answer approves the held `bash`, the command runs
//! once, and the transcript and the decision are printed.

#![cfg(all(unix, feature = "scripted-model", feature = "voice"))]

mod common;

use std::path::Path;

use adk_realtime::scripted::{self, ScriptedReply, ScriptedStep};
use common::{run, text};
use serde_json::json;

fn dir_arg(dir: &Path) -> String {
    dir.display().to_string()
}

/// A 16 kHz mono PCM16 WAV of `ms` milliseconds of silence.
fn silence_wav(ms: u32) -> Vec<u8> {
    let pcm = vec![0u8; (16_000 * 2 * ms / 1000) as usize];
    let data_len = pcm.len() as u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&pcm);
    wav
}

/// The H5 script: the model calls `bash`, is told the call waits, hears `answer`, and
/// closes with a spoken result.
fn script(answer: &str) -> Vec<ScriptedStep> {
    vec![
        ScriptedReply::new(scripted::calls(
            "call-1",
            "bash",
            json!({ "command": "touch spoken-approval" }),
        ))
        .on_text(),
        ScriptedReply::new(scripted::spoken("That needs your approval. Say yes or no."))
            .on_tool_output(),
        ScriptedReply::new(vec![scripted::input_transcript(answer)]).on_audio(),
        ScriptedReply::new(scripted::spoken("Done, the file is there.")).on_tool_output(),
    ]
}

fn voice(ws: &Path, home: &Path, answer: &str) -> std::process::Output {
    let script_path = ws.join("voice-script.json");
    std::fs::write(&script_path, serde_json::to_string(&script(answer)).unwrap()).unwrap();
    let wav_path = ws.join("answer.wav");
    std::fs::write(&wav_path, silence_wav(300)).unwrap();
    run(
        &[
            "--voice",
            "what's failing in the build?",
            "--voice-script",
            &script_path.display().to_string(),
            "--voice-input",
            &wav_path.display().to_string(),
            "--voice-output",
            &ws.join("reply.wav").display().to_string(),
            "--dir",
            &dir_arg(ws),
            "--ephemeral",
            "--model-script",
            "[]",
        ],
        home,
        ws,
    )
}

#[test]
fn a_recorded_yes_approves_the_held_bash_and_it_runs_once() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let output = voice(ws.path(), home.path(), "yes");
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("[you] what's failing in the build?"), "{shown}");
    assert!(shown.contains("approval needed: bash"), "{shown}");
    assert!(shown.contains("[you] yes"), "{shown}");
    assert!(shown.contains("decided (voice): approve"), "{shown}");
    assert!(shown.contains("ran: bash"), "{shown}");
    assert!(shown.contains("[harness] Done, the file is there."), "{shown}");
    assert!(ws.path().join("spoken-approval").exists(), "the approved command ran:\n{shown}");
    assert!(!ws.path().join("reply.wav").exists(), "a scripted provider speaks no audio");
}

#[test]
fn a_recorded_no_denies_and_the_command_never_runs() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let output = voice(ws.path(), home.path(), "no");
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("decided (voice): deny"), "{shown}");
    assert!(!ws.path().join("spoken-approval").exists(), "a denied command ran:\n{shown}");
}

#[test]
fn an_unclear_answer_leaves_the_call_held_and_exits_3() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let output = voice(ws.path(), home.path(), "what would that do?");
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(3), "{shown}");
    assert!(shown.contains("still held for a decision"), "{shown}");
    assert!(!ws.path().join("spoken-approval").exists(), "{shown}");
}
