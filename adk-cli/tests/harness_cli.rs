//! `adk-rust` on the harness, end to end with a scripted model (spec 3.1 §8.1, §11 adk-cli
//! row): the REPL in a pseudo-terminal, `code`, `goal`, `approvals`, `rules`.

#![cfg(all(unix, feature = "scripted-model", feature = "harness", feature = "persist"))]

mod common;

use std::path::Path;

use common::{Terminal, run, text};

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "a project\n").unwrap();
    dir
}

fn dir_arg(dir: &Path) -> String {
    dir.display().to_string()
}

/// The `session <id>` an exit-3 message names.
fn held_session(output: &str) -> String {
    let after = output.split("held for a decision in session ").nth(1).expect(output);
    after.trim_end_matches(|c: char| c.is_whitespace()).split('.').next().unwrap().to_string()
}

#[test]
fn code_in_plan_mode_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let script = r#"[
        {"call": {"name": "write_file", "args": {"path": "notes.txt", "content": "hi"}}},
        {"call": {"name": "bash", "args": {"command": "touch made-by-bash"}}},
        {"text": "Planned."}
    ]"#;
    let output = run(
        &[
            "code",
            "write a note",
            "--mode",
            "plan",
            "--dir",
            &dir_arg(ws.path()),
            "--model-script",
            script,
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("mode plan"), "{shown}");
    assert!(!ws.path().join("notes.txt").exists(), "plan mode wrote a file:\n{shown}");
    assert!(!ws.path().join("made-by-bash").exists(), "plan mode ran bash:\n{shown}");
}

#[test]
fn the_read_only_alias_is_read_only_mode() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let script = r#"[
        {"call": {"name": "write_file", "args": {"path": "notes.txt", "content": "hi"}}},
        {"text": "Done."}
    ]"#;
    let output = run(
        &[
            "code",
            "write",
            "--read-only",
            "--ephemeral",
            "--dir",
            &dir_arg(ws.path()),
            "--model-script",
            script,
        ],
        home.path(),
        ws.path(),
    );
    assert!(text(&output).contains("mode read_only"), "{}", text(&output));
    assert!(!ws.path().join("notes.txt").exists());
}

#[test]
fn held_calls_exit_3_and_run_once_after_approval() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = dir_arg(ws.path());
    let bash = r#"[
        {"call": {"name": "bash", "args": {"command": "echo run >> count.txt"}}},
        {"text": "Asked."}
    ]"#;

    // Without a terminal and without --yes the call is held, not run.
    let held =
        run(&["code", "count", "--dir", &dir, "--model-script", bash], home.path(), ws.path());
    let shown = text(&held);
    assert_eq!(held.status.code(), Some(3), "{shown}");
    assert!(shown.contains("bash"), "{shown}");
    assert!(!ws.path().join("count.txt").exists(), "a held call ran:\n{shown}");
    let session = held_session(&shown);

    let listed = run(&["approvals", "list"], home.path(), ws.path());
    let listing = text(&listed);
    let line = listing.lines().find(|line| line.contains("bash")).expect(&listing);
    let id = line.split_whitespace().next().unwrap().to_string();
    assert!(line.contains(&session), "{listing}");

    let approved = run(&["approvals", "approve", &id], home.path(), ws.path());
    assert!(approved.status.success(), "{}", text(&approved));

    // The next run in the session resumes the approved call before the model speaks.
    let resumed = run(
        &[
            "code",
            "continue",
            "--dir",
            &dir,
            "--session",
            &session,
            "--model-script",
            r#"[{"text": "Resumed."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    assert_eq!(resumed.status.code(), Some(0), "{}", text(&resumed));
    assert_eq!(std::fs::read_to_string(ws.path().join("count.txt")).unwrap(), "run\n");

    // Once more: the approval was consumed, nothing runs twice.
    let again = run(
        &[
            "code",
            "anything else?",
            "--dir",
            &dir,
            "--session",
            &session,
            "--model-script",
            r#"[{"text": "Nothing."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    assert_eq!(again.status.code(), Some(0), "{}", text(&again));
    assert_eq!(std::fs::read_to_string(ws.path().join("count.txt")).unwrap(), "run\n");
    let nothing = text(&run(&["approvals", "list"], home.path(), ws.path()));
    assert!(nothing.contains("No calls are waiting"), "{nothing}");
}

#[test]
fn a_session_refuses_another_workspace() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let other = workspace();
    let first = run(
        &[
            "code",
            "hi",
            "--dir",
            &dir_arg(ws.path()),
            "--session",
            "pinned",
            "--model-script",
            r#"[{"text": "Hello."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    assert!(first.status.success(), "{}", text(&first));
    let moved = run(
        &[
            "code",
            "hi",
            "--dir",
            &dir_arg(other.path()),
            "--session",
            "pinned",
            "--model-script",
            r#"[{"text": "Hello."}]"#,
        ],
        home.path(),
        other.path(),
    );
    assert!(!moved.status.success());
    assert!(text(&moved).contains("works in"), "{}", text(&moved));
}

#[test]
fn goal_resume_continues_the_same_conversation() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = dir_arg(ws.path());
    let until = "test -f done.txt";

    let first = run(
        &[
            "goal",
            "create done.txt",
            "--until",
            until,
            "--dir",
            &dir,
            "--max-iters",
            "1",
            "--yes",
            "--model-script",
            r#"[{"text": "Thinking about it."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    assert_eq!(first.status.code(), Some(1), "{}", text(&first));
    assert!(text(&first).contains("budget exhausted"), "{}", text(&first));

    let second = run(
        &[
            "goal",
            "create done.txt",
            "--until",
            until,
            "--dir",
            &dir,
            "--max-iters",
            "2",
            "--yes",
            "--resume",
            "--model-script",
            r#"[{"call": {"name": "write_file", "args": {"path": "done.txt", "content": "ok"}}}, {"text": "Created."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&second);
    assert_eq!(second.status.code(), Some(0), "{shown}");
    assert!(shown.contains("resuming goal in session"), "{shown}");
    assert!(shown.contains("goal met after 2 iteration(s)"), "{shown}");

    let session =
        shown.split("resuming goal in session ").nth(1).unwrap().split_whitespace().next().unwrap();
    let history = text(&run(&["sessions", "show", session], home.path(), ws.path()));
    assert!(history.contains("Goal: create done.txt"), "first iteration missing:\n{history}");
    assert!(history.contains("is not passing yet"), "second iteration missing:\n{history}");
    assert!(history.contains("Thinking about it."), "{history}");
}

#[test]
fn goals_need_ask_or_auto() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let output = run(
        &[
            "goal",
            "x",
            "--until",
            "true",
            "--mode",
            "plan",
            "--dir",
            &dir_arg(ws.path()),
            "--model-script",
            "[]",
        ],
        home.path(),
        ws.path(),
    );
    assert!(!output.status.success());
    assert!(text(&output).contains("needs ask or auto"), "{}", text(&output));
}

#[test]
fn the_repl_refuses_the_home_directory_without_dir() {
    let home = tempfile::tempdir().unwrap();
    let home_path = home.path().canonicalize().unwrap();
    let output = run(&["--model-script", "[]"], &home_path, &home_path);
    assert!(!output.status.success());
    assert!(text(&output).contains("--dir"), "{}", text(&output));
}

#[test]
fn always_allow_remembers_a_rule_and_the_next_call_runs_without_asking() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let script = r#"[
        {"call": {"name": "bash", "args": {"command": "echo hello"}}},
        {"text": "First done."},
        {"call": {"name": "bash", "args": {"command": "echo hello"}}},
        {"text": "Second done."}
    ]"#;
    let mut terminal = Terminal::spawn_in(
        &["--dir", &dir_arg(ws.path()), "--model-script", script],
        home.path(),
        ws.path(),
    );
    terminal.expect("Mode      : ask");
    terminal.expect("You >");
    terminal.send("say hello\r");
    terminal.expect("[approval needed] bash on host");
    terminal.expect("$ echo hello");
    terminal.expect("[a] always allow");
    terminal.send("a\r");
    terminal.expect("[1] bash: echo hello … (every session)");
    terminal.send("1\r");
    terminal.expect("First done.");
    terminal.expect("You >");
    terminal.send("again\r");
    terminal.expect("Second done.");
    terminal.expect("You >");
    terminal.send("/rules\r");
    terminal.expect("allow bash: echo hello");
    let output = terminal.finish();
    assert_eq!(output.matches("[approval needed]").count(), 1, "{output}");

    let rules = text(&run(&["rules", "list"], home.path(), ws.path()));
    assert!(rules.contains("allow bash: echo hello"), "{rules}");
}

#[test]
fn a_denial_ends_the_turn() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let script = r#"[
        {"call": {"name": "bash", "args": {"command": "rm -rf build"}}},
        {"text": "The model kept going."}
    ]"#;
    let mut terminal = Terminal::spawn_in(
        &["--ephemeral", "--dir", &dir_arg(ws.path()), "--model-script", script],
        home.path(),
        ws.path(),
    );
    terminal.expect("You >");
    terminal.send("clean up\r");
    terminal.expect("[approval needed] bash on host");
    terminal.expect("details >");
    terminal.send("n\r");
    terminal.expect("reason (optional) >");
    terminal.send("keep the build\r");
    terminal.expect("Turn ended: you denied a call");
    terminal.expect("You >");
    let output = terminal.finish();
    assert!(!output.contains("The model kept going."), "{output}");
}

#[test]
fn slash_commands_show_and_change_the_session() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let script = r#"[
        {"call": {"name": "write_todos", "args": {"todos": [{"id": "1", "content": "Read the code", "status": "in_progress", "active_form": "Reading the code"}]}}},
        {"text": "Planned."}
    ]"#;
    let mut terminal = Terminal::spawn_in(
        &["--dir", &dir_arg(ws.path()), "--session", "slash", "--model-script", script],
        home.path(),
        ws.path(),
    );
    terminal.expect("Session   : slash");
    terminal.expect("You >");
    terminal.send("/mode plan\r");
    terminal.expect("mode: plan (was ask)");
    terminal.send("/mode\r");
    terminal.expect("mode: plan");
    terminal.send("/sandbox status\r");
    terminal.expect("shell: host");
    // The sandboxed shell is another declaration: nothing granted for the host carries over.
    terminal.send("/sandbox on\r");
    terminal.expect("rules and approvals granted for the other shell do not apply");
    terminal.send("/sandbox off\r");
    terminal.expect("shell: host");
    terminal.send("make a plan\r");
    terminal.expect("Planned.");
    terminal.send("/todos\r");
    terminal.expect("▶ 1. Reading the code");
    terminal.send("/session\r");
    terminal.expect("session   slash");
    terminal.expect("mode      plan");
    terminal.send("/cost\r");
    terminal.expect("model calls");
    terminal.send("/help\r");
    terminal.expect("/approvals");
    terminal.finish();
}
