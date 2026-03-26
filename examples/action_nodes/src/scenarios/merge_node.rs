//! Merge Node scenarios: waitAll, waitAny, waitN with combine strategies.

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

use super::set_node::standard;

pub async fn run() -> Result<()> {
    println!("── 5. Merge Node ──────────────────────────────");

    // 5a. WaitAll with array combine
    let graph = GraphAgent::builder("merge-all-demo")
        .description("Merge waitAll demo")
        .channels(&["mergeResult"])
        .action_node(ActionNodeConfig::Merge(MergeNodeConfig {
            standard: standard("merge_all", "Merge All Branches", "mergeResult"),
            mode: MergeMode::WaitAll,
            combine_strategy: CombineStrategy::Array,
            required_count: None,
            timeout: None,
        }))
        .edge(START, "merge_all")
        .edge("merge_all", END)
        .build()?;

    let mut input = State::new();
    input.insert("branch:api_result".into(), json!({"status": "ok", "data": [1, 2, 3]}));
    input.insert("branch:db_result".into(), json!({"rows": 42}));
    input.insert("branch:cache_result".into(), json!("hit"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    let merged = &result["mergeResult"];
    println!("  waitAll:     merged {} branches into array", merged.as_array().map(|a| a.len()).unwrap_or(0));
    assert!(merged.is_array());
    assert_eq!(merged.as_array().unwrap().len(), 3);

    // 5b. WaitAll with object combine (keyed by branch name)
    let graph = GraphAgent::builder("merge-obj-demo")
        .description("Merge object demo")
        .channels(&["mergeResult"])
        .action_node(ActionNodeConfig::Merge(MergeNodeConfig {
            standard: standard("merge_obj", "Merge as Object", "mergeResult"),
            mode: MergeMode::WaitAll,
            combine_strategy: CombineStrategy::Object,
            required_count: None,
            timeout: None,
        }))
        .edge(START, "merge_obj")
        .edge("merge_obj", END)
        .build()?;

    let mut input = State::new();
    input.insert("branch:weather".into(), json!({"temp": 72}));
    input.insert("branch:news".into(), json!({"headline": "ADK Released"}));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    let merged = &result["mergeResult"];
    println!("  object:      weather.temp={}, news.headline={}",
        merged["weather"]["temp"], merged["news"]["headline"]);
    assert_eq!(merged["weather"]["temp"], json!(72));

    // 5c. WaitN with first combine
    let graph = GraphAgent::builder("merge-n-demo")
        .description("Merge waitN demo")
        .channels(&["mergeResult"])
        .action_node(ActionNodeConfig::Merge(MergeNodeConfig {
            standard: standard("merge_n", "Merge First 2", "mergeResult"),
            mode: MergeMode::WaitN,
            combine_strategy: CombineStrategy::First,
            required_count: Some(2),
            timeout: None,
        }))
        .edge(START, "merge_n")
        .edge("merge_n", END)
        .build()?;

    let mut input = State::new();
    input.insert("branch:fast".into(), json!("quick result"));
    input.insert("branch:medium".into(), json!("medium result"));
    input.insert("branch:slow".into(), json!("slow result"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  waitN(2):    first={}", result["mergeResult"]);
    assert!(result["mergeResult"].is_string());

    println!("  ✓ All Merge node scenarios passed\n");
    Ok(())
}
