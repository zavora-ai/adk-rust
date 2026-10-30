//! A freeze resolves an open approval prompt: the call is not run and the turn ends
//! (spec 3.1 §9.2 I20; evaluation finding F-13).

#![cfg(feature = "runner")]

use crate::common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::GOVERNANCE_FROZEN_CODE;
use adk_harness::{ApprovalPrompt, ApprovalPrompter, PromptAnswer};
use adk_model::ScriptedResponse;
use futures::StreamExt;
use serde_json::json;

/// A person who never answers.
#[derive(Debug)]
struct Away;

#[async_trait::async_trait]
impl ApprovalPrompter for Away {
    async fn prompt(&self, _prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_freeze_abandons_an_open_prompt() {
    let f = common::fixture(
        vec![
            ScriptedResponse::function_call("bash", json!({ "command": "touch ran.txt" })),
            ScriptedResponse::text("done"),
        ],
        |b| b.prompter(Arc::new(Away)),
    )
    .await;
    let governance = f.harness.runner().governance().clone();
    let freezer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        governance.freeze("incident 7: stop the agents");
    });

    let started = Instant::now();
    let stream = f.harness.send(&f.user, &f.session, "go").await.unwrap();
    let results = tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
        .await
        .expect("the open prompt resolves soon after the freeze");
    freezer.await.unwrap();

    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    let errors: Vec<_> = results.iter().filter_map(|result| result.as_ref().err()).collect();
    assert!(errors.iter().any(|error| error.code == GOVERNANCE_FROZEN_CODE), "{errors:?}");
    assert!(!f.path("ran.txt").exists(), "the call is not run");
}
