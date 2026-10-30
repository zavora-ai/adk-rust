//! The harness describes itself to the model (spec 3.1 §7.25): the `<harness>` block in
//! the system instruction, the standing-rules digest in the reminder and `harness_help`.

#![cfg(feature = "runner")]

use crate::common;

use adk_core::{Part, SubjectKind, SubjectRef, ToolOrigin};
use adk_harness::{
    CommandEntry, HarnessAgentBuilder, ModeDecision, OriginPin, RuleScope, StandingDecision,
    StandingRule, SurfaceManifest,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use common::{fixture, last_system_text};
use serde_json::{Value, json};

fn surface() -> SurfaceManifest {
    SurfaceManifest::new("adk-rust", "3.0.0")
        .command(CommandEntry::slash("approvals", "Calls in this session waiting for a decision"))
        .command(CommandEntry::slash("rules", "List your standing rules; /rules rm N revokes one"))
        .command(CommandEntry::subcommand(
            "up",
            "Run the harness as the agent kernel",
            Some("Usage: adk-rust up [--install]".into()),
        ))
        .command(CommandEntry::flag("--continue", "Continue the most recent session"))
        .pack_switch("browser", "--pack browser")
}

fn harness_block(system: &str) -> &str {
    let start = system.find("<harness version=").expect("a <harness> block");
    let end = system[start..].find("</harness>").expect("a closed block") + start;
    &system[start..end + "</harness>".len()]
}

/// The text of every reminder in `model`'s last request.
fn last_reminder(model: &ScriptedLlm) -> String {
    let request = model.last_request().expect("a request");
    request
        .contents
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::Text { text } if text.starts_with("<system-reminder>") => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn the_system_instruction_carries_the_harness_block_after_the_environment() {
    let f =
        fixture(vec![ScriptedResponse::text("ok")], |b: HarnessAgentBuilder| b.surface(surface()))
            .await;
    f.send("hello").await;
    let system = last_system_text(&f.model);
    let block = harness_block(&system);
    assert!(system.find("</environment>").unwrap() < system.find("<harness version=").unwrap());
    assert!(block.contains("surface=\"adk-rust 3.0.0\""), "{block}");
    assert!(
        block.contains("  edit (apply_patch edit_file write_file): deny | deny | ask | allow"),
        "the edit class line says what each mode does: {block}"
    );
    assert!(block.contains("ask every time"), "{block}");
    assert!(block.contains("/approvals  Calls in this session waiting for a decision"), "{block}");
    assert!(block.contains("adk-rust up  Run the harness as the agent kernel"), "{block}");
    let browser = if cfg!(feature = "browser") {
        "browser (off: --pack browser)"
    } else {
        "browser (not compiled)"
    };
    assert!(block.contains(browser), "{block}");
    assert!(block.contains("no file is restored"), "the block never promises an undo: {block}");
    assert!(block.len() <= adk_harness::HARNESS_BLOCK_MAX_BYTES);
}

#[tokio::test]
async fn the_block_is_byte_identical_across_builds_and_turns() {
    let one = fixture(
        vec![ScriptedResponse::text("one"), ScriptedResponse::text("two")],
        |b: HarnessAgentBuilder| b.surface(surface()),
    )
    .await;
    let two =
        fixture(vec![ScriptedResponse::text("one")], |b: HarnessAgentBuilder| b.surface(surface()))
            .await;
    one.send("first").await;
    let first = harness_block(&last_system_text(&one.model)).to_string();
    one.send("second").await;
    let second = harness_block(&last_system_text(&one.model)).to_string();
    two.send("first").await;
    let other = harness_block(&last_system_text(&two.model)).to_string();
    assert_eq!(first, second, "the block is the same on every turn");
    assert_eq!(first, other, "two builds with the same configuration render the same block");
    assert_eq!(first, one.harness.agent().manifest().render());
}

#[tokio::test]
async fn every_declared_tool_appears_in_a_class() {
    let f = fixture(vec![], |b: HarnessAgentBuilder| b).await;
    let agent = f.harness.agent();
    let manifest = agent.manifest();
    for tool in &agent.info().tools {
        assert!(
            manifest.classes.iter().any(|class| class.tools.contains(tool)),
            "{tool} is in no class of {:#?}",
            manifest.classes
        );
    }
}

#[tokio::test]
async fn a_switch_changes_the_class_line_it_resolves() {
    let f = fixture(vec![], |b: HarnessAgentBuilder| b.allow_host_shell_in_auto()).await;
    let manifest = f.harness.agent().manifest().clone();
    let shell = manifest.classes.iter().find(|class| class.class == "shell (host)").unwrap();
    assert_eq!(
        shell.decisions,
        [ModeDecision::Deny, ModeDecision::Deny, ModeDecision::Ask, ModeDecision::Allow]
    );
}

#[tokio::test]
async fn harness_help_answers_commands_cli_pages_and_the_manifest() {
    let calls = ["commands", "cli up", "modes-and-approvals", "manifest", "nope"];
    let mut responses: Vec<ScriptedResponse> = calls
        .iter()
        .map(|topic| ScriptedResponse::function_call("harness_help", json!({ "topic": topic })))
        .collect();
    responses.push(ScriptedResponse::text("done"));
    let f = fixture(responses, |b: HarnessAgentBuilder| {
        b.surface(surface()).help_page("modes-and-approvals", "Modes decide what runs.")
    })
    .await;
    let events = f.send("what can you do?").await;
    let answers: Vec<Value> = common::responses(&events)
        .into_iter()
        .filter(|(tool, _)| tool == "harness_help")
        .map(|(_, response)| response)
        .collect();
    assert_eq!(answers.len(), calls.len());
    let text = |value: &Value| serde_json::to_string(value).unwrap();
    assert!(text(&answers[0]).contains("/rules"), "{}", text(&answers[0]));
    assert!(text(&answers[1]).contains("Usage: adk-rust up [--install]"));
    assert!(text(&answers[2]).contains("Modes decide what runs."));
    assert!(text(&answers[3]).contains("harnessVersion"));
    assert!(text(&answers[4]).contains("no help topic 'nope'"), "{}", text(&answers[4]));
}

async fn declared_origin(f: &common::Fixture, name: &str) -> ToolOrigin {
    let tools = f.harness.agent().runtime().toolset().all_tools();
    let tool = tools.into_iter().find(|tool| tool.name() == name).unwrap();
    ToolOrigin::of(tool.as_ref())
}

fn rule(
    id: &str,
    tool: &str,
    origin: &ToolOrigin,
    decision: StandingDecision,
    label: &str,
    minute: u32,
) -> StandingRule {
    StandingRule {
        id: id.into(),
        scope: RuleScope::User {
            app_name: common::APP.into(),
            user_id: "alice".into(),
            subject_kind: SubjectKind::Human,
        },
        tool: tool.into(),
        origin: OriginPin::exact(origin),
        agent: None,
        predicates: vec![],
        decision,
        created_by: SubjectRef::human("alice"),
        created_at: chrono::DateTime::parse_from_rfc3339(&format!("2026-10-25T08:{minute:02}:00Z"))
            .unwrap()
            .into(),
        expires_at: None,
        label: label.into(),
        source_fingerprint: None,
    }
}

#[tokio::test]
async fn the_reminder_lists_the_rules_that_apply_deny_first_numbered_as_rules_lists_them() {
    let f = fixture(vec![ScriptedResponse::text("ok")], |b: HarnessAgentBuilder| b).await;
    let bash = declared_origin(&f, "bash").await;
    let edit = declared_origin(&f, "edit_file").await;
    let other = ToolOrigin::new("mcp:elsewhere", &json!({ "name": "bash" }), None);
    let store = f.harness.standing_rules().clone();
    store
        .grant(rule(
            "a",
            "bash",
            &bash,
            StandingDecision::Allow,
            "bash: commands starting \"cargo test\"",
            1,
        ))
        .await
        .unwrap();
    store
        .grant(rule(
            "b",
            "edit_file",
            &edit,
            StandingDecision::Deny { reason: "schema changes go through review".into() },
            "edit_file: within migrations/",
            2,
        ))
        .await
        .unwrap();
    store
        .grant(rule("c", "bash", &other, StandingDecision::Allow, "another server's bash", 3))
        .await
        .unwrap();
    f.send("hello").await;
    let reminder = last_reminder(&f.model);
    let deny = reminder.find("- [2] deny edit_file: within migrations/ (every session): \"schema changes go through review\"");
    let allow = reminder.find("- [1] allow bash: commands starting \"cargo test\" (every session)");
    assert!(deny.is_some() && allow.is_some(), "{reminder}");
    assert!(deny < allow, "deny comes before allow: {reminder}");
    assert!(
        !reminder.contains("another server's bash"),
        "a rule pinned to another declaration is left out"
    );
}

#[tokio::test]
async fn the_digest_is_capped_and_redacts_secrets() {
    let f = fixture(vec![ScriptedResponse::text("ok")], |b: HarnessAgentBuilder| b).await;
    let bash = declared_origin(&f, "bash").await;
    let store = f.harness.standing_rules().clone();
    for n in 0..25u32 {
        store
            .grant(rule(
                &format!("r{n}"),
                "bash",
                &bash,
                StandingDecision::Allow,
                &format!("bash: commands starting \"make t{n}\""),
                n,
            ))
            .await
            .unwrap();
    }
    store
        .grant(rule(
            "secret",
            "bash",
            &bash,
            StandingDecision::Deny { reason: "never".into() },
            "bash: commands starting \"curl -H token=ghp_0123456789abcdefghijABCDEFGHIJ0123\"",
            30,
        ))
        .await
        .unwrap();
    f.send("hello").await;
    let reminder = last_reminder(&f.model);
    assert!(reminder.contains(" more; /rules lists them)"), "{reminder}");
    let listed = reminder.lines().filter(|line| line.starts_with("- [")).count();
    assert!(listed <= 20, "at most 20 rules: {reminder}");
    let bytes: usize = reminder.lines().filter(|line| line.starts_with("- [")).map(str::len).sum();
    assert!(bytes <= 1024, "at most 1 KiB of rule lines: {reminder}");
    assert!(!reminder.contains("ghp_0123456789"), "the token is redacted: {reminder}");
    assert!(reminder.contains("- [26] deny"), "deny comes first: {reminder}");
}

#[tokio::test]
async fn no_rule_means_no_digest() {
    let f = fixture(vec![ScriptedResponse::text("ok")], |b: HarnessAgentBuilder| b).await;
    f.send("hello").await;
    assert!(!last_reminder(&f.model).contains("Standing rules"));
}
