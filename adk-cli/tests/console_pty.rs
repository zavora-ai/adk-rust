//! `adk-rust chat` in a pseudo-terminal: held calls are shown and answered at the console
//! (spec 3.1 §8.1, §11 adk-cli row). The model is scripted, so no provider key is needed.

#![cfg(all(unix, feature = "scripted-model"))]

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use common::Terminal;

const RECORD_THEN_ANSWER: &str = r#"[
    {"call": {"name": "record_note", "args": {"note": "ship it"}}},
    {"text": "Done with the note."}
]"#;

fn chat(home: &Path, script: &str) -> Terminal {
    Terminal::spawn(&["chat", "--ephemeral", "--approval-fixture", "--model-script", script], home)
}

#[test]
fn a_held_call_is_shown_and_y_runs_it() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = chat(home.path(), RECORD_THEN_ANSWER);
    terminal.expect("You >");
    terminal.send("please record a note\r");
    terminal.expect(r#"[approval needed] record_note {"note":"ship it"}"#);
    terminal.expect("[y] allow once  [n] deny (reason)  [e] edit args  [?] details >");
    terminal.send("y\r");
    terminal.expect("[approved] record_note");
    terminal.expect(r#"[tool-response] record_note {"recorded":"ship it"}"#);
    terminal.expect("Done with the note.");
    terminal.expect("You >");
    terminal.finish();
}

#[test]
fn n_denies_the_call_and_the_model_hears_the_reason() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = chat(home.path(), RECORD_THEN_ANSWER);
    terminal.expect("You >");
    terminal.send("please record a note\r");
    terminal.expect("[approval needed] record_note");
    terminal.expect("details >");
    terminal.send("n\r");
    terminal.expect("reason (optional) >");
    terminal.send("not today\r");
    terminal.expect("[denied] record_note");
    terminal.expect("the approver declined it: not today");
    terminal.expect("Done with the note.");
    let output = terminal.finish();
    assert!(!output.contains(r#""recorded""#), "the denied call ran:\n{output}");
}

#[test]
fn e_edits_the_arguments_and_y_runs_the_edited_call() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = chat(home.path(), RECORD_THEN_ANSWER);
    terminal.expect("You >");
    terminal.send("please record a note\r");
    terminal.expect("[approval needed] record_note");
    terminal.expect("details >");
    terminal.send("e\r");
    terminal.expect("arguments (JSON) >");
    terminal.send("{\"note\": \"ship it tomorrow\"}\r");
    terminal.expect(r#"edited: record_note {"note":"ship it tomorrow"}"#);
    terminal.expect("details >");
    terminal.send("y\r");
    terminal.expect("[approved] record_note");
    terminal.expect(r#"[tool-response] record_note {"recorded":"ship it tomorrow"}"#);
    let output = terminal.finish();
    assert!(!output.contains(r#"{"recorded":"ship it"}"#), "the original call ran:\n{output}");
}

#[test]
fn ctrl_c_at_the_prompt_denies_and_cancels_the_run() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = chat(home.path(), RECORD_THEN_ANSWER);
    terminal.expect("You >");
    terminal.send("please record a note\r");
    terminal.expect("[approval needed] record_note");
    terminal.expect("[y] allow once");
    terminal.send("\x03");
    terminal.expect("denied; the run is interrupted");
    terminal.expect("Interrupted.");
    // The REPL reads again from the same reader: nothing was left blocking on stdin.
    terminal.expect("You >");
    let output = terminal.finish();
    assert!(!output.contains(r#""recorded""#), "the call ran:\n{output}");
    assert!(!output.contains("Done with the note."), "the run continued:\n{output}");
    assert!(output.contains("Goodbye."), "{output}");
}

#[test]
fn without_a_terminal_the_held_call_is_denied() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_adk-rust"))
        .args(["chat", "--ephemeral", "--approval-fixture", "--model-script", RECORD_THEN_ANSWER])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("RUST_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"please record a note\ny\n").unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("there is no terminal to ask"), "{stdout}");
    assert!(stdout.contains("[denied] record_note"), "{stdout}");
    assert!(!stdout.contains(r#""recorded""#), "the call ran without approval:\n{stdout}");
}

#[cfg(feature = "persist")]
#[test]
fn continue_resumes_the_latest_session() {
    let home = tempfile::tempdir().unwrap();
    let script = r#"[{"text": "First answer."}]"#;
    let mut first =
        Terminal::spawn(&["chat", "--session", "notes", "--model-script", script], home.path());
    first.expect("Session  : notes");
    first.expect("You >");
    first.send("hello\r");
    first.expect("First answer.");
    first.finish();

    let mut resumed =
        Terminal::spawn(&["chat", "--continue", "--model-script", script], home.path());
    resumed.expect("Session  : notes");
    resumed.finish();

    let mut fresh = Terminal::spawn(&["chat", "--model-script", script], home.path());
    fresh.expect("Session  : ");
    let output = fresh.finish();
    assert!(!output.contains("Session  : notes"), "{output}");
}
