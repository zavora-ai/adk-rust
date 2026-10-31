//! "Always allow for this folder" covers every file write inside the folder (spec 3.3 §8.2):
//! after a person answers a `write_file` in `notes/` with it, the next `write_file`, an
//! `edit_file` and a write in a subfolder run without asking, while a write outside it, a `..`
//! or symlink escape, a delete and the shell do not.

#![cfg(feature = "web")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditRecordDraft;
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_surface::{Answer, Command, CommandOutcome, DecisionCard, ThreadItem};
use common::*;
use serde_json::json;

const WITHIN: Duration = Duration::from_secs(15);

/// The calls the model makes, in order; a call that is asked about is denied, so the next one
/// comes.
fn script() -> Arc<ScriptedLlm> {
    let call = ScriptedResponse::function_call;
    let mut calls = vec![
        call("write_file", json!({ "path": "notes/a.md", "content": "draft" })),
        call("write_file", json!({ "path": "notes/b.md", "content": "second" })),
        call("read_file", json!({ "path": "notes/a.md" })),
        call(
            "edit_file",
            json!({ "path": "notes/a.md", "old_string": "draft", "new_string": "final" }),
        ),
        call("write_file", json!({ "path": "notes/deep/c.md", "content": "nested" })),
        call("write_file", json!({ "path": "elsewhere.md", "content": "outside" })),
        call("write_file", json!({ "path": "notes/../escape.md", "content": "escape" })),
    ];
    // `notes/out` is a symlink out of the workspace where the test can make one.
    if cfg!(unix) {
        calls.push(call("write_file", json!({ "path": "notes/out/link.md", "content": "x" })));
    }
    calls.extend([
        call(
            "apply_patch",
            json!({ "patch": "*** Begin Patch\n*** Delete File: notes/b.md\n*** End Patch" }),
        ),
        call("bash", json!({ "command": "ls notes" })),
        ScriptedResponse::text("Done."),
    ]);
    Arc::new(ScriptedLlm::new("scripted", calls))
}

/// A card waiting for the person that is not in `answered`, or `None` once the turn has
/// ended.
async fn next_card(s: &Scenario, answered: &[String]) -> Option<DecisionCard> {
    eventually("the next card or the end of the turn", WITHIN, || async {
        let card = s.engine.decisions(&web()).await.unwrap().into_iter().find(|card| {
            !answered.contains(&card.id) && card.allowed.option(Answer::AllowOnce).is_some()
        });
        if card.is_some() {
            return Some(card);
        }
        let thread = s.engine.thread(&web(), "n1").await.unwrap();
        let done = thread.iter().flat_map(|entry| &entry.items).any(
            |item| matches!(item, ThreadItem::Message { text, .. } if text.as_str() == "Done."),
        );
        done.then_some(None)
    })
    .await
}

async fn answer(s: &Scenario, card: &DecisionCard, answer: Answer) {
    let outcome = s
        .engine
        .execute(
            &web(),
            Command::Decide {
                decision_id: card.id.clone(),
                version: card.version,
                answer,
                edited_args: None,
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
}

fn records(s: &Scenario, action: &str) -> Vec<AuditRecordDraft> {
    s.audit.records().into_iter().filter(|record| record.action == action).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_allow_for_this_folder_covers_every_file_write_in_it() {
    let s = scenario(script()).await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(s.path.join("notes")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), s.path.join("notes/out")).unwrap();

    s.engine
        .execute(&web(), Command::Send { session_id: "n1".into(), text: "take notes".into() })
        .await
        .unwrap();
    let first = next_card(&s, &[]).await.expect("the first write asks");
    assert_eq!(first.heading, "Write file notes/a.md");
    let always = first.allowed.option(Answer::AlwaysAllow).expect("always allow is offered");
    assert_eq!(
        always.label,
        "Always allow for this folder (file writes anywhere in notes/, this session)"
    );
    answer(&s, &first, Answer::AlwaysAllow).await;

    // Every call after it that asks is denied, so the script runs to its end.
    let mut answered = vec![first.id.clone()];
    let mut asked = Vec::new();
    while let Some(card) = next_card(&s, &answered).await {
        answered.push(card.id.clone());
        // Whether the shell asks depends on the host's sandbox; it is never the rule's.
        if card.call.tool != "bash" {
            asked.push(card.heading.clone());
        }
        answer(&s, &card, Answer::Deny).await;
    }
    // A write outside the folder, a `..` escape and a delete in it ask; the symlink escape
    // is refused outright, as a path outside the workspace.
    assert_eq!(asked, ["Write file elsewhere.md", "Write file notes/../escape.md", "Apply patch"]);

    // The rule let through the new file, the edit and the subfolder's file, and nothing else.
    let matched: Vec<String> = records(&s, "harness.rule.matched")
        .iter()
        .map(|record| record.ext["tool"].as_str().unwrap_or_default().to_string())
        .collect();
    // The approved call may be checked again as it resumes, under the rule just made.
    let covered = if matched.len() == 4 { &matched[1..] } else { &matched[..] };
    assert_eq!(covered, ["write_file", "edit_file", "write_file"], "{matched:?}");
    let read = |path: &str| std::fs::read_to_string(s.path.join(path)).ok();
    assert_eq!(read("notes/a.md").as_deref(), Some("final"));
    assert_eq!(read("notes/b.md").as_deref(), Some("second"));
    assert_eq!(read("notes/deep/c.md").as_deref(), Some("nested"));
    assert_eq!(read("elsewhere.md"), None);
    assert_eq!(read("escape.md"), None);
    assert!(!outside.path().join("link.md").exists());

    // One rule, on record with the tools it covers.
    let created = records(&s, "harness.rule.created");
    assert_eq!(created.len(), 1, "{created:?}");
    assert_eq!(created[0].ext["tools"], json!(["apply_patch", "edit_file", "write_file"]));
    // What a person reads of the record: what the rule covers, in plain words.
    assert_eq!(created[0].ext["covers"], "file writes in notes/");
    assert_eq!(created[0].ext["surface"], "web");
    s.kernel.stop().await.unwrap();
}
