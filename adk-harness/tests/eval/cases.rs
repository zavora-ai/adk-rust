//! The harness eval suite (spec 3.1 §7.13): `fix_failing_test`, `plan_mode_writes_nothing`
//! and `goal_until` as `adk-eval` cases, run on any model, scored into the metrics a
//! `BaselineStore` keeps. The scripted run is a test of `adk-harness`; the live run is
//! `examples/harness_eval`, which includes this file.
//!
//! Pass conditions of the scenarios are checked on files and commands, never on model prose.
//! The capability questions (spec 3.1 §7.25) are the rubric case: an answer passes when it
//! names the commands the harness's `<harness>` block lists for the question.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use adk_core::{Content, Llm, Part, SessionId, UserId};
use adk_eval::schema::ContentData;
use adk_eval::{
    EvalCase, IntermediateData, ToolCallRecord, ToolTrajectoryScorer, ToolUse, TraceAnalyzer, Turn,
};
use adk_harness::{CommandEntry, HarnessAgent, HarnessMode, InstructionsConfig, SurfaceManifest};
use adk_model::ScriptedResponse;
use adk_session::InMemorySessionService;
use futures::StreamExt;
use serde_json::{Value, json};

/// Eval set id of the suite in a baseline file.
pub const EVAL_SET_ID: &str = "adk-harness";

/// One scenario: an `adk-eval` case plus the workspace it runs in and how it passes.
pub struct Scenario {
    /// The case: its prompt and expected tool trajectory.
    pub case: EvalCase,
    /// The harness mode.
    pub mode: HarnessMode,
    /// Workspace files before the run.
    pub files: &'static [(&'static str, &'static str)],
    /// A command that must exit 0 for the case to pass, run after every turn; the prompt
    /// is sent again until it does, up to `max_iterations` turns (the `goal_until` loop).
    pub until: Option<&'static str>,
    /// Turns at most.
    pub max_iterations: u32,
    /// Whether the workspace must be unchanged.
    pub unchanged: bool,
    /// What a scripted model answers, in order.
    pub scripted: fn() -> Vec<ScriptedResponse>,
}

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn case(id: &str, description: &str, prompt: &str, expected: &[&str]) -> EvalCase {
    EvalCase {
        eval_id: id.to_string(),
        description: description.to_string(),
        conversation: vec![Turn {
            invocation_id: format!("{id}-1"),
            user_content: ContentData::text(prompt),
            final_response: None,
            intermediate_data: Some(IntermediateData {
                tool_uses: expected
                    .iter()
                    .map(|name| ToolUse {
                        name: (*name).to_string(),
                        args: Value::Null,
                        expected_response: None,
                    })
                    .collect(),
                intermediate_responses: Vec::new(),
            }),
        }],
        session_input: Default::default(),
        tags: vec!["harness".to_string(), "spec-3.1".to_string()],
        metadata: None,
    }
}

/// The three scenarios of spec 3.1 §7.13.
pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            case: case(
                "fix_failing_test",
                "A failing check passes after the harness fixes the file it reads",
                "The check `sh test.sh` fails. Read test.sh and config.txt, fix config.txt so the \
                 check passes, then run `sh test.sh` to confirm. Do not change test.sh.",
                &["bash", "write_file", "bash"],
            ),
            mode: HarnessMode::Auto,
            files: &[
                ("test.sh", "#!/bin/sh\ngrep -q '^answer=42$' config.txt\n"),
                ("config.txt", "answer=41\n"),
            ],
            until: Some("sh test.sh"),
            max_iterations: 1,
            unchanged: false,
            scripted: || {
                vec![
                    call("bash", json!({ "command": "sh test.sh" })),
                    call("read_file", json!({ "path": "test.sh" })),
                    call("read_file", json!({ "path": "config.txt" })),
                    call("write_file", json!({ "path": "config.txt", "content": "answer=42\n" })),
                    call("bash", json!({ "command": "sh test.sh" })),
                    ScriptedResponse::text("config.txt now sets answer=42 and the check passes."),
                ]
            },
        },
        Scenario {
            case: case(
                "plan_mode_writes_nothing",
                "In Plan mode the harness investigates and plans; nothing in the workspace changes",
                "Add a function `double(x: i32) -> i32` to src/lib.rs and run the tests.",
                &["read_file"],
            ),
            mode: HarnessMode::Plan,
            files: &[("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n")],
            until: None,
            max_iterations: 1,
            unchanged: true,
            scripted: || {
                vec![
                    call("read_file", json!({ "path": "src/lib.rs" })),
                    call(
                        "write_file",
                        json!({ "path": "src/lib.rs", "content": "pub fn double(x: i32) -> i32 { x * 2 }\n" }),
                    ),
                    call("bash", json!({ "command": "cargo test" })),
                    ScriptedResponse::text("Plan: add `double` below `add`, then run cargo test."),
                ]
            },
        },
        Scenario {
            case: case(
                "goal_until",
                "The prompt repeats until a check passes, within an iteration cap",
                "counter.txt holds a number. Increase it by exactly one, writing the new number \
                 back to counter.txt. Do only that.",
                &["write_file", "write_file"],
            ),
            mode: HarnessMode::Auto,
            files: &[("counter.txt", "0\n")],
            until: Some("test \"$(tr -d '[:space:]' < counter.txt)\" -ge 2"),
            max_iterations: 4,
            unchanged: false,
            scripted: || {
                vec![
                    call("read_file", json!({ "path": "counter.txt" })),
                    call("write_file", json!({ "path": "counter.txt", "content": "1\n" })),
                    ScriptedResponse::text("counter.txt is now 1."),
                    call("write_file", json!({ "path": "counter.txt", "content": "2\n" })),
                    ScriptedResponse::text("counter.txt is now 2."),
                ]
            },
        },
    ]
}

/// What one scenario run produced.
#[derive(Debug)]
pub struct Outcome {
    /// The case id.
    pub id: String,
    /// Whether the pass condition held.
    pub passed: bool,
    /// Turns sent.
    pub iterations: u32,
    /// Every tool call, in order.
    pub tool_calls: Vec<(String, Value)>,
}

fn tree_digest(root: &Path) -> String {
    fn walk(dir: &Path, root: &Path, files: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, files);
            } else if let Ok(bytes) = std::fs::read(&path) {
                files.push((path.strip_prefix(root).unwrap_or(&path).display().to_string(), bytes));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    format!("{files:?}")
}

fn check(root: &Path, command: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .status()
        .is_ok_and(|status| status.success())
}

/// Runs `scenario` on `model` in a fresh workspace.
///
/// # Panics
///
/// When the workspace or the harness cannot be set up.
pub async fn run(scenario: &Scenario, model: Arc<dyn Llm>) -> Outcome {
    let dir = tempfile::tempdir().expect("workspace");
    for (path, content) in scenario.files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, content).expect("file");
    }
    let before = tree_digest(dir.path());
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..InstructionsConfig::default()
        })
        .mode(scenario.mode)
        .allow_host_shell_in_auto()
        .without_compaction()
        .max_iterations(20)
        .build()
        .expect("harness")
        .into_harness("harness-eval", Arc::new(InMemorySessionService::new()))
        .expect("runner");
    let user = UserId::new("eval").expect("user");
    let session: SessionId = harness.open_session(&user, None).await.expect("session");
    let prompt = scenario.case.conversation[0].user_content.get_text();
    let mut tool_calls = Vec::new();
    let mut iterations = 0;
    let mut passed = false;
    while iterations < scenario.max_iterations {
        iterations += 1;
        let events = match harness.send(&user, &session, prompt.clone()).await {
            Ok(events) => events.collect::<Vec<_>>().await,
            Err(_) => break,
        };
        // A streamed call also appears in the partial events before its final event.
        for event in events.into_iter().flatten().filter(|event| !event.llm_response.partial) {
            let parts = event
                .llm_response
                .content
                .map(|content: Content| content.parts)
                .unwrap_or_default();
            for part in parts {
                if let Part::FunctionCall { name, args, .. } = part {
                    tool_calls.push((name, args));
                }
            }
        }
        passed = match scenario.until {
            Some(command) => check(dir.path(), command),
            None => true,
        };
        if passed {
            break;
        }
    }
    if scenario.unchanged {
        passed = passed && tree_digest(dir.path()) == before;
    }
    harness.close().await;
    Outcome { id: scenario.case.eval_id.clone(), passed, iterations, tool_calls }
}

/// The metrics of `outcomes`, keyed metric → case → score, as a `BaselineStore` keeps them:
/// `passed` (1 or 0), `trajectory` (the `ToolTrajectoryScorer` score against the case's
/// expected tools) and `efficiency` (the `TraceAnalyzer` share of useful calls).
pub fn metrics(
    scenarios: &[Scenario],
    outcomes: &[Outcome],
) -> HashMap<String, HashMap<String, f64>> {
    let mut metrics: HashMap<String, HashMap<String, f64>> = HashMap::new();
    let scorer = ToolTrajectoryScorer::new();
    let analyzer = TraceAnalyzer::new();
    for (scenario, outcome) in scenarios.iter().zip(outcomes) {
        let expected = scenario.case.conversation[0]
            .intermediate_data
            .as_ref()
            .map(|data| data.tool_uses.clone())
            .unwrap_or_default();
        let actual: Vec<ToolUse> = outcome
            .tool_calls
            .iter()
            .map(|(name, args)| ToolUse {
                name: name.clone(),
                args: args.clone(),
                expected_response: None,
            })
            .collect();
        let records: Vec<ToolCallRecord> = outcome
            .tool_calls
            .iter()
            .map(|(name, args)| ToolCallRecord { name: name.clone(), args: args.clone() })
            .collect();
        let mut put = |metric: &str, score: f64| {
            metrics.entry(metric.to_string()).or_default().insert(outcome.id.clone(), score);
        };
        put("passed", if outcome.passed { 1.0 } else { 0.0 });
        put("trajectory", scorer.score(&expected, &actual));
        put("efficiency", analyzer.analyze_tool_calls(&records).efficiency_score);
    }
    metrics
}

/// The scripted model of `scenario`.
pub fn scripted_model(scenario: &Scenario) -> Arc<dyn Llm> {
    Arc::new(adk_model::ScriptedLlm::new("scripted", (scenario.scripted)()))
}

/// A question about the harness itself, asked in Ask mode of a harness whose surface is
/// `adk-rust`'s (spec 3.1 §7.25, `capability_questions`).
pub struct CapabilityQuestion {
    /// The case id, in the `passed` metric.
    pub id: &'static str,
    /// What the person types.
    pub prompt: &'static str,
    /// Groups of alternatives: the answer names one of each group, ignoring case.
    pub expected: &'static [&'static [&'static str]],
    /// What a scripted model answers.
    pub scripted: &'static str,
}

/// The three capability questions of spec 3.1 §7.25: a mistyped slash command, resuming a
/// conversation, and a standing denial for a directory.
pub fn capability_questions() -> Vec<CapabilityQuestion> {
    vec![
        CapabilityQuestion {
            id: "capability_mistyped_command",
            prompt: "/aprovals",
            expected: &[&["/approvals"]],
            scripted: "There is no /aprovals command; you probably meant /approvals, which lists \
                       the calls in this session waiting for your decision.",
        },
        CapabilityQuestion {
            id: "capability_resume",
            prompt: "Can I stop now and pick this conversation up again later from the terminal?",
            expected: &[&["--continue", "--session"]],
            scripted: "Yes. Run `adk-rust --continue` in this workspace to continue the most \
                       recent session, or `adk-rust --session ID` for a specific one.",
        },
        CapabilityQuestion {
            id: "capability_deny_rule",
            prompt: "Stop it from ever editing anything under migrations/. How do I set that up?",
            expected: &[&["rules add"], &["edit_file"], &["--deny"]],
            scripted: "Add a standing denial: `adk-rust rules add edit_file --path-within \
                       migrations --deny \"schema changes go through review\"`; do the same for \
                       write_file.",
        },
    ]
}

/// The `adk-rust` commands the capability questions are about, as the CLI declares them.
pub fn capability_surface() -> SurfaceManifest {
    SurfaceManifest::new("adk-rust", env!("CARGO_PKG_VERSION"))
        .command(CommandEntry::slash("approvals", "Calls in this session waiting for a decision"))
        .command(CommandEntry::slash("rules", "List your standing rules; /rules rm N revokes one"))
        .command(CommandEntry::slash("mode", "Show or set the mode: /mode read-only|plan|ask|auto"))
        .command(CommandEntry::subcommand(
            "rules",
            "Standing rules: approvals and denials remembered across sessions",
            Some(
                "Usage: adk-rust rules add <TOOL> [--prefix <PREFIX>] [--path-within \
                 <PATH_WITHIN>] [--deny <DENY>] [--session]\n\n  --prefix  For bash: the \
                 command's leading words, such as \"cargo test\"\n  --path-within  For \
                 write_file and edit_file: the directory the path must be within\n  --deny  \
                 Refuse matching calls with this reason instead of allowing them"
                    .to_string(),
            ),
        ))
        .command(CommandEntry::flag(
            "--continue",
            "Continue the most recent session in this workspace",
        ))
        .command(CommandEntry::flag(
            "--session",
            "Continue the session with this id, creating it when it does not exist",
        ))
        .command(CommandEntry::flag(
            "rules add --deny",
            "Refuse matching calls with this reason instead of allowing them",
        ))
        .command(CommandEntry::flag(
            "rules add --path-within",
            "For write_file and edit_file: the directory the path must be within",
        ))
}

/// Whether `answer` names one alternative of every group of `question`.
pub fn capability_passes(question: &CapabilityQuestion, answer: &str) -> bool {
    let answer = answer.to_lowercase();
    question
        .expected
        .iter()
        .all(|group| group.iter().any(|alternative| answer.contains(&alternative.to_lowercase())))
}

/// Asks `question` on `model` in a fresh workspace, in Ask mode; the outcome passes when the
/// answer names the expected commands and nothing in the workspace changed.
///
/// # Panics
///
/// When the workspace or the harness cannot be set up.
pub async fn ask(question: &CapabilityQuestion, model: Arc<dyn Llm>) -> Outcome {
    let dir = tempfile::tempdir().expect("workspace");
    let before = tree_digest(dir.path());
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..InstructionsConfig::default()
        })
        .mode(HarnessMode::Ask)
        .surface(capability_surface())
        .without_compaction()
        .max_iterations(8)
        .build()
        .expect("harness")
        .into_harness("harness-eval", Arc::new(InMemorySessionService::new()))
        .expect("runner");
    let user = UserId::new("eval").expect("user");
    let session: SessionId = harness.open_session(&user, None).await.expect("session");
    let mut tool_calls = Vec::new();
    let mut answer = String::new();
    if let Ok(events) = harness.send(&user, &session, question.prompt.to_string()).await {
        let events = events.collect::<Vec<_>>().await;
        for event in events.into_iter().flatten().filter(|event| !event.llm_response.partial) {
            let parts = event
                .llm_response
                .content
                .map(|content: Content| content.parts)
                .unwrap_or_default();
            for part in parts {
                match part {
                    Part::FunctionCall { name, args, .. } => tool_calls.push((name, args)),
                    Part::Text { text } => {
                        answer.push_str(&text);
                        answer.push('\n');
                    }
                    _ => {}
                }
            }
        }
    }
    harness.close().await;
    let passed = capability_passes(question, &answer) && tree_digest(dir.path()) == before;
    Outcome { id: question.id.to_string(), passed, iterations: 1, tool_calls }
}

/// The scripted model of `question`.
pub fn capability_model(question: &CapabilityQuestion) -> Arc<dyn Llm> {
    Arc::new(adk_model::ScriptedLlm::new(
        "scripted",
        vec![ScriptedResponse::text(question.scripted)],
    ))
}
