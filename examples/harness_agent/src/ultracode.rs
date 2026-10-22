//! Adaptive `ultracode` (spec 3.1 §7.10 "Adaptive review", §13 question 4).
//!
//! The review of a change is routed once, on the change: a security-sensitive path goes to a
//! Blackboard board without a model call, anything else to one closed-schema router call whose
//! invalid answers fall back to a deterministic heuristic, and every decision replays from its
//! `harness.ultracode.routed` record. A board's seats read each other as other-agent context,
//! stay read-only under the harness policy, and fail closed on their budget.

use std::path::Path;
use std::sync::Arc;

use adk_cli::harness::ultracode::{
    Architecture, BoardStatus, Focus, IndependentReviewer, RepoSignals, ReviewBoard, RouteInputs, Rule,
    TaskFeatures, UltracodeFlags, UltracodeLimits, Verdict, WorkspaceSnapshot, replay, route,
};
use adk_core::audit_log::{AuditRecordKind, AuditRecorder};
use adk_core::{Llm, Part, UsageMetadata};
use adk_model::{ScriptedLlm, ScriptedResponse};
use anyhow::Result;
use serde_json::json;

use crate::{Checks, Subject};

pub const SCRIPTED: &[&str] = &[
    "ultracode_routes_by_change",
    "ultracode_board_reads_each_other",
    "ultracode_board_is_read_only",
    "ultracode_board_budget_fails_closed",
];

pub const LIVE: &[&str] = &["live_ultracode"];

/// Runs scripted scenario `name`, if it is one of these.
pub async fn run_scripted(name: &str, subject: Subject) -> Option<Result<Checks>> {
    if !SCRIPTED.contains(&name) {
        return None;
    }
    if subject == Subject::Baseline {
        return Some(Ok(match name {
            "ultracode_routes_by_change" => {
                let mut checks = Checks::default();
                checks.check("main runs one fixed review plan and records no routing decision", false);
                checks
            }
            "ultracode_board_reads_each_other" => {
                let mut checks = Checks::default();
                checks.check(
                    "main's review_round hands seat 1's turns to seat 2 as seat 2's own model turns",
                    false,
                );
                checks
            }
            _ => Checks::skipped("main has no review board"),
        }));
    }
    Some(match name {
        "ultracode_routes_by_change" => routes_by_change().await,
        "ultracode_board_reads_each_other" => board_reads_each_other().await,
        "ultracode_board_is_read_only" => board_is_read_only().await,
        _ => board_budget_fails_closed().await,
    })
}

fn reply(text: &str) -> ScriptedResponse {
    ScriptedResponse::text(text).with_usage(UsageMetadata {
        prompt_token_count: 400,
        candidates_token_count: 40,
        total_token_count: 440,
        ..UsageMetadata::default()
    })
}

fn write(root: &Path, path: &str, text: &str) -> Result<()> {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap_or(root))?;
    std::fs::write(path, text)?;
    Ok(())
}

/// The routing inputs of `change` applied to a fresh workspace with `setup`.
fn inputs(task: &str, setup: &[(&str, &str)], change: &[(&str, &str)]) -> Result<RouteInputs> {
    let dir = tempfile::tempdir()?;
    for (path, text) in setup {
        write(dir.path(), path, text)?;
    }
    let before = WorkspaceSnapshot::capture(dir.path());
    for (path, text) in change {
        write(dir.path(), path, text)?;
    }
    let after = WorkspaceSnapshot::capture(dir.path());
    let limits = UltracodeLimits::resolve(None, dir.path(), &UltracodeFlags::default())?;
    Ok(RouteInputs {
        features: TaskFeatures::extract(task),
        change: before.diff_with(&after, &limits),
        repo: RepoSignals::from_snapshot(&after),
        limits,
    })
}

async fn routes_by_change() -> Result<Checks> {
    let mut checks = Checks::default();

    let security = inputs("make tokens expire", &[("src/lib.rs", "")], &[("src/auth/token.rs", "pub fn expiry() {}")])?;
    let idle = Arc::new(ScriptedLlm::new("router", Vec::<ScriptedResponse>::new()));
    let router: Arc<dyn Llm> = idle.clone();
    let routed = route(&router, None, "make tokens expire", security).await;
    checks.check(
        "a security path routes to a board with a security seat, by rule",
        routed.rule == Rule::Sensitive
            && routed.plan.architecture == Architecture::Blackboard
            && routed.plan.reviewers.contains(&Focus::Security),
    );
    checks.check("and makes no router call", idle.requests().is_empty());
    let mut exts = vec![routed.record_ext("run-1", "sha256:task")];

    let packages = &[("crates/greet/Cargo.toml", "[package]"), ("crates/app/Cargo.toml", "[package]")];
    let cross = inputs(
        "add a Greeter trait and use it",
        packages,
        &[("crates/greet/src/lib.rs", "pub trait Greeter {}"), ("crates/app/src/main.rs", "fn main() {}")],
    )?;
    let answering = Arc::new(ScriptedLlm::new(
        "router",
        [reply(r#"{"architecture":"blackboard","specialists":["api-compat"],"boardRounds":1,"rationale":"a trait spans two crates"}"#)],
    ));
    let router: Arc<dyn Llm> = answering.clone();
    let routed = route(&router, None, "add a Greeter trait and use it", cross.clone()).await;
    checks.check(
        "a cross-package change applies the model's whole answer",
        routed.rule == Rule::Model
            && routed.plan.architecture == Architecture::Blackboard
            && routed.plan.reviewers == [Focus::Correctness, Focus::EdgeCases, Focus::ApiCompat, Focus::Style]
            && routed.inputs.change.packages_touched == 2,
    );
    let request = answering.requests().into_iter().next();
    checks.check(
        "the router call has no tools and the closed schema",
        request.is_some_and(|request| {
            request.tools.is_empty()
                && request.config.as_ref().and_then(|config| config.response_schema.as_ref()).is_some()
        }),
    );
    exts.push(routed.record_ext("run-2", "sha256:task"));

    let prose = Arc::new(ScriptedLlm::new("router", [reply("A graph review would do.")]));
    let router: Arc<dyn Llm> = prose;
    let routed = route(&router, None, "add a Greeter trait and use it", cross).await;
    checks.check(
        "a prose answer falls back to the heuristic, with router.unparseable recorded",
        routed.rule == Rule::Heuristic
            && routed.fallback.as_ref().is_some_and(|fallback| fallback.code == "router.unparseable")
            && routed.plan == routed.heuristic.plan,
    );
    exts.push(routed.record_ext("run-3", "sha256:task"));

    checks.check(
        "every routed record replays to its plan",
        exts.iter().all(|ext| replay(ext).is_ok_and(|plan| serde_json::to_value(plan).ok() == Some(ext["plan"].clone()))),
    );
    let mut tampered = exts[1].clone();
    tampered["inputs"]["limits"]["maxReviewers"] = json!(3);
    checks.check("a tampered limit does not replay", replay(&tampered).is_err());
    Ok(checks)
}

/// The user-role texts of `request`.
fn user_texts(request: &adk_core::LlmRequest) -> Vec<String> {
    request
        .contents
        .iter()
        .filter(|content| content.role == "user")
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .map(str::to_string)
        .collect()
}

async fn board_reads_each_other() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    write(dir.path(), "src/lib.rs", "pub fn last(v: &[i32]) -> i32 { v[v.len() - 2] }\n")?;
    let model = Arc::new(ScriptedLlm::new(
        "board",
        [
            ScriptedResponse::function_call("read_file", json!({ "path": "src/lib.rs" })),
            reply("last() reads the second-to-last element.\nVERDICT: changes\n- index len() - 1"),
            reply("Agreed with correctness; nothing else.\nVERDICT: changes"),
            reply("VERDICT: approve"),
        ],
    ));
    let outcome = ReviewBoard::new(model.clone(), dir.path()).model_calls(10).run("Review src/lib.rs now.").await;
    let requests = model.requests();
    let second = requests.get(2).map(user_texts).unwrap_or_default();
    checks.check(
        "seat 2 reads seat 1's note as other-agent context",
        second.iter().any(|text| text.starts_with("[review_correctness] said:") && text.contains("second-to-last")),
    );
    checks.check(
        "and seat 1's tool call too",
        second.iter().any(|text| text.starts_with("[review_correctness] called tool `read_file`")),
    );
    let own_turns = requests.get(2).is_some_and(|request| {
        request
            .contents
            .iter()
            .filter(|content| content.role == "model")
            .any(|content| content.parts.iter().filter_map(Part::text).any(|text| text.contains("second-to-last")))
    });
    checks.check("never as seat 2's own model turn", !own_turns);
    checks.check(
        "each seat's verdict is its last turn's",
        outcome.status == BoardStatus::Completed
            && outcome.seats.iter().map(|seat| seat.verdict).collect::<Vec<_>>()
                == [Verdict::Changes, Verdict::Changes, Verdict::Approve],
    );
    Ok(checks)
}

async fn board_is_read_only() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    write(dir.path(), "README.md", "a project\n")?;
    let before = crate::tree_digest(dir.path());
    let log = Arc::new(adk_harness::EphemeralAuditLog::new()?);
    let model = Arc::new(ScriptedLlm::new(
        "board",
        [
            ScriptedResponse::function_calls([
                ("write_file", json!({ "path": "README.md", "content": "rewritten" })),
                ("bash", json!({ "command": "touch planted" })),
                ("read_file", json!({ "path": "README.md" })),
            ]),
            reply("VERDICT: changes\n- the readme is thin"),
            reply("VERDICT: approve"),
        ],
    ));
    let outcome = ReviewBoard::new(model.clone(), dir.path())
        .roster(vec![Focus::Correctness, Focus::Style])
        .audit_log(log.clone() as Arc<dyn AuditRecorder>)
        .run("Review now.")
        .await;
    checks.check("a seat's write_file and bash change nothing", crate::tree_digest(dir.path()) == before);
    let declared: Vec<String> = model.requests().iter().flat_map(|request| request.tools.keys().cloned()).collect();
    checks.check(
        "seats declare read tools only",
        declared.iter().all(|name| ["read_file", "glob", "grep", "write_todos", "update_todo"].contains(&name.as_str())),
    );
    let records = log.records().await?;
    checks.check(
        "the seat's read is a governed call under the harness policy",
        records.iter().any(|record| {
            record.kind == AuditRecordKind::ToolCall
                && record.action == "read_file"
                && record.policy_ref.as_deref().is_some_and(|policy| policy.contains("adk-harness"))
        }),
    );
    for action in ["harness.member.spawned", "harness.member.finished"] {
        checks.check(
            format!("{action} once per seat in the run's log"),
            records.iter().filter(|record| record.action == action).count() == 2,
        );
    }
    checks.check("the log verifies", log.verify().await?);
    checks.check("the board completed", outcome.status == BoardStatus::Completed);
    Ok(checks)
}

async fn board_budget_fails_closed() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    write(dir.path(), "README.md", "a project\n")?;
    let model = Arc::new(ScriptedLlm::new(
        "board",
        [reply("VERDICT: approve"), reply("VERDICT: approve"), reply("VERDICT: approve")],
    ));
    let idle = Arc::new(ScriptedLlm::new("fallback", Vec::<ScriptedResponse>::new()));
    let outcome = ReviewBoard::new(model.clone(), dir.path())
        .model_calls(2)
        .fallback(IndependentReviewer::new(idle.clone(), dir.path()), 1_000)
        .run("Review now.")
        .await;
    checks.check("the board stops on its budget", outcome.status == BoardStatus::BudgetExhausted);
    checks.check(
        "the seat that never spoke fails closed",
        outcome.seats.iter().map(|seat| seat.verdict).collect::<Vec<_>>()
            == [Verdict::Approve, Verdict::Approve, Verdict::Failed]
            && !outcome.approved(),
    );
    checks.check("nothing runs again", idle.requests().is_empty() && model.requests().len() == 2);
    Ok(checks)
}

/// Runs live scenario `name`, if it is one of these.
pub async fn run_live(name: &str, model: Arc<dyn Llm>) -> Option<Result<Checks>> {
    (name == "live_ultracode").then_some(())?;
    Some(live_ultracode(model).await)
}

/// The router on a live model for a two-crate change, and a live board on a token-expiry
/// change: the routing replays, every seat ends with a verdict, and nothing is written.
async fn live_ultracode(model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    let task = "rename the trait Hello to Greeter across the greet and app crates";
    let cross = inputs(
        task,
        &[("greet/Cargo.toml", "[package]"), ("app/Cargo.toml", "[package]")],
        &[("greet/src/lib.rs", "pub trait Greeter {}"), ("app/src/lib.rs", "use greet::Greeter;")],
    )?;
    // As `adk-rust ultracode` routes: at the session's `members` effort (spec 3.1 §7.23).
    let policy = adk_harness::EffortPolicy::default();
    let routed = route(&model, Some(policy.members), task, cross).await;
    println!(
        "      route: {} ({}{}) · reviewers {:?}",
        routed.plan.architecture,
        routed.rule,
        routed.fallback.as_ref().map(|fallback| format!(": {}", fallback.code)).unwrap_or_default(),
        routed.plan.reviewers.iter().map(|focus| focus.name()).collect::<Vec<_>>()
    );
    checks.check(
        "the router's choice is the model's validated answer or a recorded fallback",
        routed.rule == Rule::Model || routed.fallback.is_some(),
    );
    checks.check("the routing replays", replay(&routed.record_ext("live", "sha256:task")).is_ok());

    let dir = tempfile::tempdir()?;
    write(
        dir.path(),
        "src/auth/session.rs",
        "pub struct Session { pub issued_at: u64 }\n\nimpl Session {\n    /// Valid for one hour.\n    pub fn is_valid(&self, now: u64) -> bool {\n        now.saturating_sub(self.issued_at) < 3600\n    }\n}\n",
    )?;
    let before = crate::tree_digest(dir.path());
    let log = Arc::new(adk_harness::EphemeralAuditLog::new()?);
    let outcome = ReviewBoard::new(model, dir.path())
        .effort(policy)
        .roster(vec![Focus::Correctness, Focus::EdgeCases, Focus::Security, Focus::Style])
        .audit_log(log.clone() as Arc<dyn AuditRecorder>)
        .model_calls(48)
        .run("The task being implemented is:\nSessions expire after one hour.\n\nReview the code in the workspace against this task now.")
        .await;
    for seat in &outcome.seats {
        println!("      {:<11} {:?} {}", seat.focus.name(), seat.verdict, seat.notes.chars().take(80).collect::<String>());
    }
    checks.check(
        "every seat ends with a verdict",
        outcome.status == BoardStatus::Completed && outcome.seats.iter().all(|seat| seat.verdict != Verdict::Failed),
    );
    checks.check("the board wrote nothing", crate::tree_digest(dir.path()) == before);
    checks.check("the board's log verifies", log.verify().await?);
    checks.check("the board stayed within its 48 model calls", outcome.model_calls <= 48);
    Ok(checks)
}
