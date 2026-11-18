#![cfg(feature = "yaml")]
//! The sample YAML workflow (`tests/workflows/refund_desk.yaml`) on a live model: two LLM
//! agent nodes on Anthropic `claude-haiku-5-5`, a conditional branch on the first agent's
//! answer, and an approval interrupt that resumes.
//!
//! Requires `ANTHROPIC_API_KEY`; run with
//! `cargo nextest run -p adk-graph --features yaml --test integration --run-ignored only --no-capture -E 'test(/workflow_definition_live/)'`.

use std::path::Path;
use std::sync::Arc;

use adk_core::Llm;
use adk_graph::definition::{WorkflowDefinition, WorkflowRegistry};
use adk_graph::{ExecutionConfig, GraphError, Interrupt, State};
use serde_json::json;

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn the_refund_workflow_runs_live_and_resumes_after_approval() {
    dotenvy::dotenv().ok();
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let registry = WorkflowRegistry::new().model_resolver(move |reference| {
        let model = reference.strip_prefix("anthropic/").ok_or_else(|| {
            adk_core::AdkError::model(format!("only anthropic/ models are wired here: {reference}"))
        })?;
        let config =
            adk_model::anthropic::AnthropicConfig::new(key.clone(), model).with_max_tokens(256);
        Ok(Arc::new(adk_model::anthropic::AnthropicClient::new(config)?) as Arc<dyn Llm>)
    });
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/workflows/refund_desk.yaml");
    let graph = WorkflowDefinition::from_file(path).unwrap().build(&registry).await.unwrap();

    println!("| step | result |");
    println!("|---|---|");
    let request =
        "Hi, the blender I bought last week (order 4411) arrived broken. I want my money back.";
    let paused = graph
        .invoke(
            State::from([("input".to_string(), json!(request))]),
            ExecutionConfig::new("live-1"),
        )
        .await
        .unwrap_err();
    let GraphError::Interrupted(paused) = paused else {
        panic!("expected a pause, got {paused:?}")
    };
    println!("| classify (llm, claude-haiku-5-5) | category = {} |", paused.state["category"]);
    let Interrupt::Dynamic { message, .. } = &paused.interrupt else {
        panic!("expected the approval pause, got {:?}", paused.interrupt)
    };
    println!("| conditional route | category matches (?i)refund -> approve |");
    println!("| approve (interrupt) | paused: {message:?} |");
    assert!(paused.state["category"].as_str().unwrap().to_lowercase().contains("refund"));

    let state = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("live-1")
                .with_resume_value("approve", json!({ "approved": true })),
        )
        .await
        .unwrap();
    println!("| resume | approval = {} |", state["approval"]);
    println!("| refund_reply (llm, claude-haiku-5-5) | reply = {} |", state["reply"]);
    println!("| close (set) | status = {} |", state["status"]);
    assert_eq!(state["approval"], json!({ "approved": true }));
    let reply = state["reply"].as_str().unwrap().to_lowercase();
    assert!(reply.contains("refund") || reply.contains("approved"), "{reply}");
    assert_eq!(state["status"], json!(format!("closed: {}", state["category"].as_str().unwrap())));
}
