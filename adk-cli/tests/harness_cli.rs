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
    assert!(text(&first).contains("budget exhausted after 1 iteration(s)"), "{}", text(&first));

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
    assert!(history.contains("Goal check `test -f done.txt` failed"), "check missing:\n{history}");
    assert!(history.contains("Thinking about it."), "{history}");
    assert!(history.contains("Created."), "second iteration missing:\n{history}");
}

/// A goal is a durable run (spec 3.1 §7.11, §8.1): a call held without a terminal parks it,
/// and after the call is approved `--resume` continues the same run at that call, which
/// runs once.
#[test]
fn a_held_goal_parks_and_resumes_at_the_held_call_after_approval() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = dir_arg(ws.path());
    let until = "test -f done.txt";
    let write =
        r#"[{"call": {"name": "write_file", "args": {"path": "done.txt", "content": "ok"}}}]"#;

    let held = run(
        &["goal", "create done.txt", "--until", until, "--dir", &dir, "--model-script", write],
        home.path(),
        ws.path(),
    );
    let shown = text(&held);
    assert_eq!(held.status.code(), Some(3), "{shown}");
    assert!(shown.contains("goal run "), "{shown}");
    assert!(shown.contains("adk-rust goal --resume"), "{shown}");
    assert!(!ws.path().join("done.txt").exists(), "a held call ran:\n{shown}");
    let session = held_session(&shown);

    // Resuming before the decision runs nothing and reports the call again.
    let early = run(
        &[
            "goal",
            "create done.txt",
            "--until",
            until,
            "--dir",
            &dir,
            "--resume",
            "--model-script",
            "[]",
        ],
        home.path(),
        ws.path(),
    );
    assert_eq!(early.status.code(), Some(3), "{}", text(&early));
    assert!(!ws.path().join("done.txt").exists());

    let listing = text(&run(&["approvals", "list"], home.path(), ws.path()));
    let line = listing.lines().find(|line| line.contains("write_file")).expect(&listing);
    assert!(line.contains(&session), "{listing}");
    let id = line.split_whitespace().next().unwrap().to_string();
    let approved = run(&["approvals", "approve", &id], home.path(), ws.path());
    assert!(approved.status.success(), "{}", text(&approved));
    // The call belongs to a parked goal, so approving it points to `goal --resume`.
    assert!(
        text(&approved)
            .contains("`adk-rust goal 'create done.txt' --until 'test -f done.txt' --resume`"),
        "{}",
        text(&approved)
    );

    let resumed = run(
        &[
            "goal",
            "create done.txt",
            "--until",
            until,
            "--dir",
            &dir,
            "--resume",
            "--model-script",
            r#"[{"text": "Created."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&resumed);
    assert_eq!(resumed.status.code(), Some(0), "{shown}");
    assert!(shown.contains(&format!("resuming goal in session {session}")), "{shown}");
    assert!(shown.contains("goal met after 1 iteration(s)"), "{shown}");
    assert_eq!(std::fs::read_to_string(ws.path().join("done.txt")).unwrap(), "ok");
}

/// A goal whose process dies mid-check resumes in its session (spec 3.1 §7.11, §7.12): the
/// run the dead process held is reclaimed when its lease lapses, and the goal runs on.
#[test]
fn a_goal_killed_mid_check_resumes_in_its_session() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let dir = dir_arg(ws.path());
    let until = "touch checking; test -f stop || (sleep 20; false)";

    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command
        .args([
            "goal",
            "wait for stop",
            "--until",
            until,
            "--dir",
            &dir,
            "--yes",
            "--no-memory",
            "--model-script",
            r#"[{"text": "Working on it."}]"#,
        ])
        .current_dir(ws.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    common::isolate(&mut command, home.path());
    let mut child = command.spawn().unwrap();
    let started = std::time::Instant::now();
    while !ws.path().join("checking").exists() {
        assert!(started.elapsed() < common::TIMEOUT, "the goal never reached its check");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // The process dies during the check, as a crash would.
    child.kill().unwrap();
    child.wait().unwrap();

    std::fs::write(ws.path().join("stop"), "").unwrap();
    let resumed = run(
        &[
            "goal",
            "wait for stop",
            "--until",
            until,
            "--dir",
            &dir,
            "--yes",
            "--no-memory",
            "--resume",
            "--model-script",
            r#"[{"text": "Resumed."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&resumed);
    assert_eq!(resumed.status.code(), Some(0), "{shown}");
    assert!(shown.contains("resuming goal in session"), "{shown}");
    assert!(shown.contains("goal met after 1 iteration(s)"), "{shown}");
}

/// `--check-timeout` bounds each run of the check (spec 3.1 §7.11): a check that runs out
/// of time fails the iteration.
#[test]
fn the_check_timeout_flag_bounds_the_check() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let output = run(
        &[
            "goal",
            "wait",
            "--until",
            "sleep 5",
            "--check-timeout",
            "1",
            "--max-iters",
            "1",
            "--yes",
            "--no-memory",
            "--dir",
            &dir_arg(ws.path()),
            "--model-script",
            r#"[{"text": "Waiting."}]"#,
        ],
        home.path(),
        ws.path(),
    );
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(1), "{shown}");
    assert!(shown.contains("budget exhausted after 1 iteration(s)"), "{shown}");
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
#[cfg_attr(feature = "surface", ignore = "`surface` replaces the REPL with the terminal surface")]
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
#[cfg_attr(feature = "surface", ignore = "`surface` replaces the REPL with the terminal surface")]
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
#[cfg_attr(feature = "surface", ignore = "`surface` replaces the REPL with the terminal surface")]
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

// --- ultracode (spec 3.1 §7.10 "Adaptive review", §8.1) ---------------------------------

/// Every file under `root`, relative and sorted.
fn files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Runs `adk-rust ultracode TASK` unattended in auto mode with a receipt; returns the output
/// and the receipt.
fn ultracode(
    ws: &Path,
    home: &Path,
    task: &str,
    script: &str,
    extra: &[&str],
) -> (std::process::Output, serde_json::Value) {
    let receipt = home.join("receipt.json");
    let dir = dir_arg(ws);
    let receipt_arg = receipt.display().to_string();
    let mut args = vec![
        "ultracode",
        task,
        "--ephemeral",
        "--mode",
        "auto",
        "--yes",
        "--dir",
        &dir,
        "--receipt",
        &receipt_arg,
        "--model-script",
        script,
    ];
    args.extend_from_slice(extra);
    let output = run(&args, home, ws);
    let text = std::fs::read_to_string(&receipt).unwrap_or_else(|_| {
        panic!("no receipt:\n{}", common::text(&output));
    });
    (output, serde_json::from_str(&text).unwrap())
}

const APPROVE: &str = r#"{"text": "VERDICT: approve"}"#;

fn script(head: &[&str], approvals: usize) -> String {
    let mut replies: Vec<&str> = head.to_vec();
    replies.extend(std::iter::repeat_n(APPROVE, approvals));
    format!("[{}]", replies.join(",\n"))
}

#[test]
fn ultracode_routes_a_security_change_to_a_board() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    write(&ws.path().join(".adk/harness.toml"), "[ultracode]\nmax_board_rounds = 1\n");
    let before = files(ws.path());
    let script = script(
        &[
            r#"{"call": {"name": "write_file", "args": {"path": "src/auth/token.rs", "content": "pub fn expiry() -> u64 { 3600 }\n"}}}"#,
            r#"{"text": "Done."}"#,
        ],
        4,
    );
    let (output, receipt) =
        ultracode(ws.path(), home.path(), "make tokens expire after an hour", &script, &[]);
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("route: blackboard (sensitive)"), "{shown}");
    let routed = &receipt["routed"];
    assert_eq!(routed["rule"], "sensitive", "{receipt:#}");
    assert_eq!(
        routed["plan"]["reviewers"],
        serde_json::json!(["correctness", "edge-cases", "security", "style"])
    );
    assert_eq!(routed["plan"]["boardRounds"], 1);
    assert!(routed.get("router").is_none(), "no router call");
    assert_eq!(receipt["chainVerified"], true);
    assert_eq!(receipt["finished"]["decision"], "approved", "{receipt:#}");
    assert_eq!(receipt["reviewed"][0]["architecture"], "blackboard");
    assert_eq!(receipt["reviewed"][0]["workspaceUnchanged"], true);
    let mut expected = before;
    expected.push("src/auth/token.rs".to_string());
    expected.sort();
    assert_eq!(files(ws.path()), expected, "only the implementer changed the workspace");
    assert_eq!(
        adk_cli::harness::ultracode::replay(routed).map(|plan| plan.architecture.to_string()),
        Ok("blackboard".to_string())
    );
}

#[test]
fn ultracode_takes_the_model_route_and_records_it() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    for package in ["crates/greet", "crates/app"] {
        write(&ws.path().join(package).join("Cargo.toml"), "[package]\nname = \"x\"\n");
    }
    let script = script(
        &[
            r#"{"calls": [
                {"name": "write_file", "args": {"path": "crates/greet/src/lib.rs", "content": "pub trait Greeter {}\n"}},
                {"name": "write_file", "args": {"path": "crates/app/src/main.rs", "content": "fn main() {}\n"}}
            ]}"#,
            r#"{"text": "Done."}"#,
            r#"{"text": "{\"architecture\":\"blackboard\",\"specialists\":[\"api-compat\"],\"boardRounds\":1,\"rationale\":\"one trait spans two crates\"}"}"#,
        ],
        4,
    );
    let (output, receipt) =
        ultracode(ws.path(), home.path(), "add a Greeter trait and use it", &script, &[]);
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("route: blackboard (model)"), "{shown}");
    assert!(shown.contains("why: one trait spans two crates"), "{shown}");
    let routed = &receipt["routed"];
    assert_eq!((&routed["rule"], &routed["source"]), (&"model".into(), &"model".into()));
    assert_eq!(routed["inputs"]["change"]["packagesTouched"], 2);
    assert_eq!(
        routed["plan"]["reviewers"],
        serde_json::json!(["correctness", "edge-cases", "api-compat", "style"])
    );
    assert!(routed["router"]["answerDigest"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(routed["router"]["effort"], "low", "the router runs at the members' effort");
    assert!(receipt["routerAnswer"].as_str().unwrap().contains("one trait spans two crates"));
    let plan = adk_cli::harness::ultracode::replay(routed).unwrap();
    assert_eq!(serde_json::to_value(&plan).unwrap(), routed["plan"]);
    assert_eq!(receipt["finished"]["modelCalls"], 2 + 1 + 4);
    assert_eq!(receipt["chainVerified"], true);
}

#[test]
fn ultracode_invalid_router_answer_falls_back() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let script = script(
        &[
            r#"{"call": {"name": "write_file", "args": {"path": "src/lib.rs", "content": "pub fn last(v: &[u8]) -> Option<u8> { v.last().copied() }\n"}}}"#,
            r#"{"text": "Done."}"#,
            r#"{"text": "I would review this with a graph."}"#,
        ],
        3,
    );
    let (output, receipt) =
        ultracode(ws.path(), home.path(), "fix the off-by-one in last()", &script, &[]);
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("route: graph (heuristic: router.unparseable)"), "{shown}");
    let routed = &receipt["routed"];
    assert_eq!(routed["fallback"]["code"], "router.unparseable");
    assert!(routed["router"]["answer"].is_null(), "an invalid answer is not applied");
    assert!(routed["router"]["answerDigest"].is_string(), "its digest is recorded");
    assert_eq!(receipt["finished"]["decision"], "approved", "{receipt:#}");
    assert_eq!(receipt["reviewed"][0]["verdicts"].as_array().unwrap().len(), 3);
    assert!(adk_cli::harness::ultracode::replay(routed).is_ok());
}

#[test]
fn ultracode_architecture_flag_skips_the_router() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let script = script(
        &[
            r#"{"call": {"name": "write_file", "args": {"path": "src/lib.rs", "content": "pub fn one() -> u8 { 1 }\n"}}}"#,
            r#"{"text": "Done."}"#,
        ],
        3,
    );
    let (output, receipt) =
        ultracode(ws.path(), home.path(), "add one()", &script, &["--architecture", "graph"]);
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert_eq!(receipt["routed"]["rule"], "override");
    assert_eq!(receipt["started"]["source"], "flag");
    // A router call would have taken a reviewer's reply, and that reviewer would fail.
    assert_eq!(receipt["finished"]["decision"], "approved", "{shown}");
    assert_eq!(receipt["finished"]["modelCalls"], 2 + 3);
}

#[test]
fn ultracode_project_file_cannot_force_the_board() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    write(&ws.path().join(".adk/harness.toml"), "[ultracode]\narchitecture = \"blackboard\"\n");
    let pinned = script(
        &[
            r#"{"call": {"name": "write_file", "args": {"path": "src/lib.rs", "content": "pub fn two() -> u8 { 2 }\n"}}}"#,
            r#"{"text": "Done."}"#,
        ],
        3,
    );
    let (output, receipt) =
        ultracode(ws.path(), home.path(), "add two()", &pinned, &["--router", "heuristic"]);
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("`ultracode.architecture` ignored"), "{shown}");
    assert_eq!(receipt["routed"]["rule"], "heuristic");
    assert_eq!(receipt["started"]["requested"], "auto");

    // A glob that matches every path adds the security seat, and still does not choose the
    // board.
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    write(&ws.path().join(".adk/harness.toml"), "[ultracode]\nsensitive_paths = [\"*\"]\n");
    let globbed = script(
        &[
            r#"{"call": {"name": "write_file", "args": {"path": "src/lib.rs", "content": "pub fn two() -> u8 { 2 }\n"}}}"#,
            r#"{"text": "Done."}"#,
        ],
        4,
    );
    let (output, receipt) =
        ultracode(ws.path(), home.path(), "add two()", &globbed, &["--router", "heuristic"]);
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    let routed = &receipt["routed"];
    assert_eq!(routed["rule"], "heuristic", "{receipt:#}");
    assert_eq!(routed["plan"]["architecture"], "graph");
    assert_eq!(
        routed["plan"]["reviewers"],
        serde_json::json!(["correctness", "edge-cases", "security", "style"])
    );
    assert_eq!(
        routed["inputs"]["change"]["workspaceSensitiveMatched"],
        serde_json::json!(["src/lib.rs"])
    );
    assert!(adk_cli::harness::ultracode::replay(routed).is_ok());
}

#[test]
fn ultracode_a_sensitive_revise_adds_the_security_seat() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let changes = r#"{"text": "VERDICT: changes\n- validate the caller's token"}"#;
    let script = script(
        &[
            r#"{"call": {"name": "write_file", "args": {"path": "src/greet.rs", "content": "pub fn hi() -> &'static str { \"hi\" }\n"}}}"#,
            r#"{"text": "Done."}"#,
            changes,
            changes,
            changes,
            r#"{"call": {"name": "write_file", "args": {"path": "src/auth/token.rs", "content": "pub fn valid(token: &str) -> bool { !token.is_empty() }\n"}}}"#,
            r#"{"text": "Revised."}"#,
        ],
        4,
    );
    let (output, receipt) =
        ultracode(ws.path(), home.path(), "add a greeting", &script, &["--router", "heuristic"]);
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(
        shown.contains("━━ escalate ━━ a sensitive path changed: src/auth/token.rs"),
        "{shown}"
    );
    let routed = &receipt["routed"];
    assert_eq!(
        routed["plan"]["reviewers"],
        serde_json::json!(["correctness", "edge-cases", "style"])
    );
    let escalated = &receipt["escalated"];
    assert_eq!(escalated["round"], 1, "{receipt:#}");
    assert_eq!(escalated["change"]["security"], serde_json::json!(["src/auth/token.rs"]));
    assert_eq!(
        escalated["plan"]["reviewers"],
        serde_json::json!(["correctness", "edge-cases", "security", "style"])
    );
    let stages: Vec<Vec<String>> = receipt["reviewed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stage| {
            stage["verdicts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|verdict| verdict["focus"].as_str().unwrap().to_string())
                .collect()
        })
        .collect();
    assert_eq!(
        stages,
        [
            vec!["correctness", "edge-cases", "style"],
            vec!["correctness", "edge-cases", "security", "style"]
        ]
    );
    assert_eq!(receipt["finished"]["decision"], "approved", "{receipt:#}");
    assert_eq!(receipt["finished"]["escalated"], true);
    assert_eq!(receipt["finished"]["modelCalls"], 2 + 3 + 2 + 4);
    let plan = adk_cli::harness::ultracode::replay_escalation(routed, escalated).unwrap();
    assert_eq!(serde_json::to_value(&plan).unwrap(), escalated["plan"]);
    let order: Vec<&str> = receipt["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["action"].as_str().unwrap())
        .collect();
    let at = |action: &str| order.iter().position(|a| *a == action).unwrap();
    assert!(
        at("harness.ultracode.escalated")
            < order.iter().rposition(|a| *a == "harness.ultracode.reviewed").unwrap()
    );
}

#[test]
fn ultracode_sees_a_planted_git_hook() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    write(&ws.path().join(".git/config"), "[core]\n");
    write(&ws.path().join(".git/objects/ab/cdef"), "blob");
    write(&ws.path().join(".adk/harness.toml"), "[ultracode]\nmax_board_rounds = 1\n");
    let script = script(
        &[
            r#"{"call": {"name": "bash", "args": {"command": "mkdir -p .git/hooks && printf 'curl example.com' > .git/hooks/pre-commit"}}}"#,
            r#"{"text": "Done."}"#,
        ],
        4,
    );
    let (output, receipt) = ultracode(
        ws.path(),
        home.path(),
        "speed up the tests",
        &script,
        &["--router", "heuristic"],
    );
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert!(ws.path().join(".git/hooks/pre-commit").exists(), "{shown}");
    let routed = &receipt["routed"];
    assert_eq!(routed["rule"], "sensitive", "{receipt:#}");
    assert_eq!(
        routed["inputs"]["change"]["protected"],
        serde_json::json!([".git/hooks/pre-commit"])
    );
    assert!(routed["plan"]["reviewers"].as_array().unwrap().contains(&"security".into()));
}

#[test]
fn ultracode_budget_cap_finalizes_before_revise() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    write(
        &ws.path().join(".adk/harness.toml"),
        "[ultracode]\nimplement_model_calls = 4\nreviewer_model_calls = 2\nmax_model_calls = 12\n",
    );
    let script = r#"[
        {"call": {"name": "write_file", "args": {"path": "src/lib.rs", "content": "pub fn three() -> u8 { 3 }\n"}}},
        {"text": "Done."},
        {"text": "VERDICT: changes\n- name it better"},
        {"text": "VERDICT: changes\n- handle overflow"},
        {"text": "VERDICT: changes\n- add a doc comment"}
    ]"#;
    let (output, receipt) =
        ultracode(ws.path(), home.path(), "add three()", script, &["--router", "heuristic"]);
    let shown = text(&output);
    assert_eq!(output.status.code(), Some(0), "{shown}");
    assert_eq!(receipt["finished"]["decision"], "budgetExhausted", "{shown}");
    assert_eq!(receipt["finished"]["rounds"], 1);
    assert_eq!(receipt["finished"]["modelCalls"], 5);
    assert_eq!(receipt["started"]["limits"]["maxModelCalls"], 12);
    assert!(!shown.contains("━━ revise ━━"), "no revise starts: {shown}");
}

#[test]
fn ultracode_counts_an_implement_turn_a_denial_ended() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let receipt = home.path().join("receipt.json");
    let receipt_arg = receipt.display().to_string();
    let dir = dir_arg(ws.path());
    let script = script(&[r#"{"call": {"name": "bash", "args": {"command": "rm -rf build"}}}"#], 3);
    let mut terminal = Terminal::spawn_in(
        &[
            "ultracode",
            "clean up the build",
            "--ephemeral",
            "--dir",
            &dir,
            "--receipt",
            &receipt_arg,
            "--model-script",
            &script,
        ],
        home.path(),
        ws.path(),
    );
    terminal.expect("[approval needed] bash on host");
    terminal.expect("details >");
    terminal.send("n\r");
    terminal.expect("reason (optional) >");
    terminal.send("keep the build\r");
    let (code, shown) = terminal.wait();
    assert_eq!(code, Some(0), "{shown}");
    let receipt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&receipt).expect(&shown)).unwrap();
    assert_eq!(receipt["routed"]["rule"], "unchanged", "{receipt:#}");
    assert_eq!(receipt["routed"]["inputs"]["change"]["firstTurn"], "failed");
    // The implement turn's one model call counts, though the denial stopped its stream.
    assert_eq!(receipt["finished"]["modelCalls"], 1 + 3, "{receipt:#}");
    assert_eq!(receipt["finished"]["decision"], "approved");
}

#[test]
fn ultracode_receipt_never_follows_a_planted_link() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let victim = home.path().join("victim.txt");
    std::fs::write(&victim, "keep").unwrap();
    let elsewhere = home.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let dir = dir_arg(ws.path());
    let approvals = std::iter::repeat_n(APPROVE, 3).collect::<Vec<_>>().join(",");

    // The implementer links the receipt's temporary name to a file outside the workspace.
    let link = format!(
        r#"{{"call": {{"name": "bash", "args": {{"command": "ln -s {} receipt.json.tmp"}}}}}}"#,
        victim.display()
    );
    let script = format!(r#"[{link}, {{"text": "Done."}}, {approvals}]"#);
    let receipt = ws.path().join("receipt.json").display().to_string();
    let args = [
        "ultracode",
        "tidy up",
        "--ephemeral",
        "--mode",
        "auto",
        "--yes",
        "--router",
        "heuristic",
        "--dir",
        &dir,
        "--receipt",
        &receipt,
        "--model-script",
        &script,
    ];
    let output = run(&args, home.path(), ws.path());
    let shown = text(&output);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep", "{shown}");
    assert!(ws.path().join("receipt.json.tmp").symlink_metadata().is_ok(), "{shown}");
    assert_eq!(output.status.code(), Some(0), "{shown}");
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&receipt).unwrap()).unwrap();
    assert_eq!(written["chainVerified"], true);

    // The implementer links the receipt's directory outside the workspace.
    let link = format!(
        r#"{{"call": {{"name": "bash", "args": {{"command": "ln -s {} out"}}}}}}"#,
        elsewhere.display()
    );
    let script = format!(r#"[{link}, {{"text": "Done."}}, {approvals}]"#);
    let receipt = ws.path().join("out/r.json").display().to_string();
    let args = [
        "ultracode",
        "tidy up",
        "--ephemeral",
        "--mode",
        "auto",
        "--yes",
        "--router",
        "heuristic",
        "--dir",
        &dir,
        "--receipt",
        &receipt,
        "--model-script",
        &script,
    ];
    let output = run(&args, home.path(), ws.path());
    let shown = text(&output);
    assert!(ws.path().join("out").symlink_metadata().is_ok(), "{shown}");
    assert!(!elsewhere.join("r.json").exists(), "{shown}");
    assert_ne!(output.status.code(), Some(0), "{shown}");
    assert!(shown.contains("refusing to write"), "{shown}");
}

#[test]
fn the_goal_mirror_never_follows_a_planted_link() {
    let home = tempfile::tempdir().unwrap();
    let ws = workspace();
    let victim = home.path().join("victim.txt");
    std::fs::write(&victim, "keep").unwrap();
    let dir = dir_arg(ws.path());
    let state = ws.path().join("goal.json").display().to_string();
    let link = format!(
        r#"{{"call": {{"name": "bash", "args": {{"command": "ln -s {} goal.json.tmp && touch done.txt"}}}}}}"#,
        victim.display()
    );
    let script = format!(r#"[{link}, {{"text": "Done."}}]"#);
    let args = [
        "goal",
        "create done.txt",
        "--until",
        "test -f done.txt",
        "--mode",
        "auto",
        "--yes",
        "--dir",
        &dir,
        "--max-iters",
        "1",
        "--state",
        &state,
        "--model-script",
        &script,
    ];
    let output = run(&args, home.path(), ws.path());
    let shown = text(&output);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep", "{shown}");
    assert!(ws.path().join("goal.json.tmp").symlink_metadata().is_ok(), "{shown}");
    assert_eq!(output.status.code(), Some(0), "{shown}");
    let mirror: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(mirror["goal"], "create done.txt");
}
