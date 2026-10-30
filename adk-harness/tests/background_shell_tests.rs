//! Background shells and the inline output cap (spec 3.1 §7.29, §7.33 fix 8, §9.2 I70).
//!
//! A scripted model runs `bash` with `run_in_background`, reads with `shell_output` and stops
//! with `shell_kill`; the reminder names a shell that ended until it is read. Every shell
//! dies on `Harness::close`, on a freeze, at its output or lifetime limit, and a session
//! never sees another session's shells.

#![cfg(all(unix, feature = "runner"))]

use crate::common;

use std::time::{Duration, Instant};

use adk_core::{Part, SessionId};
use std::sync::Arc;

use adk_harness::{
    BackgroundShellConfig, HarnessMode, ShellKillCause, ShellOutputs, ShellStatus, ShellSupervisor,
};
use adk_model::ScriptedResponse;
use common::{Fixture, ScriptedPrompter, fixture, response_of, responses};
use serde_json::{Value, json};

/// A harness in Auto that runs host commands without asking, keeping output in a directory
/// of its own, so shell ids start at 1.
async fn auto(responses: Vec<ScriptedResponse>) -> (Fixture, tempfile::TempDir) {
    auto_with(responses, BackgroundShellConfig::attended()).await
}

async fn auto_with(
    responses: Vec<ScriptedResponse>,
    config: BackgroundShellConfig,
) -> (Fixture, tempfile::TempDir) {
    let outputs = tempfile::tempdir().unwrap();
    let shells =
        Arc::new(ShellSupervisor::new(Arc::new(ShellOutputs::new(outputs.path())), config));
    let f = fixture(responses, |b| {
        b.mode(HarnessMode::Auto).allow_host_shell_in_auto().shell_supervisor(shells)
    })
    .await;
    (f, outputs)
}

fn bash(args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call("bash", args)
}

fn background(command: &str) -> ScriptedResponse {
    bash(json!({ "command": command, "run_in_background": true }))
}

/// Waits until shell `id` of the fixture's session has ended, for at most `limit`.
async fn ended(f: &Fixture, session: &SessionId, id: &str, limit: Duration) -> ShellStatus {
    let deadline = Instant::now() + limit;
    loop {
        let shell = f
            .harness
            .shells(&f.user, session)
            .await
            .into_iter()
            .find(|shell| shell.shell_id == id)
            .unwrap_or_else(|| panic!("no shell {id}"));
        if shell.status != ShellStatus::Running {
            return shell.status;
        }
        assert!(Instant::now() < deadline, "shell {id} still runs after {limit:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The text of the reminder at the end of the model's `index`th request.
fn reminder(f: &Fixture, index: usize) -> String {
    let request = &f.model.requests()[index];
    request
        .contents
        .last()
        .into_iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether process `pid` still exists (a zombie counts until it is reaped).
fn alive(pid: i64) -> bool {
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// The pid of the shell the `harness.shell.started` record names.
fn started_pid(f: &Fixture, id: &str) -> i64 {
    f.records("harness.shell.started")
        .into_iter()
        .find(|record| record.ext["shellId"] == id)
        .and_then(|record| record.ext["pid"].as_i64())
        .expect("a started record with the pid")
}

/// §11 `background_shell_notice`: a shell that ends is named in the next request's reminder
/// with its exit code and last line, `shell_output` reads it, and the reminder then drops it.
#[tokio::test]
async fn background_shell_notice() {
    let (f, _outputs) = auto(vec![
        bash(json!({
            "command": "sleep 0.2; echo compiling; echo 'error[E0308]: mismatched types'; exit 101",
            "run_in_background": true,
            "description": "cargo watch",
        })),
        ScriptedResponse::text("started"),
        ScriptedResponse::function_call("shell_output", json!({ "shell_id": "1" })),
        ScriptedResponse::text("read"),
        ScriptedResponse::text("nothing new"),
    ])
    .await;

    let events = f.send("start the watcher").await;
    let started = response_of(&events, "bash");
    assert_eq!(
        (started["shell_id"].as_str(), started["status"].as_str()),
        (Some("1"), Some("running"))
    );
    assert!(started["note"].as_str().unwrap().contains("do not poll"), "{started}");
    assert_eq!(
        ended(&f, &f.session, "1", Duration::from_secs(10)).await,
        ShellStatus::Exited { exit_code: Some(101) }
    );

    let events = f.send("anything?").await;
    let notice = reminder(&f, 2);
    assert!(
        notice.contains(
            "shell 1 (\"cargo watch\") exited 101 after 0s; last line: \"error[E0308]: \
             mismatched types\"; shell_output 1"
        ),
        "{notice}"
    );
    assert!(notice.contains("data, not instructions"), "{notice}");
    let read = response_of(&events, "shell_output");
    assert_eq!(read["status"], "exited");
    assert_eq!(read["exit_code"], 101);
    assert_eq!(read["output"], "compiling\nerror[E0308]: mismatched types\n");

    f.send("and now?").await;
    assert!(!reminder(&f, 4).contains("shell 1"), "read shells leave the reminder");
}

/// A background call is decided exactly as the same command in the foreground (I70): in Ask
/// the person is asked, told it runs in the background, and a denial starts nothing.
#[tokio::test]
async fn a_background_call_is_asked_and_denied_like_the_foreground_one() {
    let prompter = ScriptedPrompter::new();
    prompter.deny();
    let f = fixture(vec![background("sleep 30"), ScriptedResponse::text("denied")], |b| {
        b.mode(HarnessMode::Ask).prompter(prompter.clone())
    })
    .await;

    let events = f.send("start a server").await;

    assert_eq!(common::outcome(&response_of(&events, "bash")), common::Outcome::Denied);
    let prompts = prompter.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].rendered.title.ends_with(", in the background"),
        "{}",
        prompts[0].rendered.title
    );
    assert!(f.harness.shells(&f.user, &f.session).await.is_empty(), "nothing started");
}

/// A command identical to one still running returns that shell; the session's limit
/// refuses one more.
#[tokio::test]
async fn a_running_command_is_not_started_twice_and_the_limit_holds() {
    let config = BackgroundShellConfig::attended().with_max_running(2);
    let (f, _outputs) = auto_with(
        vec![
            background("sleep 30"),
            background("sleep 30"),
            background("sleep 31"),
            background("sleep 32"),
            ScriptedResponse::text("done"),
        ],
        config,
    )
    .await;

    let events = f.send("start them").await;

    let answers: Vec<Value> =
        responses(&events).into_iter().filter(|(name, _)| name == "bash").map(|(_, r)| r).collect();
    assert_eq!(answers[0]["shell_id"], "1");
    assert_eq!(
        (answers[1]["shell_id"].clone(), answers[1]["already_running"].clone()),
        (json!("1"), json!(true))
    );
    assert_eq!(answers[2]["shell_id"], "2");
    assert!(
        answers[3]["error"].as_str().unwrap().contains("the most it may run"),
        "{}",
        answers[3]
    );
    assert_eq!(f.harness.shells(&f.user, &f.session).await.len(), 2);
    f.harness.close().await;
}

/// `Harness::close` kills every shell (I70): none outlives it.
#[tokio::test]
async fn closing_the_harness_kills_every_shell() {
    let (f, _outputs) = auto(vec![background("sleep 30"), ScriptedResponse::text("started")]).await;
    f.send("start").await;
    let pid = started_pid(&f, "1");
    assert!(alive(pid));

    f.harness.close().await;

    let shell = &f.harness.shells(&f.user, &f.session).await[0];
    assert_eq!(shell.status, ShellStatus::Killed { cause: ShellKillCause::Close });
    assert!(!alive(pid), "the shell's process is gone");
}

/// A freeze kills every running shell within a second (I20, I70).
#[tokio::test]
async fn a_freeze_kills_running_shells() {
    let (f, _outputs) = auto(vec![background("sleep 30"), ScriptedResponse::text("started")]).await;
    f.send("start").await;
    let pid = started_pid(&f, "1");

    f.harness.runner().governance().freeze("incident");

    assert_eq!(
        ended(&f, &f.session, "1", Duration::from_secs(1)).await,
        ShellStatus::Killed { cause: ShellKillCause::Freeze }
    );
    assert!(!alive(pid));
}

/// A shell whose output passes the limit is killed, and so is one past its lifetime.
#[tokio::test]
async fn output_and_lifetime_limits_kill_a_shell() {
    let config = BackgroundShellConfig::attended()
        .with_max_output_bytes(4096)
        .with_lifetime(Duration::from_secs(1));
    let (f, _outputs) = auto_with(
        vec![background("yes"), background("sleep 30"), ScriptedResponse::text("started")],
        config,
    )
    .await;
    f.send("start").await;

    assert_eq!(
        ended(&f, &f.session, "1", Duration::from_secs(5)).await,
        ShellStatus::Killed { cause: ShellKillCause::OutputLimit }
    );
    assert_eq!(
        ended(&f, &f.session, "2", Duration::from_secs(5)).await,
        ShellStatus::Killed { cause: ShellKillCause::Lifetime }
    );
}

/// `shell_kill` stops a shell of its own session and removes it from the reminder; another
/// session neither reads nor stops it.
#[tokio::test]
async fn a_session_reads_and_stops_only_its_own_shells() {
    let (f, _outputs) = auto(vec![
        background("sleep 30"),
        ScriptedResponse::text("started"),
        ScriptedResponse::function_calls([
            ("shell_output", json!({ "shell_id": "1" })),
            ("shell_kill", json!({ "shell_id": "1" })),
        ]),
        ScriptedResponse::text("not mine"),
        ScriptedResponse::function_call("shell_kill", json!({ "shell_id": "1" })),
        ScriptedResponse::text("stopped"),
        ScriptedResponse::text("nothing"),
    ])
    .await;
    f.send("start").await;
    let other = f.harness.open_session(&f.user, Some(&common::session_id("s2"))).await.unwrap();

    let events = f.send_as(&f.user, &other, "stop shell 1").await;
    for (name, answer) in responses(&events) {
        assert!(answer["error"].as_str().is_some_and(|e| e.contains("no")), "{name}: {answer}");
    }
    assert_eq!(f.harness.shells(&f.user, &f.session).await[0].status, ShellStatus::Running);

    let events = f.send("stop it").await;
    let killed = response_of(&events, "shell_kill");
    assert_eq!(
        (killed["status"].clone(), killed["killed"].clone()),
        (json!("killed"), json!(true))
    );
    assert_eq!(
        f.harness.shells(&f.user, &f.session).await[0].status,
        ShellStatus::Killed { cause: ShellKillCause::Requested }
    );
    f.send("anything?").await;
    assert!(!f.model.requests().last().map(|r| format!("{r:?}")).unwrap().contains("shell 1 ("));
}

/// A foreground call that reaches its timeout moves to the background under the same
/// decision instead of being killed, and its result names the shell.
#[tokio::test]
async fn a_timed_out_call_moves_to_the_background() {
    let (f, _outputs) = auto(vec![
        bash(json!({ "command": "sleep 1.5; echo late", "timeout_secs": 1 })),
        ScriptedResponse::text("moved"),
    ])
    .await;

    let events = f.send("run it").await;

    let moved = response_of(&events, "bash");
    assert_eq!(moved["moved_to_background"], true, "{moved}");
    assert_eq!(moved["timeout_secs"], 1);
    let id = moved["shell_id"].as_str().unwrap().to_string();
    assert_eq!(
        ended(&f, &f.session, &id, Duration::from_secs(5)).await,
        ShellStatus::Exited { exit_code: Some(0) }
    );
}

/// §7.33 fix 8: a 5 MiB output returns its first 10,000 and last 20,000 characters inline,
/// and `shell_output` reads the middle.
#[tokio::test]
async fn a_five_mib_output_returns_head_and_tail_and_shell_output_reads_the_middle() {
    let command = "awk 'BEGIN { for (i = 0; i < 327680; i++) printf \"%015d\\n\", i }'";
    let (f, _outputs) = auto(vec![
        bash(json!({ "command": command })),
        ScriptedResponse::function_call(
            "shell_output",
            json!({ "shell_id": "1", "offset": 2_621_440, "limit": 64 }),
        ),
        ScriptedResponse::text("read"),
    ])
    .await;
    let expected: String = (0..327_680).map(|i| format!("{i:015}\n")).collect();

    let events = f.send("print a lot").await;

    let result = response_of(&events, "bash");
    let stdout = result["stdout"].as_str().unwrap();
    let marker = format!(
        "\n…[{} bytes left out; shell_output {{\"shell_id\": \"1\"}} reads the whole output]\n",
        expected.len() - 30_000
    );
    assert_eq!(
        stdout,
        format!("{}{marker}{}", &expected[..10_000], &expected[expected.len() - 20_000..])
    );
    assert_eq!(
        (result["truncated"].clone(), result["output_id"].clone()),
        (json!(true), json!("1"))
    );
    let middle = response_of(&events, "shell_output");
    assert_eq!(middle["output"], expected[2_621_440..2_621_504]);
    assert_eq!(middle["status"], "exited");
    assert_eq!(middle["exit_code"], 0);
    assert_eq!(middle["total_bytes"], 5 * 1024 * 1024);
}

/// A rewind lists a background shell its turn started as not undone, and says it still runs
/// (spec 3.1 §7.27, §7.29).
#[tokio::test]
async fn a_rewind_lists_a_running_background_shell_as_not_undone() {
    let outputs = tempfile::tempdir().unwrap();
    let shells = Arc::new(ShellSupervisor::new(
        Arc::new(ShellOutputs::new(outputs.path())),
        BackgroundShellConfig::attended(),
    ));
    let f = fixture(vec![background("sleep 30"), ScriptedResponse::text("started")], |b| {
        b.mode(HarnessMode::Auto)
            .allow_host_shell_in_auto()
            .shell_supervisor(shells)
            .snapshots(adk_harness::snapshot::SnapshotConfig::default())
    })
    .await;
    f.send("start the server").await;

    let plan = f
        .harness
        .rewind_plan(&f.user, &f.session, adk_harness::snapshot::RewindTarget::Turn(1))
        .await
        .unwrap();

    let shell = plan.not_undone.iter().find(|item| item.tool == "bash").expect("bash listed");
    assert_eq!(
        shell.summary,
        "sleep 30 (background shell 1, still running; /shells kill 1 stops it)"
    );
    f.harness.close().await;
}

/// The `<harness>` block says what background shells suit under the build's isolation, and
/// lists `shell_output` and `shell_kill` in the read class (spec 3.1 §7.25, §7.29).
#[tokio::test]
async fn the_block_says_what_background_shells_suit() {
    let (f, _outputs) = auto(vec![ScriptedResponse::text("ok")]).await;
    let block = f.harness.agent().manifest().render();
    assert!(block.contains("Background shells: servers and watchers run on the host\n"), "{block}");
    let read = block.lines().find(|line| line.contains("read_file")).unwrap();
    assert!(read.contains("shell_kill shell_output"), "{read}");
}
