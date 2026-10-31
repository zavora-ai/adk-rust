//! A session's "deny all remaining" and prompt cap hold for its whole turn while another
//! session prompts (spec 3.1 §7.5, threat I-3; evaluation finding F-6).

#![cfg(feature = "runner")]

use crate::common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_harness::{ApprovalPrompt, ApprovalPrompter, PromptAnswer};
use adk_model::ScriptedResponse;
use serde_json::json;
use tokio::sync::Notify;

/// Alice's first prompt is answered "deny all remaining" after Bob's first prompt has
/// queued behind it; Bob's prompts are approved.
#[derive(Debug, Default)]
struct TwoUsers {
    alice_prompts: AtomicUsize,
    bob_prompts: AtomicUsize,
    alice_waiting: Notify,
}

#[async_trait::async_trait]
impl ApprovalPrompter for TwoUsers {
    async fn prompt(&self, prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        let user = prompt
            .request
            .acting
            .as_ref()
            .and_then(|acting| acting.on_behalf_of.as_ref())
            .map(|subject| subject.id.clone())
            .unwrap_or_default();
        if user == "bob" {
            self.bob_prompts.fetch_add(1, Ordering::SeqCst);
            return Ok(PromptAnswer::ApproveOnce);
        }
        if self.alice_prompts.fetch_add(1, Ordering::SeqCst) == 0 {
            self.alice_waiting.notify_one();
            // Bob's first call reaches the handler and waits for this prompt to end.
            tokio::time::sleep(Duration::from_millis(300)).await;
            return Ok(PromptAnswer::DenyAllRemaining);
        }
        Ok(PromptAnswer::Deny { message: Some("prompted again".into()) })
    }
}

fn three_commands(prefix: &str) -> ScriptedResponse {
    ScriptedResponse::function_calls(
        (1..=3).map(|n| ("bash", json!({ "command": format!("echo {prefix}{n}") }))),
    )
}

#[tokio::test]
async fn deny_all_remaining_holds_for_the_turn_while_another_session_prompts() {
    let prompter = Arc::new(TwoUsers::default());
    let mut script = vec![three_commands("a"), three_commands("b")];
    script.extend((0..6).map(|_| ScriptedResponse::text("done")));
    let f = common::fixture(script, |b| b.prompter(prompter.clone())).await;
    let bob = common::user("bob");
    let bob_session = f.harness.open_session(&bob, None).await.unwrap();

    let alice_turn = f.send("go");
    let bob_turn = async {
        prompter.alice_waiting.notified().await;
        f.send_as(&bob, &bob_session, "go").await
    };
    let (alice_events, _) = tokio::join!(alice_turn, bob_turn);

    assert_eq!(
        prompter.alice_prompts.load(Ordering::SeqCst),
        1,
        "after deny all remaining, alice's other calls are refused unprompted"
    );
    let refused = common::responses(&alice_events)
        .into_iter()
        .filter(|(_, response)| response.to_string().contains("denied the remaining calls"))
        .count();
    assert_eq!(refused, 3, "{:#?}", common::responses(&alice_events));
    assert_eq!(prompter.bob_prompts.load(Ordering::SeqCst), 3, "bob's own turn is unaffected");
}
