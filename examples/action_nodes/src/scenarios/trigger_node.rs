//! Trigger Node scenarios: manual trigger with input.

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

use super::set_node::standard;

pub async fn run() -> Result<()> {
    println!("── 8. Trigger Node ────────────────────────────");

    // 8a. Manual trigger with user input
    let graph = GraphAgent::builder("trigger-manual")
        .description("Manual trigger demo")
        .channels(&["input", "triggerResult"])
        .action_node(ActionNodeConfig::Trigger(TriggerNodeConfig {
            standard: standard("manual_trigger", "Start Workflow", "triggerResult"),
            trigger_type: TriggerType::Manual,
            manual: Some(ManualTriggerConfig {
                input_label: Some("input".into()),
                default_prompt: Some("What would you like to do?".into()),
            }),
            webhook: None,
            schedule: None,
            event: None,
        }))
        .edge(START, "manual_trigger")
        .edge("manual_trigger", END)
        .build()?;

    let mut input = State::new();
    input.insert("input".into(), json!("Process the quarterly report"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    let trigger = &result["triggerResult"];
    println!("  manual:      type={}, input={}",
        trigger["trigger_type"], trigger["input"]);
    assert_eq!(trigger["trigger_type"], json!("manual"));
    assert_eq!(trigger["input"], json!("Process the quarterly report"));

    // 8b. Manual trigger with default prompt (no input in state)
    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    let trigger = &result["triggerResult"];
    println!("  default:     input={}", trigger["input"]);
    assert_eq!(trigger["input"], json!("What would you like to do?"));

    // 8c. Webhook trigger metadata (in-graph, not runtime)
    let graph = GraphAgent::builder("trigger-webhook")
        .description("Webhook trigger metadata")
        .channels(&["triggerResult"])
        .action_node(ActionNodeConfig::Trigger(TriggerNodeConfig {
            standard: standard("webhook_trigger", "Webhook Entry", "triggerResult"),
            trigger_type: TriggerType::Webhook,
            manual: None,
            webhook: Some(WebhookConfig {
                path: "/api/webhook/orders".into(),
                method: Some(HttpMethod::Post),
                auth: Some(WebhookAuthConfig {
                    auth_type: "bearer".into(),
                    token: Some("secret-token".into()),
                    header_name: None,
                    api_key: None,
                }),
            }),
            schedule: None,
            event: None,
        }))
        .edge(START, "webhook_trigger")
        .edge("webhook_trigger", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    let trigger = &result["triggerResult"];
    println!("  webhook:     type={}, path={}",
        trigger["trigger_type"], trigger["path"]);
    assert_eq!(trigger["trigger_type"], json!("webhook"));
    assert_eq!(trigger["path"], json!("/api/webhook/orders"));

    println!("  ✓ All Trigger node scenarios passed\n");
    Ok(())
}
