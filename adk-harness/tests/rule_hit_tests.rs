//! A call a standing rule allows is announced, not run silently (spec 3.1 §7.33 fix 3).

#![cfg(feature = "runner")]

use crate::common;

use adk_harness::{HarnessAgentBuilder, PromptAnswer, RuleHit, RulePredicate};
use adk_model::ScriptedResponse;
use common::{ScriptedPrompter, fixture};
use serde_json::json;

/// "Always allow" with the one-word `printf` prefix the prompt offers.
fn remember_printf(prompter: &ScriptedPrompter) {
    prompter.then(|prompt| {
        let suggestion = prompt
            .suggestions
            .iter()
            .find(|suggestion| {
                matches!(suggestion.predicates.first(),
                    Some(RulePredicate::ShellPrefix { tokens, .. }) if tokens.len() == 1)
            })
            .expect("a one-word prefix suggestion")
            .clone();
        PromptAnswer::ApproveAndRemember(suggestion)
    });
}

#[tokio::test]
async fn a_rule_granted_in_a_batch_is_announced_for_the_next_call_of_the_batch() {
    let prompter = ScriptedPrompter::new();
    remember_printf(&prompter);
    let f = {
        let prompter = prompter.clone();
        fixture(
            vec![
                ScriptedResponse::function_calls([
                    ("bash", json!({ "command": "printf a" })),
                    ("bash", json!({ "command": "printf b" })),
                ]),
                ScriptedResponse::text("done"),
            ],
            move |b: HarnessAgentBuilder| b.prompter(prompter),
        )
        .await
    };
    f.send("print a and b").await;
    assert_eq!(prompter.count(), 1, "the second call is covered by the rule");
    let hits = prompter.rule_hits.lock().unwrap().clone();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].number, Some(1));
    assert_eq!(hits[0].tool, "bash");
    assert!(hits[0].to_string().starts_with("[allowed by rule 1] bash: printf"), "{}", hits[0]);
}

#[tokio::test]
async fn a_call_a_rule_already_allows_is_announced_when_it_is_evaluated() {
    let prompter = ScriptedPrompter::new();
    remember_printf(&prompter);
    let f = {
        let prompter = prompter.clone();
        fixture(
            vec![
                ScriptedResponse::function_call("bash", json!({ "command": "printf a" })),
                ScriptedResponse::text("done"),
                ScriptedResponse::function_call("bash", json!({ "command": "printf c" })),
                ScriptedResponse::text("done"),
            ],
            move |b: HarnessAgentBuilder| b.prompter(prompter),
        )
        .await
    };
    f.send("print a").await;
    assert!(prompter.rule_hits.lock().unwrap().is_empty(), "the first call was asked about");
    f.send("print c").await;
    assert_eq!(prompter.count(), 1);
    let hits: Vec<RuleHit> = prompter.rule_hits.lock().unwrap().clone();
    assert_eq!(hits.len(), 1, "announced once, by the policy: {hits:?}");
    assert_eq!(hits[0].number, Some(1));
}
