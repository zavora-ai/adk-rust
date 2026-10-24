//! The harness CLI's hooks, completion and console-ordering surface, end to end with a
//! scripted model (spec 3.1 §8.1, §7.21, §7.22, §11 adk-cli row): `/hooks`, hook notices,
//! `--no-hooks`, user-config hooks, Tab completion in a pseudo-terminal, the `--yes` line
//! order under a durable goal, and the `--pack payments` refusal.

#![cfg(all(unix, feature = "scripted-model", feature = "harness", feature = "persist"))]

mod common;

use std::path::Path;

use common::{Terminal, run, text};

const READ_README: &str = r#"[
    {"call": {"name": "read_file", "args": {"path": "README.md"}}},
    {"text": "Read it."}
]"#;

const WORKSPACE_HOOKS: &str = r#"
[[hooks]]
event = "on_stop"
command = "echo '{\"decision\":\"annotate\",\"text\":\"turn done: HOOK-NOTE-1\"}'"
"#;

const USER_HOOKS: &str = r#"
[[hooks]]
event = "post_tool"
matcher = "read_file"
command = "echo '{\"decision\":\"annotate\",\"text\":\"USER-NOTE-2\"}'"
"#;

/// A workspace with instruction files and the hooks above.
fn workspace_with_hooks() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "a project\n").unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "Be brief.\n").unwrap();
    std::fs::create_dir_all(dir.path().join(".adk")).unwrap();
    std::fs::write(dir.path().join(".adk/harness.toml"), WORKSPACE_HOOKS).unwrap();
    dir
}

/// Writes the user's `harness.toml` under the private home `common::isolate` points at.
fn write_user_config(home: &Path, text: &str) {
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/adk-rust")
    } else {
        home.join(".config/adk-rust")
    };
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("harness.toml"), text).unwrap();
}

fn dir_arg(dir: &Path) -> String {
    dir.display().to_string()
}

#[test]
#[cfg_attr(feature = "tui", ignore = "`tui` replaces the REPL with the terminal surface")]
fn hooks_are_listed_with_their_source_and_an_on_stop_notice_reaches_the_user() {
    let home = tempfile::tempdir().unwrap();
    write_user_config(home.path(), USER_HOOKS);
    let ws = workspace_with_hooks();
    let mut terminal = Terminal::spawn_in(
        &["--dir", &dir_arg(ws.path()), "--trust", "--ephemeral", "--model-script", READ_README],
        home.path(),
        ws.path(),
    );
    terminal.expect("Hooks     : 2 (/hooks lists them)");
    terminal.expect("You >");
    terminal.send("/hooks\r");
    terminal.expect("post_tool [read_file]: echo");
    terminal.expect("user · inherit · 30 s · runs");
    terminal.expect("on_stop: echo");
    terminal.expect("workspace · inherit · 30 s · runs");
    terminal.expect("bounds: 8 per event, 60 s per turn");
    terminal.send("read the readme\r");
    // The user hook's note reaches the model inside the tool result; the workspace's
    // on_stop note reaches the user as a notice.
    terminal.expect("[tool-response] read_file");
    terminal.expect("USER-NOTE-2");
    terminal.expect("Read it.");
    terminal.expect("[hook:workspace] turn done: HOOK-NOTE-1");
    terminal.expect("You >");
    terminal.finish();
}

#[test]
#[cfg_attr(feature = "tui", ignore = "`tui` replaces the REPL with the terminal surface")]
fn no_hooks_runs_none_and_hooks_says_so() {
    let home = tempfile::tempdir().unwrap();
    write_user_config(home.path(), USER_HOOKS);
    let ws = workspace_with_hooks();
    let output = run(
        &[
            "code",
            "read the readme",
            "--dir",
            &dir_arg(ws.path()),
            "--trust",
            "--ephemeral",
            "--no-hooks",
            "--model-script",
            READ_README,
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("[tool-response] read_file"), "{shown}");
    assert!(!shown.contains("HOOK-NOTE-1"), "the workspace hook ran under --no-hooks:\n{shown}");
    assert!(!shown.contains("USER-NOTE-2"), "the user hook ran under --no-hooks:\n{shown}");

    let mut terminal = Terminal::spawn_in(
        &[
            "--dir",
            &dir_arg(ws.path()),
            "--trust",
            "--ephemeral",
            "--no-hooks",
            "--model-script",
            "[]",
        ],
        home.path(),
        ws.path(),
    );
    terminal.expect("Hooks     : off (--no-hooks)");
    terminal.expect("You >");
    terminal.send("/hooks\r");
    terminal.expect("Hooks are off (--no-hooks).");
    terminal.finish();
}

#[test]
#[cfg_attr(feature = "tui", ignore = "`tui` replaces the REPL with the terminal surface")]
fn an_untrusted_workspace_hook_is_listed_as_not_running() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace_with_hooks();
    let mut terminal = Terminal::spawn_in(
        &["--dir", &dir_arg(ws.path()), "--ephemeral", "--model-script", READ_README],
        home.path(),
        ws.path(),
    );
    // The trust prompt lists each workspace hook with its command (spec 3.1 §9.2 I54).
    terminal.expect("Hooks in .adk/harness.toml");
    terminal.expect("on_stop: echo");
    terminal.expect("Trust them? [y/N] >");
    terminal.send("n\r");
    terminal.expect("You >");
    terminal.send("/hooks\r");
    terminal.expect("on_stop: echo");
    terminal.expect("does not run: the workspace is not trusted");
    terminal.send("read the readme\r");
    terminal.expect("Read it.");
    terminal.expect("You >");
    let output = terminal.finish();
    assert!(!output.contains("[hook:workspace]"), "an untrusted workspace hook ran:\n{output}");
}

#[test]
fn a_hooks_table_in_the_project_file_is_not_an_unknown_key() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace_with_hooks();
    let output = run(
        &[
            "code",
            "read",
            "--dir",
            &dir_arg(ws.path()),
            "--trust",
            "--ephemeral",
            "--model-script",
            "[]",
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&output);
    assert!(!shown.contains("unknown key `hooks`"), "{shown}");
}

#[test]
#[cfg_attr(feature = "tui", ignore = "`tui` replaces the REPL with the terminal surface")]
fn tab_completes_a_slash_command() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let mut terminal = Terminal::spawn_in(
        &["--dir", &dir_arg(ws.path()), "--ephemeral", "--model-script", "[]"],
        home.path(),
        ws.path(),
    );
    terminal.expect("You >");
    terminal.send("/mo\t");
    terminal.expect("/mode");
    terminal.send("\r");
    terminal.expect("mode: ask");
    terminal.finish();
}

/// Under a durable goal the console reads the run's events from the session while the
/// worker runs it, so a decision printed by `--yes` could precede the call it belongs to
/// (spec 3.1 §8.1).
#[test]
fn yes_decisions_follow_their_tool_call_lines_under_a_durable_goal() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let write = r#"[
        {"call": {"name": "write_file", "args": {"path": "done.txt", "content": "ok"}}},
        {"text": "Created."}
    ]"#;
    let output = run(
        &[
            "goal",
            "create done.txt",
            "--until",
            "test -f done.txt",
            "--yes",
            "--dir",
            &dir_arg(ws.path()),
            "--model-script",
            write,
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    let call = shown.find("[tool-call] write_file").expect(&shown);
    let approved = shown.find("[approved by --yes] write_file").expect(&shown);
    let response = shown.find("[tool-response] write_file").expect(&shown);
    assert!(call < approved && approved < response, "{shown}");
}

#[test]
fn pack_payments_is_refused_with_the_library_path() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let output = run(
        &[
            "code",
            "buy",
            "--pack",
            "payments",
            "--dir",
            &dir_arg(ws.path()),
            "--ephemeral",
            "--model-script",
            "[]",
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&output);
    assert!(!output.status.success(), "{shown}");
    assert!(shown.contains("pack_payments(PaymentsPack)"), "{shown}");
    assert!(shown.contains("payments.md"), "{shown}");
}
