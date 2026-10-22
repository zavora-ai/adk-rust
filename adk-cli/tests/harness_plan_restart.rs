//! A plan approved through `adk-rust` runs in the next process (spec 3.1 §7.11, §13 Q5, H4):
//! plans are checkpointed in `harness.db`, beside the sessions, not in memory.

#![cfg(all(unix, feature = "scripted-model", feature = "harness", feature = "persist"))]

mod common;

use common::{run, text};

#[test]
fn an_approved_plan_runs_in_the_next_process() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("README.md"), "a project\n").unwrap();
    let dir = ws.path().display().to_string();
    let plan = r#"[
        {"call": {"name": "exit_plan_mode", "args": {
            "plan": "Write two files.",
            "next_mode": "ask",
            "steps": [
                {"id": "1", "title": "One", "instruction": "Write 1.txt"},
                {"id": "2", "title": "Two", "instruction": "Write 2.txt", "depends_on": ["1"]}
            ]
        }}},
        {"text": "Planned."}
    ]"#;
    let planned = run(
        &[
            "code",
            "plan two files",
            "--mode",
            "plan",
            "--yes",
            "--session",
            "plan-1",
            "--no-memory",
            "--dir",
            &dir,
            "--model-script",
            plan,
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&planned);
    assert_eq!(planned.status.code(), Some(0), "{shown}");
    assert!(shown.contains("checkpointed in harness.db"), "{shown}");

    // A new process: the plan is found in harness.db and both steps run in this turn. With
    // plans in memory this turn is free-form and writes only 1.txt.
    let steps = r#"[
        {"call": {"name": "write_file", "args": {"path": "1.txt", "content": "one"}}},
        {"text": "Step one done."},
        {"call": {"name": "write_file", "args": {"path": "2.txt", "content": "two"}}},
        {"text": "Step two done."}
    ]"#;
    let ran = run(
        &[
            "code",
            "go",
            "--yes",
            "--session",
            "plan-1",
            "--no-memory",
            "--dir",
            &dir,
            "--model-script",
            steps,
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&ran);
    assert_eq!(ran.status.code(), Some(0), "{shown}");
    assert_eq!(std::fs::read_to_string(ws.path().join("1.txt")).unwrap(), "one", "{shown}");
    assert_eq!(std::fs::read_to_string(ws.path().join("2.txt")).unwrap(), "two", "{shown}");
}

#[test]
fn ephemeral_plans_stay_in_memory_and_say_so() {
    let home = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("README.md"), "a project\n").unwrap();
    let output = run(
        &[
            "code",
            "hello",
            "--ephemeral",
            "--dir",
            &ws.path().display().to_string(),
            "--model-script",
            r#"[{"text": "Hi."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("an approved plan does not survive a restart"), "{shown}");
}
