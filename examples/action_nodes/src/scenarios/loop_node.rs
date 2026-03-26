//! Loop Node scenarios: forEach, while, times with result collection.

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

use super::set_node::standard;

pub async fn run() -> Result<()> {
    println!("── 4. Loop Node ───────────────────────────────");

    // 4a. forEach: iterate over an array
    let graph = GraphAgent::builder("foreach-demo")
        .description("ForEach loop demo")
        .channels(&["items", "item", "index", "loopResult", "collected"])
        .action_node(ActionNodeConfig::Loop(LoopNodeConfig {
            standard: standard("foreach", "Process Items", "loopResult"),
            loop_type: LoopType::ForEach,
            for_each: Some(ForEachConfig {
                source: "items".into(),
                item_var: "item".into(),
                index_var: "index".into(),
            }),
            while_config: None,
            times: None,
            parallel: None,
            results: Some(ResultsConfig {
                collect: true,
                aggregation_key: Some("collected".into()),
            }),
        }))
        .edge(START, "foreach")
        .edge("foreach", END)
        .build()?;

    let mut input = State::new();
    input.insert("items".into(), json!(["apple", "banana", "cherry"]));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  forEach:     iterations={}, collected={}",
        result["loopResult"]["iterations"], result["collected"]);
    assert_eq!(result["loopResult"]["iterations"], json!(3));

    // 4b. times: repeat N times
    let graph = GraphAgent::builder("times-demo")
        .description("Times loop demo")
        .channels(&["i", "loopResult", "indices"])
        .action_node(ActionNodeConfig::Loop(LoopNodeConfig {
            standard: standard("times", "Repeat 5x", "loopResult"),
            loop_type: LoopType::Times,
            for_each: None,
            while_config: None,
            times: Some(TimesConfig { count: 5, index_var: "i".into() }),
            parallel: None,
            results: Some(ResultsConfig {
                collect: true,
                aggregation_key: Some("indices".into()),
            }),
        }))
        .edge(START, "times")
        .edge("times", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  times(5):    iterations={}, indices={}",
        result["loopResult"]["iterations"], result["indices"]);
    assert_eq!(result["loopResult"]["iterations"], json!(5));
    assert_eq!(result["indices"], json!([0, 1, 2, 3, 4]));

    // 4c. while: conditional loop
    let graph = GraphAgent::builder("while-demo")
        .description("While loop demo")
        .channels(&["ready", "loopResult"])
        .action_node(ActionNodeConfig::Loop(LoopNodeConfig {
            standard: standard("while_loop", "Wait Until Ready", "loopResult"),
            loop_type: LoopType::While,
            for_each: None,
            while_config: Some(WhileConfig {
                condition: "{{ready}}".into(),
                max_iterations: 100,
            }),
            times: None,
            parallel: None,
            results: None,
        }))
        .edge(START, "while_loop")
        .edge("while_loop", END)
        .build()?;

    let mut input = State::new();
    input.insert("ready".into(), json!("true"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  while(true): iterations={}", result["loopResult"]["iterations"]);
    assert_eq!(result["loopResult"]["iterations"], json!(1));

    println!("  ✓ All Loop node scenarios passed\n");
    Ok(())
}
