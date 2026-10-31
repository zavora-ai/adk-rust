//! `apply_patch` under the harness policy (spec 3.1 §7.26, §9.2 I67): class `Edit`, every
//! header path read, a protected path anywhere non-waivable, and a `PathWithin` rule that
//! matches only when every path is within its directory.

#![cfg(feature = "runner")]

use crate::common;

use adk_harness::{HarnessMode, PromptAnswer, RulePredicate};
use adk_model::ScriptedResponse;
use common::{Outcome, fixture, outcome, response_of};
use serde_json::json;

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

fn patch(body: &str) -> ScriptedResponse {
    ScriptedResponse::function_call(
        "apply_patch",
        json!({ "patch": format!("*** Begin Patch\n{body}\n*** End Patch") }),
    )
}

#[tokio::test]
async fn apply_patch_is_an_edit_asked_in_ask_and_run_in_auto() {
    let f = fixture(vec![patch("*** Add File: src/a.rs\n+fn a() {}"), done()], |b| {
        b.mode(HarnessMode::Auto)
    })
    .await;
    let events = f.send("patch").await;
    let response = response_of(&events, "apply_patch");
    assert_eq!(outcome(&response), Outcome::Executed, "{response}");
    assert_eq!(response["files"][0]["op"], "add");
    assert_eq!(std::fs::read_to_string(f.path("src/a.rs")).unwrap(), "fn a() {}\n");

    let f = fixture(vec![patch("*** Add File: src/a.rs\n+fn a() {}"), done()], |b| b).await;
    let events = f.send("patch").await;
    assert_eq!(outcome(&response_of(&events, "apply_patch")), Outcome::Held);
    assert!(!f.path("src/a.rs").exists());
}

#[tokio::test]
async fn a_protected_path_anywhere_in_a_patch_is_never_run_unasked() {
    // Auto runs ordinary edits; one protected header makes the whole call non-waivable.
    let body = "*** Add File: src/a.rs\n+fn a() {}\n*** Add File: AGENTS.md\n+pwned";
    let f = fixture(vec![patch(body), done()], |b| b.mode(HarnessMode::Auto)).await;
    let events = f.send("patch").await;
    assert_eq!(outcome(&response_of(&events, "apply_patch")), Outcome::Held);
    assert!(!f.path("src/a.rs").exists() && !f.path("AGENTS.md").exists());

    // A move out of a protected directory names it as the source.
    let f = fixture(
        vec![patch("*** Update File: .git/config\n*** Move to: config.txt"), done()],
        |b| b.mode(HarnessMode::Auto),
    )
    .await;
    std::fs::create_dir_all(f.path(".git")).unwrap();
    std::fs::write(f.path(".git/config"), "[core]\n").unwrap();
    let events = f.send("patch").await;
    assert_eq!(outcome(&response_of(&events, "apply_patch")), Outcome::Held);
    assert!(f.path(".git/config").exists());
}

#[tokio::test]
async fn a_path_outside_the_workspace_anywhere_in_a_patch_is_denied() {
    let body = "*** Add File: src/a.rs\n+x\n*** Add File: ../outside.txt\n+x";
    let f = fixture(vec![patch(body), done()], |b| b.mode(HarnessMode::Auto)).await;
    let events = f.send("patch").await;
    let response = response_of(&events, "apply_patch");
    assert_eq!(outcome(&response), Outcome::Denied, "{response}");
    assert!(response.to_string().contains("outside the workspace"), "{response}");
}

#[tokio::test]
async fn a_path_within_rule_covers_a_patch_only_when_every_path_is_within() {
    let prompter = common::ScriptedPrompter::new();
    prompter.then(|prompt| {
        let within_src = prompt
            .suggestions
            .iter()
            .find(|suggestion| {
                suggestion.predicates.iter().any(|predicate| {
                    matches!(predicate, RulePredicate::PathWithin { dir, .. } if dir == "src")
                })
            })
            .expect("a patch within src offers a rule for src")
            .clone();
        PromptAnswer::ApproveAndRemember(within_src)
    });
    prompter.deny();
    let script = vec![
        patch("*** Add File: src/a.rs\n+a\n*** Add File: src/nested/b.rs\n+b"),
        done(),
        patch("*** Add File: src/c.rs\n+c"),
        done(),
        patch("*** Add File: src/d.rs\n+d\n*** Add File: docs/e.md\n+e"),
        done(),
    ];
    let f = fixture(script, |b| b.prompter(prompter.clone())).await;

    f.send("first").await;
    assert_eq!(prompter.count(), 1);
    assert!(f.path("src/nested/b.rs").exists());
    let events = f.send("second").await;
    assert_eq!(outcome(&response_of(&events, "apply_patch")), Outcome::Executed);
    assert_eq!(prompter.count(), 1, "the rule covers a patch inside src");

    let events = f.send("third").await;
    assert_eq!(outcome(&response_of(&events, "apply_patch")), Outcome::Denied);
    assert_eq!(prompter.count(), 2, "a patch reaching outside src is asked about");
    assert!(!f.path("src/d.rs").exists() && !f.path("docs/e.md").exists());
}

#[tokio::test]
async fn a_patch_touching_a_protected_path_offers_no_rule() {
    let prompter = common::ScriptedPrompter::new();
    prompter.deny();
    let body = "*** Add File: src/a.rs\n+a\n*** Add File: .claude/settings.json\n+{}";
    let f = fixture(vec![patch(body), done()], |b| b.prompter(prompter.clone())).await;
    f.send("patch").await;
    let prompts = prompter.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].suggestions.is_empty(), "nothing is offered to remember");
}
