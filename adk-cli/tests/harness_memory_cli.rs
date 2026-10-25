//! Long-term memory in `adk-rust` with a scripted model (spec 3.1 §7.15.2, §10 L2): a
//! session ends distilled into `harness.db`, `adk-rust memory list|show|forget|accept|
//! export|import`, `/memory` in the REPL, the one-time notice, and `--no-memory`.

#![cfg(all(unix, feature = "scripted-model", feature = "harness", feature = "persist"))]

mod common;

use std::path::Path;

use common::{Terminal, repl, run, text};

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "a project\n").unwrap();
    dir
}

fn dir_arg(dir: &Path) -> String {
    dir.display().to_string()
}

/// A turn's reply, then the distiller's answer at the end of the session.
fn script(reply: &str) -> String {
    let facts = serde_json::json!({ "facts": [
        { "text": "Use pnpm, not npm, to add dependencies", "kind": "preference",
          "source": "user_stated", "quote": "use pnpm, not npm", "confidence": 0.95, "supersedes": "" },
        { "text": "The test suite needs --features sqlite", "kind": "procedure",
          "source": "tool_observed", "quote": "", "confidence": 0.8, "supersedes": "" }
    ]});
    serde_json::json!([{ "text": reply }, { "text": facts.to_string() }]).to_string()
}

fn memory(args: &[&str], home: &Path, cwd: &Path) -> String {
    let mut all = vec!["memory"];
    all.extend_from_slice(args);
    let output = run(&all, home, cwd);
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    shown
}

/// The id at the start of the listed line containing `needle`.
fn id_of(listing: &str, needle: &str) -> String {
    let line = listing
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} in {listing}"));
    line.split_whitespace().next().unwrap().to_string()
}

#[test]
fn a_session_is_distilled_and_its_facts_are_managed_from_the_command_line() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let output = run(
        &[
            "code",
            "From now on use pnpm, not npm, here.",
            "--dir",
            &dir_arg(ws.path()),
            "--model-script",
            &script("Noted."),
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("Long-term memory is on."), "the notice is shown: {shown}");
    assert!(shown.contains("harness.db"), "it says where facts live: {shown}");

    let listed = memory(&["list", "--dir", &dir_arg(ws.path())], home.path(), ws.path());
    assert!(listed.contains("known · preference"), "{listed}");
    assert!(listed.contains("user_stated in session"), "{listed}");
    assert!(listed.contains("assumed · procedure"), "{listed}");
    assert!(
        listed.find("known ·").unwrap() < listed.find("assumed ·").unwrap(),
        "known first: {listed}"
    );
    let known = id_of(&listed, "known · preference");
    let assumed = id_of(&listed, "assumed · procedure");

    let shown = memory(&["show", &known[..16]], home.path(), ws.path());
    assert!(shown.contains("\"quote\": \"use pnpm, not npm\""), "{shown}");

    memory(&["accept", &assumed], home.path(), ws.path());
    let listed = memory(&["list", "--dir", &dir_arg(ws.path())], home.path(), ws.path());
    assert!(
        listed.contains("known · procedure · ") && listed.contains("user_confirmed"),
        "{listed}"
    );

    memory(&["forget", &known], home.path(), ws.path());
    let listed = memory(&["list", "--dir", &dir_arg(ws.path())], home.path(), ws.path());
    assert!(!listed.contains(&known), "{listed}");
    let all = memory(&["list", "--all"], home.path(), ws.path());
    assert!(all.contains(&format!("{known} forgotten")), "history is kept: {all}");

    let export = home.path().join("facts.jsonl");
    memory(&["export", "--out", &export.display().to_string()], home.path(), ws.path());
    assert_eq!(std::fs::read_to_string(&export).unwrap().lines().count(), 2);
    let imported = memory(&["import", &export.display().to_string()], home.path(), ws.path());
    assert!(imported.contains("1 facts imported as assumed, 1 skipped"), "{imported}");

    // A second run neither repeats the notice nor loses the facts.
    let again = run(
        &[
            "code",
            "hello",
            "--dir",
            &dir_arg(ws.path()),
            "--model-script",
            r#"[{"text": "Hi."}, {"text": "{\"facts\": []}"}]"#,
        ],
        home.path(),
        ws.path(),
    );
    assert!(!text(&again).contains("Long-term memory is on."), "{}", text(&again));
}

#[test]
fn no_memory_learns_nothing() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let output = run(
        &[
            "code",
            "use pnpm, not npm",
            "--no-memory",
            "--dir",
            &dir_arg(ws.path()),
            "--model-script",
            &script("Noted."),
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(!shown.contains("Long-term memory is on."), "{shown}");
    let listed = memory(&["list", "--all"], home.path(), ws.path());
    assert!(listed.contains("No facts yet"), "{listed}");
}

#[test]
fn the_repl_lists_and_forgets_facts() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    run(
        &[
            "code",
            "use pnpm, not npm",
            "--dir",
            &dir_arg(ws.path()),
            "--model-script",
            &script("Noted."),
        ],
        home.path(),
        ws.path(),
    );
    let listed = memory(&["list", "--dir", &dir_arg(ws.path())], home.path(), ws.path());
    let known = id_of(&listed, "known · preference");
    let mut terminal = Terminal::spawn_in(
        &repl(&[
            "--dir",
            &dir_arg(ws.path()),
            "--model-script",
            r#"[{"text": "{\"facts\": []}"}]"#,
        ]),
        home.path(),
        ws.path(),
    );
    terminal.expect("Memory    : long-term facts on");
    terminal.expect("You >");
    terminal.send("/memory\r");
    terminal.expect("Working notes:");
    terminal.expect("Long-term facts");
    terminal.expect(&format!("{known} known · preference"));
    terminal.expect("Use pnpm, not npm, to add dependencies");
    terminal.send(&format!("/memory forget {}\r", &known[..16]));
    terminal.expect("forgotten: Use pnpm, not npm");
    terminal.send("/memory\r");
    terminal.expect("superseded or forgotten, kept as history");
    let shown = terminal.finish();
    let after = shown.rsplit("forgotten: Use pnpm").next().unwrap();
    assert!(!after.contains(&format!("{known} known")), "{shown}");
}
