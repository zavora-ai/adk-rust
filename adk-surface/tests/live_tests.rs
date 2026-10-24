//! The WP-A scenario on a live model (spec 3.3 §6, §7.1, §11): a kernel-hosted harness agent
//! on `claude-sonnet-5-5` holds a write, chat allows it once, the web home is told who decided,
//! and the write runs once.
//!
//! Needs `ANTHROPIC_API_KEY`, read from the repository's `.env` when present:
//!
//! ```text
//! cargo test -p adk-surface --features web --test live_tests -- --ignored --nocapture
//! ```

#![cfg(feature = "web")]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{Llm, RunState};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_surface::{Answer, Command, CommandOutcome, ThreadItem};
use common::*;

const WITHIN: Duration = Duration::from_secs(180);

fn model() -> Arc<dyn Llm> {
    let env = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
    if env.exists() {
        dotenvy::from_path_override(&env).expect("the .env file loads");
    }
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    Arc::new(AnthropicClient::new(AnthropicConfig::new(key, "claude-sonnet-5-5")).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "calls the Anthropic API"]
async fn a_live_agent_waits_for_one_answer_and_runs_once() {
    let s = scenario(model()).await;
    let sent = s
        .engine
        .execute(
            &web(),
            Command::Send {
                session_id: "live".into(),
                text: "Create the file invoice.txt containing exactly INV-1, using write_file. \
                       Then reply with one short sentence."
                    .into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(sent, CommandOutcome::Sent { .. }));

    let card = eventually("the held write's card", WITHIN, || async {
        s.engine.decisions(&web()).await.unwrap().into_iter().next()
    })
    .await;
    println!("web home card: {}", serde_json::to_string_pretty(&card).unwrap());
    let chat_card = s.engine.decision(&telegram(), &card.id).await.unwrap();
    println!("chat card: {}", serde_json::to_string_pretty(&chat_card).unwrap());
    assert_eq!(chat_card.call.args, serde_json::Value::Null, "chat carries no arguments");
    assert_eq!(card.status_text, "Waiting for you");
    assert_eq!(card.call.tool, "write_file");
    assert!(card.heading.starts_with("Write file "), "{}", card.heading);

    let decide = |answer| Command::Decide {
        decision_id: card.id.clone(),
        version: card.version,
        answer,
        edited_args: None,
        reason: None,
        expanded: true,
    };
    let chat = s.engine.execute(&telegram(), decide(Answer::AllowOnce)).await.unwrap();
    assert!(matches!(chat, CommandOutcome::Answered { .. }), "{chat:?}");
    let web_outcome = s.engine.execute(&web(), decide(Answer::Deny)).await.unwrap();
    let CommandOutcome::AlreadyDecided { decided, .. } = web_outcome else {
        panic!("the web home is told it was decided: {web_outcome:?}");
    };
    println!("web home: {}", decided.text);
    assert!(decided.text.starts_with("You said yes in Telegram at "));

    eventually("the woken process to finish", WITHIN, || async {
        let processes = s.kernel.processes().ps(Default::default()).await.unwrap();
        processes.into_iter().find(|process| process.run.state == RunState::Completed)
    })
    .await;
    let written = std::fs::read_to_string(s.path.join("invoice.txt")).unwrap();
    assert_eq!(written.trim(), "INV-1");
    let thread = s.engine.thread(&web(), "live").await.unwrap();
    for entry in &thread {
        for item in &entry.items {
            println!("{} {}", entry.at_text, serde_json::to_string(item).unwrap());
        }
    }
    let writes = thread
        .iter()
        .flat_map(|entry| &entry.items)
        .filter(|item| matches!(item, ThreadItem::Result { tool, ok: true, .. } if tool == "write_file"))
        .count();
    assert_eq!(writes, 1, "the write ran once");
    s.kernel.stop().await.unwrap();
}
