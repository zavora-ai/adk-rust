//! A decision the kernel itself asks is drawn in plain words (spec 3.3 §7.1, §8): an agent's
//! proposed routine reaches the decision box as who suggests it, what it does and when it
//! runs — never as the kernel's tool name or the agenda item's JSON, which stay in the exact
//! call under the details — and its answers say "Do it" and "Not now" (§8.1 "Suggestion").
//!
//! The model is `ScriptedLlm`; the kernel, its agenda, the harness, the approval service and
//! the session store are real.

#![cfg(feature = "web")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::ApprovalState;
use adk_kernel::{AgendaFilter, AgendaStateKind};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_surface::{Answer, Command, CommandOutcome, DecisionCard};
use common::*;
use serde_json::json;

const WITHIN: Duration = Duration::from_secs(15);

/// No kernel tool name and no JSON in what a person reads first.
fn assert_plain(text: &str) {
    for raw in ["kernel", "agenda", "{", "}", "\"", "planHash", "partition", "subject", "origin"] {
        assert!(!text.to_lowercase().contains(&raw.to_lowercase()), "{raw:?} in {text:?}");
    }
}

fn labels(card: &DecisionCard) -> Vec<&str> {
    card.allowed.options.iter().map(|option| option.label.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_proposed_routine_reads_in_plain_words() {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "propose_followup",
                json!({
                    "title": "Check the news every 5 minutes",
                    "rationale": "You asked to keep up with the news.",
                    "when": { "cron": { "expr": "*/5 * * * *", "missed": "skip" } },
                }),
            ),
            ScriptedResponse::text("I suggested a routine."),
        ],
    ));
    let s = scenario(model).await;
    s.engine
        .execute(
            &web(),
            Command::Send { session_id: "s1".into(), text: "Keep me up with the news".into() },
        )
        .await
        .unwrap();
    let card = eventually("the routine's card", WITHIN, || async {
        s.engine
            .decisions(&web())
            .await
            .unwrap()
            .into_iter()
            .find(|card| card.call.tool == adk_kernel::AGENDA_TOOL_NAME)
    })
    .await;

    assert_eq!(card.heading, "Check the news every 5 minutes");
    let summary = card.summary.clone().expect("a suggestion says who and when");
    assert_eq!(summary, "Finance suggests this. Once you say yes, it runs every 5 minutes.");
    assert_eq!(card.agent.display_name, "Finance");
    for text in [card.heading.as_str(), summary.as_str(), card.agent.display_name.as_str()] {
        assert_plain(text);
    }
    for step in &card.timeline {
        assert_plain(&step.text);
    }
    assert_eq!(labels(&card), ["Do it", "Edit", "Not now"]);
    // The exact call stays, for the details.
    assert_eq!(card.call.args["title"], "Check the news every 5 minutes");
    assert!(card.call.text.starts_with("kernel:agenda("), "{}", card.call.text);

    // In chat the card says what kind of decision it is, and carries no arguments (S-I5).
    let chat = s
        .engine
        .decisions(&telegram())
        .await
        .unwrap()
        .into_iter()
        .find(|chat| chat.id == card.id)
        .unwrap();
    assert_eq!((chat.heading.as_str(), chat.summary.as_deref()), ("Suggestion", None));
    assert_eq!(labels(&chat), ["Do it", "Not now"]);

    // "Do it" approves the item, and the kernel schedules it.
    let outcome = s
        .engine
        .execute(
            &web(),
            Command::Decide {
                decision_id: card.id.clone(),
                version: card.version,
                answer: Answer::AllowOnce,
                edited_args: None,
                reason: None,
                expanded: true,
            },
        )
        .await
        .unwrap();
    let CommandOutcome::Answered { card: answered } = outcome else {
        panic!("answered: {outcome:?}");
    };
    assert_eq!(answered.status_text, "Decided");
    assert_eq!(s.service.get(&card.id).await.unwrap().unwrap().state, ApprovalState::Approved);
    eventually("the routine to be scheduled", WITHIN, || async {
        s.kernel
            .agenda()
            .list(&AgendaFilter::default())
            .await
            .unwrap()
            .into_iter()
            .find(|item| item.state.kind() == AgendaStateKind::Scheduled)
    })
    .await;
    s.kernel.stop().await.unwrap();
}
