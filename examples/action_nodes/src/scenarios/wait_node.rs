//! Wait Node scenarios: fixed delay.

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

use super::set_node::standard;

pub async fn run() -> Result<()> {
    println!("── 6. Wait Node ───────────────────────────────");

    // 6a. Fixed delay (50ms — fast for demo)
    let graph = GraphAgent::builder("wait-demo")
        .description("Wait node demo")
        .channels(&["waitResult"])
        .action_node(ActionNodeConfig::Wait(WaitNodeConfig {
            standard: standard("wait_fixed", "Short Delay", "waitResult"),
            wait_type: WaitType::Fixed,
            fixed: Some(FixedDuration { duration: 50, unit: "ms".into() }),
            until: None,
            webhook: None,
            condition: None,
        }))
        .edge(START, "wait_fixed")
        .edge("wait_fixed", END)
        .build()?;

    let start = std::time::Instant::now();
    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    let elapsed = start.elapsed();
    println!("  fixed(50ms): waited_ms={}, actual={:.0}ms",
        result["waitResult"]["waited_ms"], elapsed.as_millis());
    assert!(elapsed.as_millis() >= 40); // allow small timing variance

    // 6b. Condition polling (already true)
    let graph = GraphAgent::builder("wait-cond-demo")
        .description("Wait condition demo")
        .channels(&["ready", "waitResult"])
        .action_node(ActionNodeConfig::Wait(WaitNodeConfig {
            standard: standard("wait_cond", "Wait for Ready", "waitResult"),
            wait_type: WaitType::Condition,
            fixed: None,
            until: None,
            webhook: None,
            condition: Some(ConditionPolling {
                condition: "{{ready}}".into(),
                interval_ms: 10,
                max_wait_ms: 1000,
            }),
        }))
        .edge(START, "wait_cond")
        .edge("wait_cond", END)
        .build()?;

    let mut input = State::new();
    input.insert("ready".into(), json!("true"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  condition:   met={}, elapsed={}ms",
        result["waitResult"]["condition_met"], result["waitResult"]["elapsed_ms"]);
    assert_eq!(result["waitResult"]["condition_met"], json!(true));

    println!("  ✓ All Wait node scenarios passed\n");
    Ok(())
}
