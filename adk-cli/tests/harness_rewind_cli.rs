//! `/rewind`, `/redo` and `adk-rust sessions rewind` end to end with a scripted model
//! (spec 3.1 §7.27): a turn writes through `write_file` and through `bash`; `/rewind 1`
//! restores both, `/redo` brings them back, and the subcommand rewinds outside the REPL.

#![cfg(all(unix, feature = "scripted-model", feature = "harness", feature = "persist"))]

mod common;

use std::path::Path;

use common::{Terminal, repl, run, text};

const SCRIPT: &str = r#"[
    {"call": {"name": "write_file", "args": {"path": "notes.txt", "content": "from the tool\n"}}},
    {"call": {"name": "bash", "args": {"command": "echo 'from bash' > shell.txt && sed -i.bak 's/old/new/' kept.txt && rm kept.txt.bak"}}},
    {"text": "Wrote the files."}
]"#;

fn read(dir: &Path, path: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(path)).ok()
}

#[test]
fn rewind_restores_tool_and_shell_writes_and_redo_brings_them_back() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("kept.txt"), "old\n").unwrap();
    let dir = ws.path().display().to_string();
    let mut terminal = Terminal::spawn_in(
        &repl(&[
            "--dir",
            &dir,
            "--session",
            "rew",
            "--mode",
            "auto",
            "--yes",
            "--model-script",
            SCRIPT,
        ]),
        home.path(),
        ws.path(),
    );
    terminal.expect("Snapshots : before each turn");
    terminal.expect("You >");
    terminal.send("write the notes and run the script\r");
    terminal.expect("Wrote the files.");
    terminal.send("/rewind\r");
    terminal.expect("3 files changed");
    terminal.send("/rewind 1\r");
    terminal.expect("conflicts");
    terminal.expect("[a] yes, restore them too");
    terminal.send("a\r");
    terminal.expect("Rewound: 1 restored, 0 recreated, 2 deleted");
    terminal.send("/redo\r");
    terminal.expect("Rewind? [y/N]");
    terminal.send("y\r");
    terminal.expect("Rewound: 1 restored, 2 recreated, 0 deleted");
    terminal.finish();
    assert_eq!(read(ws.path(), "notes.txt").as_deref(), Some("from the tool\n"));
    assert_eq!(read(ws.path(), "shell.txt").as_deref(), Some("from bash\n"));
    assert_eq!(read(ws.path(), "kept.txt").as_deref(), Some("new\n"));

    // Outside the REPL: the list, then the rewind. The person edits a file first; `--yes`
    // keeps it as a conflict and restores the rest.
    std::fs::write(ws.path().join("shell.txt"), "edited by me\n").unwrap();
    let listed = run(&["sessions", "rewind", "rew"], home.path(), ws.path());
    assert!(text(&listed).contains("write the notes and run the script"), "{}", text(&listed));
    let rewound = run(&["sessions", "rewind", "rew", "1", "--yes"], home.path(), ws.path());
    let shown = text(&rewound);
    assert_eq!(rewound.status.code(), Some(0), "{shown}");
    assert!(shown.contains("not undone"), "{shown}");
    assert_eq!(read(ws.path(), "notes.txt"), None, "the tool's file is removed");
    assert_eq!(read(ws.path(), "shell.txt").as_deref(), Some("edited by me\n"), "a conflict kept");
    assert_eq!(
        read(ws.path(), "kept.txt").as_deref(),
        Some("old\n"),
        "the redo's own write is undone"
    );
}

#[test]
fn no_snapshots_turns_rewind_off() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().display().to_string();
    let script = r#"[{"text": "Hello."}]"#;
    let mut terminal = Terminal::spawn_in(
        &repl(&["--dir", &dir, "--ephemeral", "--no-snapshots", "--model-script", script]),
        home.path(),
        ws.path(),
    );
    terminal.expect("Snapshots : off");
    terminal.expect("You >");
    terminal.send("/rewind\r");
    terminal.expect("Snapshots are off");
    terminal.finish();
}
