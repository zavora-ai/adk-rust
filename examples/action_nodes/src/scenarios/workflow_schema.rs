//! WorkflowSchema scenario: load a graph from JSON and execute it.

use adk_graph::ExecutionConfig;
use adk_graph::state::State;
use adk_graph::workflow::WorkflowSchema;
use anyhow::Result;
use serde_json::json;

pub async fn run() -> Result<()> {
    println!("── 9. WorkflowSchema (JSON → Graph) ───────────");

    // Define a workflow as JSON — this is what adk-studio produces.
    // Note: StandardProperties are flattened into the node object (not nested).
    let workflow_json = serde_json::to_string(&json!({
        "edges": [
            {"from": "__start__", "to": "init_vars"},
            {"from": "init_vars", "to": "greet"},
            {"from": "greet", "to": "__end__"}
        ],
        "conditions": [],
        "actionNodes": {
            "init_vars": {
                "type": "set",
                "id": "init_vars",
                "name": "Initialize",
                "errorHandling": {"mode": "stop"},
                "tracing": {"enabled": true, "logLevel": "debug"},
                "callbacks": {},
                "execution": {"timeout": 30000},
                "mapping": {"outputKey": "initResult"},
                "mode": "set",
                "variables": [
                    {"key": "user_name", "value": "World", "valueType": "string", "isSecret": false}
                ]
            },
            "greet": {
                "type": "transform",
                "id": "greet",
                "name": "Generate Greeting",
                "errorHandling": {"mode": "stop"},
                "tracing": {"enabled": true, "logLevel": "debug"},
                "callbacks": {},
                "execution": {"timeout": 30000},
                "mapping": {"outputKey": "greeting"},
                "transformType": "template",
                "template": "Hello, {{user_name}}! This graph was loaded from JSON."
            }
        },
        "agentNodes": []
    }))?;

    // Load and build the graph from JSON
    let schema = WorkflowSchema::from_json(&workflow_json)?;
    let graph = schema.build_graph("json-workflow")?;

    // Execute — the set node initializes user_name, then transform interpolates it
    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  from JSON:   greeting={}", result["greeting"]);
    assert_eq!(
        result["greeting"],
        json!("Hello, World! This graph was loaded from JSON.")
    );

    // Execute with custom input — set node overwrites to "World"
    let mut input = State::new();
    input.insert("user_name".into(), json!("ADK Developer"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    println!("  with input:  greeting={}", result["greeting"]);
    assert!(result["greeting"].as_str().unwrap().contains("World"));

    println!("  ✓ WorkflowSchema scenario passed\n");
    Ok(())
}
