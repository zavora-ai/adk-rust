//! The walkthrough's scenarios, in the order a developer meets the harness (spec 3.1 §5).
//!
//! Each scenario runs the same code scripted and live: a scripted run replays what a
//! model that follows the prompt does, a live run sends the same prompts to the provider.
//! Every check is on files, stores and audit records, never on what the model says.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{RunState, SessionId, ToolContext, UsageMetadata, UserId};
use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, GoalSpec, GoalStatus, HarnessDecision, HarnessMode,
    IsolationStatus, LOOP_DETECTED_CODE, LoopGuard, ModeSource, OsSandboxConfig, PLAN_GATE_TOOL,
    PlanRunStatus, PromptAnswer, RbacMap, RulePredicate, SandboxLauncher, ShellIsolation,
    StandingRule, ToolKind,
};
use adk_model::ScriptedResponse;
use anyhow::Result;
use serde_json::{Value, json};

use super::support::{
    APP, Checks, Durable, Env, Models, Outcome, call, cargo_test_passes, clip, collect, describe,
    text, tree_digest, write_crate,
};

/// One scenario: its name, the spec row it demonstrates, and what it shows.
pub struct Scenario {
    /// The name to select it with.
    pub name: &'static str,
    /// The spec 3.1 capability or section.
    pub spec: &'static str,
    /// One line on what it shows.
    pub title: &'static str,
}

/// Every scenario, in walkthrough order.
pub const SCENARIOS: &[Scenario] = &[
    Scenario { name: "approvals", spec: "H2", title: "approvals bound to the tool" },
    Scenario { name: "plan_mode", spec: "§7.4", title: "plan mode writes nothing" },
    Scenario {
        name: "plan_graph",
        spec: "H4",
        title: "an approved plan runs as a checkpointed graph",
    },
    Scenario { name: "durable_goal", spec: "H1", title: "a goal survives the process" },
    Scenario {
        name: "team",
        spec: "H3",
        title: "members answer in isolation, under the stricter mode",
    },
    Scenario { name: "identity", spec: "H6", title: "a role map decides who may use the shell" },
    Scenario { name: "watches_itself", spec: "H7", title: "spend per subject and the loop guard" },
    Scenario { name: "sandbox", spec: "§7.9", title: "the sandbox keeps writes in the workspace" },
    Scenario { name: "long_term_memory", spec: "H11", title: "facts remembered across sessions" },
];

/// Runs scenario `name`.
pub async fn run(name: &str, models: &Models) -> Result<Checks> {
    match name {
        "approvals" => approvals(models).await,
        "plan_mode" => plan_mode(models).await,
        "plan_graph" => plan_graph(models).await,
        "durable_goal" => durable_goal(models).await,
        "team" => team(models).await,
        "identity" => identity(models).await,
        "watches_itself" => watches_itself(models).await,
        "sandbox" => sandbox(models).await,
        "long_term_memory" => long_term_memory(models).await,
        other => anyhow::bail!("unknown scenario {other}"),
    }
}

fn bash(command: &str) -> ScriptedResponse {
    call("bash", json!({ "command": command }))
}

/// A `bash` call with a fixed call id, so a scripted retry can name it.
fn bash_with_id(id: &str, command: &str) -> ScriptedResponse {
    let mut response = adk_core::LlmResponse::new(adk_core::Content {
        role: "model".to_string(),
        parts: vec![adk_core::Part::FunctionCall {
            name: "bash".into(),
            args: json!({ "command": command }),
            id: Some(id.into()),
            thought_signature: None,
        }],
    });
    response.finish_reason = Some(adk_core::FinishReason::Stop);
    response.turn_complete = true;
    ScriptedResponse::response(response)
}

fn command_of(args: &Value) -> &str {
    args.get("command").and_then(Value::as_str).unwrap_or_default()
}

fn ids(user: &str, session: &str) -> Result<(UserId, SessionId)> {
    Ok((UserId::new(user)?, SessionId::new(session)?))
}

// --- 1. H2: approvals bound to the tool -------------------------------------------------

const RUN_TESTS: &str = "Run exactly `cargo test --quiet` in the workspace with the bash tool: no other \
    flags, no pipes, no redirections, no `cd`. Then tell me in one line whether the tests pass. Do not \
    change any file.";

/// Ask mode holds `bash`; one approval runs it once; "always allow" stores a `ShellPrefix`
/// rule for the user, so the next session runs `cargo test --quiet` with no held call, while
/// `cargo publish` and another user's first call are still held (spec 3.1 §5 H2, §7.5).
async fn approvals(models: &Models) -> Result<Checks> {
    let mut checks = Checks::default();
    let model = models.model(vec![
        bash("cargo test --quiet"),
        text("The tests pass."),
        bash("cargo test --quiet"),
        text("The tests pass."),
        bash("cargo test --quiet"),
        text("The tests pass."),
        bash("cargo test --quiet"),
        bash("cargo publish --dry-run"),
    ]);
    let dir = tempfile::tempdir()?;
    write_crate(dir.path(), false)?;
    let env =
        Env::in_dir(dir, model.clone(), |b| b.mode(HarnessMode::Ask).without_default_members())
            .await?;
    let begins = || env.records("harness.bash.begin").len();

    println!("  1. Ask mode holds a host command; approve it once");
    let turn = env.send("alice", "s1", RUN_TESTS).await?;
    let pending = env.pending("alice", "s1").await?;
    checks.check(
        "Ask holds `cargo test --quiet` before anything runs",
        turn.last_outcome("bash") == Outcome::Held && pending.len() == 1 && begins() == 0,
    );
    let Some(held) = pending.first() else { return Ok(checks) };
    env.decide("alice", "s1", held, HarnessDecision::Approve).await?;
    let turn = env.resume("alice", "s1").await?;
    checks.check(
        "approved once, it runs once",
        begins() == 1 && turn.calls_of("bash").first().is_some_and(|c| c.outcome() == Outcome::Ran),
    );

    println!("  2. \"Always allow\" stores a ShellPrefix rule");
    env.send("alice", "s2", RUN_TESTS).await?;
    let pending = env.pending("alice", "s2").await?;
    let suggestion = pending.first().and_then(|entry| {
        StandingRule::suggestions(&entry.request).into_iter().find(|suggestion| {
            matches!(suggestion.predicates.as_slice(), [RulePredicate::ShellPrefix { tokens, .. }] if *tokens == ["cargo", "test"])
        })
    });
    checks.check("the held call offers the rule `bash: cargo test …`", suggestion.is_some());
    let (Some(held), Some(suggestion)) = (pending.first(), suggestion) else { return Ok(checks) };
    env.decide("alice", "s2", held, HarnessDecision::ApproveAndRemember(suggestion)).await?;
    env.resume("alice", "s2").await?;
    let rules = env.harness.standing_rules().list(APP, "alice").await?;
    checks.check(
        format!("alice has one user-scoped rule, ShellPrefix [cargo, test], pinned to bash's declaration ({} rules)", rules.len()),
        rules.len() == 1
            && rules[0].scope.session_id().is_none()
            && rules[0].tool == "bash"
            && matches!(rules[0].predicates.as_slice(), [RulePredicate::ShellPrefix { tokens, .. }] if *tokens == ["cargo", "test"]),
    );
    checks.check("harness.rule.created is audited", env.records("harness.rule.created").len() == 1);

    println!("  3. A new session runs `cargo test --quiet` with zero prompts");
    let before = begins();
    let turn = env.send("alice", "s3", RUN_TESTS).await?;
    let held = env.pending("alice", "s3").await?;
    let runs = turn.calls_of("bash");
    checks.check(
        format!(
            "no call held, `cargo test --quiet` ran ({} bash calls, {} held)",
            runs.len(),
            held.len()
        ),
        held.is_empty()
            && begins() > before
            && !runs.is_empty()
            && runs.iter().all(|c| {
                c.outcome() == Outcome::Ran && command_of(&c.args).starts_with("cargo test")
            }),
    );
    checks.check(
        "harness.rule.matched names the rule",
        !env.records("harness.rule.matched").is_empty(),
    );

    println!("  4. Rules belong to their user");
    env.send("bob", "s1", RUN_TESTS).await?;
    let bob = env.pending("bob", "s1").await?;
    checks.check("bob's first `cargo test --quiet` is held", bob.len() == 1);

    println!("  5. `cargo publish` still asks");
    let before = begins();
    env.send(
        "alice",
        "s4",
        "Run exactly `cargo publish --dry-run` in the workspace with the bash tool and tell me what it printed.",
    )
    .await?;
    let pending = env.pending("alice", "s4").await?;
    checks.check(
        format!("`cargo publish --dry-run` is held, not run (pending: {})", describe(&pending)),
        pending.iter().any(|p| command_of(&p.request.args).starts_with("cargo publish"))
            && begins() == before,
    );
    for held in &pending {
        env.decide(
            "alice",
            "s4",
            held,
            HarnessDecision::Deny { message: Some("no publishing from the walkthrough".into()) },
        )
        .await?;
    }
    Ok(checks)
}

// --- 2. Plan mode writes nothing ---------------------------------------------------------

/// In Plan the workspace is unchanged whatever the model tries, the todo list fills, and
/// `exit_plan_mode` is held for the person; approving it switches the session to Ask
/// (spec 3.1 §7.4, §11 `live_plan_mode_writes_nothing`).
async fn plan_mode(models: &Models) -> Result<Checks> {
    let mut checks = Checks::default();
    let model = models.model(vec![
        call("read_file", json!({ "path": "src/lib.rs" })),
        call("edit_file", json!({ "path": "src/lib.rs", "old_string": "a - b", "new_string": "a + b" })),
        call(
            "write_todos",
            json!({ "todos": [
                { "content": "Make `add` in src/lib.rs return `a + b`", "status": "pending" },
                { "content": "Run `cargo test --quiet`", "status": "pending" }
            ] }),
        ),
        call(
            "exit_plan_mode",
            json!({ "plan": "1. In src/lib.rs, make `add` return `a + b`.\n2. Run `cargo test --quiet`.", "next_mode": "ask" }),
        ),
        text("Plan approved."),
    ]);
    let dir = tempfile::tempdir()?;
    write_crate(dir.path(), true)?;
    let before = tree_digest(dir.path());
    let env =
        Env::in_dir(dir, model, |b| b.mode(HarnessMode::Plan).without_default_members()).await?;
    let (user, session) = ids("alice", "s1")?;

    println!("  1. A fix-the-bug task in Plan mode");
    let turn = env
        .send(
            "alice",
            "s1",
            "The test in src/lib.rs fails. Fix the bug. Your first tool call must be write_todos, listing the \
             steps you will take; then make the change. If the session's mode does not let you change files, \
             present your plan with exit_plan_mode instead: next_mode \"ask\", the plan as text, no steps.",
        )
        .await?;
    let after = tree_digest(env.dir.path());
    checks.check(format!("the workspace tree is unchanged ({after})"), after == before);
    let attempts: Vec<_> = turn
        .calls()
        .into_iter()
        .filter(|c| matches!(c.name.as_str(), "write_file" | "edit_file" | "bash"))
        .collect();
    checks.check(
        format!("every edit or shell call was refused ({} attempts)", attempts.len()),
        attempts.iter().all(|c| c.outcome() == Outcome::Refused),
    );
    let todos = env.harness.todos(&user, &session).await?;
    checks.check(
        format!("the todo list is non-empty ({} items)", todos.items.len()),
        !todos.items.is_empty(),
    );
    let pending = env.pending("alice", "s1").await?;
    checks.check(
        "exit_plan_mode is held for the person",
        pending.len() == 1 && pending[0].request.tool_name == "exit_plan_mode",
    );
    checks.check(
        "the session is still in Plan",
        env.harness.mode(&user, &session).await? == HarnessMode::Plan,
    );

    println!("  2. Approve the plan");
    let Some(held) = pending.first() else { return Ok(checks) };
    if let Some(steps) = held.request.args.get("steps") {
        println!("    the plan's steps: {steps}");
    }
    env.decide("alice", "s1", held, HarnessDecision::Approve).await?;
    let turn = env.resume("alice", "s1").await?;
    let exit = turn.calls_of("exit_plan_mode");
    checks.check(
        "the approved exit_plan_mode ran",
        exit.first().is_some_and(|c| c.outcome() == Outcome::Ran),
    );
    let record = env.harness.agent().runtime().mode_store().get(APP, "alice", "s1").await?;
    checks.check(
        "approving exit_plan_mode switched the session to Ask, set by the plan approval",
        record.is_some_and(|record| {
            record.mode == HarnessMode::Ask && record.set_by == ModeSource::PlanApproval
        }),
    );
    checks
        .check("harness.mode.changed is audited", !env.records("harness.mode.changed").is_empty());
    checks.check("the tree is still unchanged", tree_digest(env.dir.path()) == before);
    Ok(checks)
}

// --- 3. H4: plan graph ----------------------------------------------------------------------

const PLAN_PROMPT: &str = "Plan creating three files in the workspace: a.txt containing `alpha`, b.txt \
    containing `beta`, c.txt containing `gamma`, one file per step. Then call exit_plan_mode with next_mode \
    \"auto\" and exactly these three steps: {\"id\":\"1\",\"title\":\"a.txt\",\"instruction\":\"Create a.txt \
    containing alpha with write_file\"}, {\"id\":\"2\",\"title\":\"b.txt\",\"instruction\":\"Create b.txt \
    containing beta with write_file\",\"depends_on\":[\"1\"]}, {\"id\":\"3\",\"title\":\"c.txt\",\"instruction\":\
    \"Create c.txt containing gamma with write_file\",\"depends_on\":[\"2\"],\"gate\":\"approve\"}.";

fn modified(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

/// An approved plan with an `Approve` gate on step 3 compiles to a graph checkpointed in
/// SQLite; steps 1 and 2 run and the gate is a held call; the process stops; in a new
/// process the person denies the gate, which stops the plan; a rewind to step 2 asks again,
/// and after approval only step 3 runs (spec 3.1 §5 H4, §7.11).
async fn plan_graph(models: &Models) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join("ws"))?;
    let ws = dir.path().join("ws");
    let write =
        |path: &str, content: &str| call("write_file", json!({ "path": path, "content": content }));
    let steps = json!([
        { "id": "1", "title": "a.txt", "instruction": "Create a.txt containing alpha with write_file" },
        { "id": "2", "title": "b.txt", "instruction": "Create b.txt containing beta with write_file", "depends_on": ["1"] },
        { "id": "3", "title": "c.txt", "instruction": "Create c.txt containing gamma with write_file", "depends_on": ["2"], "gate": "approve" },
    ]);
    let first = models.model(vec![
        call("exit_plan_mode", json!({ "plan": "Create a.txt, b.txt and c.txt, one per step; ask before c.txt.", "next_mode": "auto", "steps": steps })),
        write("a.txt", "alpha"),
        text("a.txt created."),
        write("b.txt", "beta"),
        text("b.txt created."),
    ]);
    let stamps = {
        let process =
            Durable::open(dir.path(), first, HarnessMode::Plan, |b| b.without_default_members())
                .await?;
        println!("  1. Plan, and approve the plan");
        process.send("alice", "s1", PLAN_PROMPT).await?;
        let pending = process.pending("alice", "s1").await?;
        checks.check(
            "exit_plan_mode is held",
            pending.iter().any(|p| p.request.tool_name == "exit_plan_mode"),
        );
        process.approve_all("alice", "s1").await?;
        process.resume("alice", "s1").await?;
        let compiled = process.records("harness.plan.compiled");
        checks.check(
            "the approved plan compiled with interrupt_before [step:3]",
            compiled.first().is_some_and(|r| r.ext["interruptBefore"] == json!(["step:3"])),
        );

        println!("  2. Run the plan: steps 1 and 2, then the gate");
        process.send("alice", "s1", "Go ahead.").await?;
        checks.check(
            "steps 1 and 2 ran; step 3 waits",
            ws.join("a.txt").exists() && ws.join("b.txt").exists() && !ws.join("c.txt").exists(),
        );
        let pending = process.pending("alice", "s1").await?;
        checks.check(
            "the gate is a held plan_step_gate call for step 3",
            pending.len() == 1
                && pending[0].request.tool_name == PLAN_GATE_TOOL
                && pending[0].request.args["stepId"] == "3",
        );
        let graphs = process.harness.agent().runtime().plan_graph().cloned();
        let checkpoints = match &graphs {
            Some(graphs) => match graphs.active(APP, "alice", "s1").await? {
                Some(plan) => graphs.checkpointer().list(&plan.thread_id).await?.len(),
                None => 0,
            },
            None => 0,
        };
        checks.check(
            format!("the plan graph is checkpointed in SQLite ({checkpoints} checkpoints)"),
            checkpoints > 0,
        );
        (modified(&ws.join("a.txt")), modified(&ws.join("b.txt")))
        // The process stops here.
    };

    println!("  3. A new process: the person denies step 3 at its gate");
    let second = models.model(vec![write("c.txt", "gamma"), text("c.txt created.")]);
    let process =
        Durable::open(dir.path(), second, HarnessMode::Plan, |b| b.without_default_members())
            .await?;
    let pending = process.pending("alice", "s1").await?;
    checks.check(
        "the held gate survived the restart",
        pending.len() == 1 && pending[0].request.tool_name == PLAN_GATE_TOOL,
    );
    let Some(gate) = pending.first() else { return Ok(checks) };
    let (user, session) = ids("alice", "s1")?;
    println!("    [alice/s1] deny {PLAN_GATE_TOOL} for step 3");
    process
        .harness
        .decide(
            &user,
            &session,
            &gate.fingerprint,
            HarnessDecision::Deny { message: Some("not yet".into()) },
        )
        .await?;
    process.resume("alice", "s1").await?;
    let Some(graphs) = process.harness.agent().runtime().plan_graph().cloned() else {
        return Ok(checks);
    };
    let plan = graphs.active(APP, "alice", "s1").await?;
    checks.check(
        "the denied gate stopped the plan before step 3",
        plan.is_some_and(|plan| plan.status == PlanRunStatus::Cancelled)
            && !ws.join("c.txt").exists(),
    );

    println!("  4. Rewind to step 2, approve the gate, resume");
    let thread = graphs.rewind(APP, "alice", "s1", "2").await?;
    checks.check(
        format!("rewind forked the checkpointed thread after step 2 ({thread})"),
        thread.ends_with(":rewind:1"),
    );
    process.send("alice", "s1", "I changed my mind: run the rest of the plan.").await?;
    let pending = process.pending("alice", "s1").await?;
    checks.check(
        "step 3's gate asks again, as a new approval",
        pending.len() == 1
            && pending[0].request.tool_name == PLAN_GATE_TOOL
            && pending[0].request.args["run"] == 1,
    );
    process.approve_all("alice", "s1").await?;
    let turn = process.resume("alice", "s1").await?;
    let writes: Vec<String> = turn
        .calls_of("write_file")
        .iter()
        .map(|c| c.args["path"].as_str().unwrap_or_default().to_string())
        .collect();
    checks.check(
        format!("after the gate only step 3 ran (writes {writes:?})"),
        writes == ["c.txt"]
            && std::fs::read_to_string(ws.join("c.txt")).unwrap_or_default().contains("gamma"),
    );
    checks.check(
        "steps 1 and 2 never ran again: a.txt and b.txt are untouched",
        stamps == (modified(&ws.join("a.txt")), modified(&ws.join("b.txt"))),
    );
    let plan = graphs.active(APP, "alice", "s1").await?;
    checks.check(
        "the rewound plan is done",
        plan.is_some_and(|plan| plan.status == PlanRunStatus::Done && plan.rewinds == 1),
    );
    Ok(checks)
}

/// `src/lib.rs` of the `calc` crate with `add` fixed.
const FIXED_LIB: &str = "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(2, 3), 5);\n    }\n}\n";

// --- 4. H1: durable goal ---------------------------------------------------------------------

/// A goal with `until: cargo test --quiet` runs as a durable run on SQLite; it parks on a
/// held write; the process stops; a new process approves, the run resumes in the same
/// session, the until check passes, and the action ledger holds one record per approved call
/// (spec 3.1 §5 H1, §7.11, §7.12).
async fn durable_goal(models: &Models) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let ws = dir.path().join("ws");
    write_crate(&ws, true)?;
    let (user, session) = ids("alice", "s1")?;
    let spec = GoalSpec {
        goal: "Make the failing test in src/lib.rs pass: read the file, then rewrite the whole file with \
               write_file so that `add` returns `a + b` (keep the test as it is), then run exactly \
               `cargo test --quiet` with bash."
            .into(),
        until: "cargo test --quiet".into(),
        max_iterations: 3,
        check_timeout: Duration::from_secs(300),
    };
    let first = models.model(vec![
        call("read_file", json!({ "path": "src/lib.rs" })),
        call("write_file", json!({ "path": "src/lib.rs", "content": FIXED_LIB })),
    ]);
    let (run_id, seen) = {
        let process =
            Durable::open(dir.path(), first, HarnessMode::Ask, |b| b.without_default_members())
                .await?;
        println!("  1. Start the goal as a durable run");
        process.harness.open_session(&user, Some(&session)).await?;
        let turn = collect(process.harness.start_goal(&user, &session, spec).await?).await?;
        let run_id = turn
            .events
            .iter()
            .find_map(|event| event.provider_metadata.get("harness.goal.run_id").cloned())
            .unwrap_or_default();
        let worker = process.worker("first");
        let state = process
            .wait_for(&run_id, &[RunState::Waiting, RunState::Completed, RunState::Failed])
            .await?;
        println!("    the durable run's turn, from the session:");
        let seen = process.print_session("alice", "s1", 0).await?;
        let pending = process.pending("alice", "s1").await?;
        checks.check(
            format!("the run parked on a held call ({state:?}, pending {})", describe(&pending)),
            state == RunState::Waiting && !pending.is_empty(),
        );
        checks.check(
            "nothing changed while it waits",
            std::fs::read_to_string(ws.join("src/lib.rs"))?.contains("a - b"),
        );
        println!("  2. The process stops");
        worker.abort();
        (run_id, seen)
    };

    println!("  3. A new process approves; the run resumes");
    let second = models.model(vec![bash("cargo test --quiet"), text("The tests pass.")]);
    let process =
        Durable::open(dir.path(), second, HarnessMode::Ask, |b| b.without_default_members())
            .await?;
    let survived = process.pending("alice", "s1").await?;
    checks.check(
        format!("the held call survived the restart ({})", describe(&survived)),
        !survived.is_empty(),
    );
    let worker = process.worker("second");
    let mut approved: Vec<String> = Vec::new();
    let mut state = RunState::Waiting;
    for _ in 0..20 {
        state = process
            .wait_for(&run_id, &[RunState::Waiting, RunState::Completed, RunState::Failed])
            .await?;
        if state != RunState::Waiting || approved.len() >= 8 {
            break;
        }
        approved.extend(process.approve_all("alice", "s1").await?);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    worker.shutdown().await;
    println!("    the durable run's turns, from the session:");
    process.print_session("alice", "s1", seen).await?;
    checks.check(
        format!("run {run_id} completed in the new process ({state:?}, approvals {approved:?})"),
        state == RunState::Completed,
    );
    let goal = process.harness.goal(&user, &session).await?;
    println!("    goal: {goal:?}");
    checks.check(
        "the goal is done in the same session",
        goal.as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Done && goal.last_check == Some(0)),
    );
    for tool in ["write_file", "edit_file", "bash"] {
        let records = process.ledger_records(tool).await?;
        let approvals = approved.iter().filter(|name| *name == tool).count();
        if records.is_empty() && approvals == 0 {
            continue;
        }
        checks.check(
            format!("the action ledger holds one completed record per approved {tool} ({} records, {approvals} approvals)", records.len()),
            records.len() == approvals && records.iter().all(|(_, outcome)| outcome.is_some()),
        );
    }
    // The goal's own `until` check is audited as a bash begin too, marked `goalCheck`.
    let begins = process
        .records("harness.bash.begin")
        .into_iter()
        .filter(|r| r.ext.get("goalCheck") != Some(&json!(true)))
        .count();
    checks.check(
        format!("each approved bash began once ({begins} begins)"),
        begins == approved.iter().filter(|name| *name == "bash").count(),
    );
    checks.check("cargo test --quiet passes", cargo_test_passes(&ws));
    Ok(checks)
}

// --- 5. H3: team ---------------------------------------------------------------------------

/// A local `explore` member answers a question in its own context, audited with its parent;
/// in Plan the full-tool `general` member's write and shell calls are refused, because a
/// member runs under the session's stricter mode (spec 3.1 §5 H3, §7.10).
async fn team(models: &Models) -> Result<Checks> {
    let mut checks = Checks::default();
    const QUESTION: &str = "Ask the `explore` member which function in src/lib.rs is wrong and why, then tell \
        me its answer with the file path. Do not read the files yourself.";
    let coordinator = models.model(vec![
        call("explore", json!({ "request": "Which function in src/lib.rs is wrong, and why? Give the file path and line." })),
        text("`add` in src/lib.rs subtracts."),
    ]);
    let member = models.model(vec![
        call("read_file", json!({ "path": "src/lib.rs" })),
        text("src/lib.rs:3: `add` returns `a - b`; it should return `a + b`."),
    ]);
    let dir = tempfile::tempdir()?;
    write_crate(dir.path(), true)?;
    let member_model = member.clone();
    let env =
        Env::in_dir(dir, coordinator, move |b| b.mode(HarnessMode::Ask).member_model(member_model))
            .await?;

    println!("  1. The explore member answers a question");
    let turn = env.send("alice", "s1", QUESTION).await?;
    let delegated = turn.calls_of("explore");
    let request = delegated
        .first()
        .and_then(|c| c.args.get("request"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    checks.check(
        "the coordinator delegated to explore and got an answer",
        delegated.first().is_some_and(|c| c.outcome() == Outcome::Ran),
    );
    let first = member.requests().into_iter().next();
    // Everything the member's first model call carried, as text.
    let seen: String = first
        .iter()
        .flat_map(|request| request.contents.iter())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            adk_core::Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    checks.check(
        "the member saw the delegated request, not the parent's conversation",
        !request.is_empty()
            && seen.contains(&request.chars().take(40).collect::<String>())
            && !seen.contains(QUESTION),
    );
    checks.check(
        "the member read the code itself",
        member.requests().len() >= 2
            && turn.calls_of("read_file").iter().any(|c| c.author == "explore"),
    );
    let spawned = env.records("harness.member.spawned");
    checks.check(
        "harness.member.spawned records explore as a local member of harness",
        spawned.iter().any(|r| {
            r.ext["member"] == "explore"
                && r.ext["kind"] == "local"
                && r.ext["parentAgentId"].as_str().is_some_and(|id| id.ends_with("/harness"))
        }),
    );
    checks.check(
        "harness.member.finished records it completed",
        env.records("harness.member.finished")
            .iter()
            .any(|r| r.ext["member"] == "explore" && r.ext["outcome"] == "completed"),
    );

    println!("  2. In Plan, a member inherits the stricter mode");
    const MEMBER_TASK: &str = "Create notes.txt containing hello with write_file, then run `touch ran.txt` with \
        bash. Make both calls even if you expect the session's mode to refuse them: the harness decides, and I want \
        its answers. Report exactly what each call returned.";
    let coordinator = models.model(vec![
        call("general", json!({ "request": MEMBER_TASK })),
        text("The member could not change anything in plan mode."),
    ]);
    let member = models.model(vec![
        call("write_file", json!({ "path": "notes.txt", "content": "hello" })),
        call("bash", json!({ "command": "touch ran.txt" })),
        text("Both calls were refused: the session is in plan mode."),
    ]);
    let member_model = member.clone();
    let env = Env::new(coordinator, move |b| b.mode(HarnessMode::Plan).member_model(member_model))
        .await?;
    let turn = env
        .send(
            "alice",
            "s1",
            &format!(
                "Delegate this task to the `general` member, word for word: \"{MEMBER_TASK}\" Then tell me what the \
                 member reported. Do not call write_file or bash yourself."
            ),
        )
        .await?;
    let attempts: Vec<_> = turn
        .calls()
        .into_iter()
        .filter(|c| {
            c.author == "general" && matches!(c.name.as_str(), "write_file" | "edit_file" | "bash")
        })
        .collect();
    checks.check(
        format!(
            "the general member's write and shell calls were refused ({} attempts)",
            attempts.len()
        ),
        !attempts.is_empty() && attempts.iter().all(|c| c.outcome() == Outcome::Refused),
    );
    checks.check(
        "nothing was written",
        !env.path("notes.txt").exists() && !env.path("ran.txt").exists(),
    );
    Ok(checks)
}

// --- 6. H6: identity ----------------------------------------------------------------------

/// The permissions of each user, as an identity provider would grant them.
struct Roles;

#[async_trait::async_trait]
impl adk_auth::ScopeResolver for Roles {
    async fn resolve(&self, ctx: &dyn ToolContext) -> Vec<String> {
        match ctx.user_id() {
            "alice" => vec!["harness:shell".into(), "harness:edit".into()],
            _ => Vec::new(),
        }
    }
}

/// A role map requires `harness:shell` for `bash`: alice has it and her command runs; bob
/// has no permissions and his is refused with a code naming the permission, even in Auto
/// with host commands allowed (spec 3.1 §5 H6, §7.14).
async fn identity(models: &Models) -> Result<Checks> {
    let mut checks = Checks::default();
    let model = models.model(vec![
        bash("echo alice >> who.txt"),
        text("Done."),
        bash("echo bob >> who.txt"),
        text("The shell is not available to me."),
    ]);
    let env = Env::new(model, |b| {
        b.rbac(RbacMap::harness_defaults().with_scope_resolver(Arc::new(Roles)))
            .mode(HarnessMode::Auto)
            .allow_host_shell_in_auto()
            .without_default_members()
    })
    .await?;
    let ask = |name: &str| {
        format!(
            "Run exactly `echo {name} >> who.txt` with the bash tool. Do not use any other tool. Then answer in one line: what did the call return?"
        )
    };

    println!("  1. alice holds harness:shell");
    let turn = env.send("alice", "s1", &ask("alice")).await?;
    checks.check("alice's command ran", turn.last_outcome("bash") == Outcome::Ran);

    println!("  2. bob holds no permission");
    let turn = env.send("bob", "s1", &ask("bob")).await?;
    let refused = turn.calls_of("bash");
    checks.check(
        "bob's bash is refused with harness.rbac.denied:harness:shell",
        !refused.is_empty()
            && refused.iter().all(|c| {
                c.outcome() == Outcome::Refused
                    && c.error().contains("harness.rbac.denied:harness:shell")
            }),
    );
    let who = std::fs::read_to_string(env.path("who.txt")).unwrap_or_default();
    checks.check(format!("who.txt holds alice's line only ({who:?})"), who == "alice\n");
    let begins = env.records("harness.bash.begin");
    checks.check(
        "one harness.bash.begin, acting for alice",
        begins.len() == 1
            && begins[0].actor.on_behalf_of.as_ref().is_some_and(|subject| subject.id == "alice"),
    );
    Ok(checks)
}

// --- 7. H7: the harness watches itself ------------------------------------------------------

fn priced(response: ScriptedResponse, tokens: i32, usd: f64) -> ScriptedResponse {
    response.with_usage(UsageMetadata {
        prompt_token_count: tokens * 9 / 10,
        candidates_token_count: tokens - tokens * 9 / 10,
        total_token_count: tokens,
        cost: Some(usd),
        ..UsageMetadata::default()
    })
}

/// `Harness::spend` reports each run per acting agent and subject, the report behind the
/// CLI's `/cost`; the loop guard ends a turn of identical reads and tightens the session to
/// ReadOnly (spec 3.1 §5 H7, §7.13).
async fn watches_itself(models: &Models) -> Result<Checks> {
    let mut checks = Checks::default();
    println!("  1. The spend report");
    let model = models.model(vec![
        priced(call("read_file", json!({ "path": "notes.txt" })), 1_200, 0.0024),
        priced(text("It says: ship on Friday."), 1_300, 0.0026),
    ]);
    let env = Env::new(model, |b| b.without_default_members()).await?;
    std::fs::write(env.path("notes.txt"), "ship on Friday\n")?;
    env.send("alice", "s1", "Read notes.txt with read_file and tell me what it says, in one line.")
        .await?;
    let alice = UserId::new("alice")?;
    let report = env.harness.spend(Some(&alice));
    println!(
        "    /cost\n{}",
        report.render().lines().map(|line| format!("      {line}")).collect::<Vec<_>>().join("\n")
    );
    checks.check(
        format!(
            "one run, {} model calls, {} tokens for alice",
            report.model_calls, report.total_tokens
        ),
        report.runs == 1 && report.model_calls >= 2 && report.total_tokens > 0,
    );
    let agent = format!("urn:adk:agent:{APP}/harness");
    checks.check(
        "every entry is keyed by the acting agent and the subject",
        !report.entries.is_empty()
            && report.entries.iter().all(|entry| {
                entry.key.agent.as_deref() == Some(agent.as_str())
                    && entry.subject.as_ref().is_some_and(|s| s.id == "alice")
            }),
    );
    if !models.is_live() {
        checks.check(
            format!("the cost is the models' reported cost (${:.4})", report.cost_usd()),
            report.cost_micro_usd == 5_000,
        );
    }
    let spend = env.records("harness.spend");
    checks.check(
        "the run's spend is audited as harness.spend",
        spend.len() == 1 && spend[0].ext["spendKey"]["subject"]["id"] == "alice",
    );

    println!("  2. The loop guard");
    let read = || call("read_file", json!({ "path": "notes.txt" }));
    let model = models.model(vec![read(), read(), read(), read(), text("done")]);
    let env = Env::new(model, |b| {
        b.loop_guard(LoopGuard::default()).mode(HarnessMode::Auto).without_default_members()
    })
    .await?;
    std::fs::write(env.path("notes.txt"), "ship on Friday\n")?;
    let turn = env
        .send(
            "alice",
            "s1",
            "This is a test of loop detection. Call read_file on notes.txt four times, one call after another, \
             with the same arguments each time, before you answer.",
        )
        .await?;
    checks.check(
        "the turn stopped with harness.loop_detected",
        turn.stopped_with(LOOP_DETECTED_CODE),
    );
    let record = env.harness.agent().runtime().mode_store().get(APP, "alice", "s1").await?;
    checks.check(
        "the session was tightened to ReadOnly by the system",
        record.is_some_and(|record| {
            record.mode == HarnessMode::ReadOnly && record.set_by == ModeSource::SystemTighten
        }),
    );
    let detected = env.records("harness.loop.detected");
    checks.check(
        format!(
            "harness.loop.detected is audited ({})",
            detected.first().map(|r| r.ext["identicalCalls"].to_string()).unwrap_or_default()
        ),
        detected.len() == 1
            && detected[0].ext["action"] == "tighten_read_only"
            && detected[0].ext["identicalCalls"].as_u64().is_some_and(|n| n >= 3),
    );
    Ok(checks)
}

// --- 8. The sandbox --------------------------------------------------------------------------

/// The person at the prompt: approves sandboxed commands once and refuses every
/// unsandboxed retry.
#[derive(Debug, Default)]
struct SandboxPrompter {
    kinds: Mutex<Vec<ToolKind>>,
}

#[async_trait::async_trait]
impl ApprovalPrompter for SandboxPrompter {
    async fn prompt(&self, prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        println!("      prompt: {} | {}", prompt.rendered.title, clip(&prompt.rendered.body, 200));
        self.kinds.lock().unwrap_or_else(|e| e.into_inner()).push(prompt.kind);
        Ok(match prompt.kind {
            ToolKind::Shell(adk_harness::Isolation::OsSandbox) => PromptAnswer::ApproveOnce,
            _ => PromptAnswer::Deny {
                message: Some("the walkthrough keeps commands in the sandbox".into()),
            },
        })
    }
}

/// Under the OS sandbox (Seatbelt on macOS, bubblewrap on Linux), a command writes the
/// workspace but not outside it, and its result carries the denial hint; an unsandboxed
/// retry needs its own approval, which the person refuses (spec 3.1 §7.9).
async fn sandbox(models: &Models) -> Result<Checks> {
    let status = SandboxLauncher::new(OsSandboxConfig::default()).status();
    let IsolationStatus::OsSandbox { enforcer } = &status else {
        return Ok(Checks::skipped(format!("no OS sandbox on this host ({status})")));
    };
    let mut checks = Checks::default();
    let outside = tempfile::tempdir()?;
    let target = outside.path().join("escaped.txt");
    // Plain commands: the harness prompt asks the model for no chaining or redirection.
    let escape = format!("touch {}", target.display());
    let model = models.model(vec![
        bash("touch inside.txt"),
        bash_with_id("call-a", &escape),
        call(
            "bash",
            json!({ "command": escape, "unsandboxed": true, "retry_of": "call-a", "reason": "the command writes outside the workspace" }),
        ),
        text("inside.txt was created; the sandbox refused the file outside the workspace, and the unsandboxed retry was declined."),
    ]);
    let prompter = Arc::new(SandboxPrompter::default());
    let handle = prompter.clone();
    let env = Env::new(model, move |b| {
        b.shell(ShellIsolation::OsSandbox(OsSandboxConfig::default()))
            .prompter(handle)
            .without_default_members()
    })
    .await?;
    checks.check(
        format!("bash runs under {enforcer}"),
        env.harness.agent().info().isolation == status,
    );
    let turn = env
        .send(
            "alice",
            "s1",
            &format!(
                "Run these two commands with the bash tool, one call each, exactly as written: `touch inside.txt`, \
                 then `{escape}`. Then report what happened."
            ),
        )
        .await?;
    checks.check("the workspace write happened", env.path("inside.txt").exists());
    checks.check("nothing was written outside the workspace", !target.exists());
    let sandboxed = turn
        .calls_of("bash")
        .into_iter()
        .find(|c| command_of(&c.args) == escape && c.args.get("unsandboxed").is_none());
    let response = sandboxed.and_then(|c| c.response).unwrap_or(Value::Null);
    checks.check(
        format!(
            "the write outside failed under {enforcer} with a hint ({})",
            response.pointer("/sandbox/hint").and_then(Value::as_str).unwrap_or("none")
        ),
        response.get("exit_code").is_some_and(|code| *code != 0)
            && response["sandbox"]["enforcer"] == enforcer.as_str()
            && response.pointer("/sandbox/hint").is_some(),
    );
    if cfg!(target_os = "macos") {
        checks.check(
            "Seatbelt's refusal is recognized and the hint names the retry",
            response["sandbox"]["denialSuspected"] == true
                && response
                    .pointer("/sandbox/hint")
                    .and_then(Value::as_str)
                    .is_some_and(|hint| hint.contains("retry_of")),
        );
    }
    let begins = env.records("harness.bash.begin");
    checks.check(
        "harness.bash.begin records a read-write workspace and no network",
        begins
            .first()
            .is_some_and(|r| r.ext["workspaceAccess"] == "read_write" && r.ext["network"] == false),
    );
    let kinds = prompter.kinds.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let retries =
        turn.calls_of("bash").into_iter().filter(|c| c.args.get("unsandboxed").is_some()).count();
    checks.check(
        format!("every unsandboxed retry was put to the person and refused ({retries} retries, prompts {kinds:?})"),
        kinds.iter().filter(|kind| **kind == ToolKind::ShellEscalation).count() == retries && !target.exists(),
    );
    Ok(checks)
}

// --- 9. H11: long-term memory ----------------------------------------------------------------

/// Long-term facts with provenance (spec 3.1 §5 H11, §7.15.2). The crate's build script
/// sets `harness_fact_store` when `adk-harness` exports `FactStore`.
async fn long_term_memory(_models: &Models) -> Result<Checks> {
    if cfg!(harness_fact_store) {
        return Ok(Checks::skipped(
            "adk-harness exports FactStore; this walkthrough has no H11 scenario for it yet (spec 3.1 §7.15.2)",
        ));
    }
    Ok(Checks::skipped(
        "adk-harness on this checkout does not export FactStore (spec 3.1 §7.15.2 long-term facts are not merged)",
    ))
}
