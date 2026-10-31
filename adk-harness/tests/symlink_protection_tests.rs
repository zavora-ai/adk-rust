//! Protected paths are decided on the target a write reaches, not on the path as written
//! (spec 3.1 §9.2 I11, I29; evaluation finding F-2, probe P-6).

#![cfg(all(feature = "runner", unix))]

use crate::common;

use std::os::unix::fs::symlink;

use adk_harness::{HarnessMode, PromptAnswer, RulePredicate};
use adk_model::ScriptedResponse;
use common::{Outcome, fixture, outcome, responses};
use serde_json::{Value, json};

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

fn write(path: &str, content: &str) -> (&'static str, Value) {
    ("write_file", json!({ "path": path, "content": content }))
}

/// `AGENTS.md`, `.git/config`, and links into them from ordinary-looking paths.
fn plant(root: &std::path::Path) {
    std::fs::write(root.join("AGENTS.md"), "rules\n").unwrap();
    std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
    std::fs::write(root.join(".git/config"), "[core]\n").unwrap();
    std::fs::create_dir(root.join("docs")).unwrap();
    symlink("../AGENTS.md", root.join("docs/notes.md")).unwrap();
    symlink(".git/config", root.join("settings.ini")).unwrap();
    symlink(".git", root.join("cfg")).unwrap();
}

/// The outcome of each call in `events`, in order.
fn outcomes(events: &[adk_core::Event]) -> Vec<Outcome> {
    responses(events).iter().map(|(_, response)| outcome(response)).collect()
}

#[tokio::test]
async fn auto_holds_writes_that_reach_protected_files_through_symlinks() {
    let batch = ScriptedResponse::function_calls([
        write("AGENTS.md", "pwned\n"),
        write("docs/notes.md", "pwned via symlink\n"),
        write("settings.ini", "[core]\n\tpwned = true\n"),
        write("cfg/hooks/pre-commit", "#!/bin/sh\n"),
        write("docs/plain.md", "fine\n"),
    ]);
    let f = fixture(vec![batch, done()], |b| b.mode(HarnessMode::Auto)).await;
    let root = std::fs::canonicalize(f.dir.path()).unwrap();
    plant(&root);

    let events = f.send("edit").await;
    assert_eq!(
        outcomes(&events),
        [Outcome::Held, Outcome::Held, Outcome::Held, Outcome::Held, Outcome::Executed],
        "{:#?}",
        responses(&events)
    );
    assert_eq!(std::fs::read_to_string(root.join("AGENTS.md")).unwrap(), "rules\n");
    assert_eq!(std::fs::read_to_string(root.join(".git/config")).unwrap(), "[core]\n");
    assert!(!root.join(".git/hooks/pre-commit").exists());
    assert_eq!(std::fs::read_to_string(root.join("docs/plain.md")).unwrap(), "fine\n");
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    assert_eq!(pending.len(), 4);
}

#[tokio::test]
async fn a_workspace_rule_never_covers_a_symlink_to_a_protected_file() {
    let prompter = common::ScriptedPrompter::new();
    prompter.then(|prompt| {
        let anywhere = prompt
            .suggestions
            .iter()
            .find(|suggestion| {
                suggestion.predicates.iter().any(|predicate| {
                    matches!(predicate, RulePredicate::PathWithin { dir, .. } if dir == ".")
                })
            })
            .expect("an edit offers a workspace rule")
            .clone();
        PromptAnswer::ApproveAndRemember(anywhere)
    });
    prompter.deny();
    let script = vec![
        ScriptedResponse::function_calls([write("docs/first.md", "one\n")]),
        done(),
        ScriptedResponse::function_calls([write("docs/second.md", "two\n")]),
        done(),
        ScriptedResponse::function_calls([write("docs/notes.md", "pwned via rule\n")]),
        done(),
    ];
    let f = fixture(script, |b| b.prompter(prompter.clone())).await;
    let root = std::fs::canonicalize(f.dir.path()).unwrap();
    plant(&root);

    f.send("first").await;
    assert_eq!(prompter.count(), 1);
    let events = f.send("second").await;
    assert_eq!(outcomes(&events), [Outcome::Executed]);
    assert_eq!(prompter.count(), 1, "the session rule covers an ordinary file");

    let events = f.send("through the link").await;
    assert_eq!(outcomes(&events), [Outcome::Denied], "{:#?}", responses(&events));
    assert_eq!(prompter.count(), 2, "the protected target is asked about, never covered");
    let prompts = prompter.prompts.lock().unwrap();
    assert!(prompts[1].suggestions.is_empty(), "nothing is offered to remember");
    assert_eq!(std::fs::read_to_string(root.join("AGENTS.md")).unwrap(), "rules\n");
}

#[tokio::test]
async fn a_link_created_earlier_in_the_session_is_seen_by_the_next_call() {
    let batch = ScriptedResponse::function_calls([
        ("bash", json!({ "command": "ln -s ../AGENTS.md docs/late.md" })),
        write("docs/late.md", "pwned via a fresh link\n"),
    ]);
    let f = fixture(vec![batch, done()], |b| b.mode(HarnessMode::Auto).allow_host_shell_in_auto())
        .await;
    let root = std::fs::canonicalize(f.dir.path()).unwrap();
    plant(&root);

    let events = f.send("link and write").await;
    assert_eq!(outcomes(&events), [Outcome::Executed, Outcome::Held], "{:#?}", responses(&events));
    assert!(std::fs::symlink_metadata(root.join("docs/late.md")).unwrap().is_symlink());
    assert_eq!(std::fs::read_to_string(root.join("AGENTS.md")).unwrap(), "rules\n");
}

#[tokio::test]
async fn a_dangling_link_is_refused_in_every_mode() {
    for mode in [HarnessMode::Ask, HarnessMode::Auto] {
        let batch = ScriptedResponse::function_calls([write("docs/dangling.md", "created\n")]);
        let f = fixture(vec![batch, done()], |b| b.mode(mode)).await;
        let root = std::fs::canonicalize(f.dir.path()).unwrap();
        std::fs::create_dir(root.join("docs")).unwrap();
        symlink("../CLAUDE.md", root.join("docs/dangling.md")).unwrap();

        let events = f.send("write").await;
        assert_eq!(outcomes(&events), [Outcome::Denied], "{mode}: {:#?}", responses(&events));
        assert!(!root.join("CLAUDE.md").exists(), "{mode}");
    }
}
