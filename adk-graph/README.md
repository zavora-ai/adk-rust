# adk-graph

Graph-based workflow orchestration for Rust Agent Development Kit (ADK-Rust) agents, inspired by LangGraph.

[![Crates.io](https://img.shields.io/crates/v/adk-graph.svg)](https://crates.io/crates/adk-graph)
[![Documentation](https://docs.rs/adk-graph/badge.svg)](https://docs.rs/adk-graph)
[![License](https://img.shields.io/crates/l/adk-graph.svg)](LICENSE)

## Overview

`adk-graph` provides a powerful way to build complex, stateful agent workflows using a graph-based approach. It brings LangGraph-style capabilities to the Rust ADK ecosystem while maintaining full compatibility with ADK's agent system, callbacks, and streaming infrastructure.

## Features

- **Graph-Based Workflows**: Define agent workflows as directed graphs with nodes and edges
- **AgentNode**: Wrap LLM agents as graph nodes with custom input/output mappers
- **Cyclic Support**: Native support for loops and iterative reasoning (ReAct pattern)
- **Conditional Routing**: Dynamic edge routing based on state
- **State Management**: Typed state with reducers (overwrite, append, sum, custom)
- **Checkpointing**: Persistent state after each step (memory, SQLite)
- **Human-in-the-Loop**: Interrupt before/after nodes, dynamic interrupts
- **Streaming**: Multiple stream modes (values, updates, messages, debug)
- **ADK Integration**: Full callback support, works with existing runners

## Architecture

```
              ┌─────────────────────────────────────────┐
              │              Agent Trait                │
              │  (name, description, run, sub_agents)   │
              └────────────────┬────────────────────────┘
                               │
       ┌───────────────────────┼───────────────────────┐
       │                       │                       │
┌──────▼──────┐      ┌─────────▼─────────┐   ┌─────────▼─────────┐
│  LlmAgent   │      │   GraphAgent      │   │  RealtimeAgent    │
│ (text-based)│      │ (graph workflow)  │   │  (voice-based)    │
└─────────────┘      └───────────────────┘   └───────────────────┘
```

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-graph = { version = "0.3.2", features = ["sqlite"] }
adk-agent = "0.3.2"
adk-model = "0.3.2"
adk-core = "0.3.2"
```

### Basic Graph with AgentNode

```rust
use adk_graph::{
    edge::{END, START},
    node::{AgentNode, ExecutionConfig},
    agent::GraphAgent,
    state::State,
};
use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Create LLM agents
    let translator = Arc::new(
        LlmAgentBuilder::new("translator")
            .model(model.clone())
            .instruction("Translate the input text to French. Only output the translation.")
            .build()?
    );

    let summarizer = Arc::new(
        LlmAgentBuilder::new("summarizer")
            .model(model.clone())
            .instruction("Summarize the input text in one sentence.")
            .build()?
    );

    // Create AgentNodes with input/output mappers
    let translator_node = AgentNode::new(translator)
        .with_input_mapper(|state| {
            let text = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(text)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.is_empty() {
                        updates.insert("translation".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    let summarizer_node = AgentNode::new(summarizer)
        .with_input_mapper(|state| {
            let text = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(text)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.is_empty() {
                        updates.insert("summary".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    // Build graph with parallel execution
    let agent = GraphAgent::builder("text_processor")
        .description("Translates and summarizes text in parallel")
        .channels(&["input", "translation", "summary"])
        .node(translator_node)
        .node(summarizer_node)
        .edge(START, "translator")
        .edge(START, "summarizer")  // Both start in parallel
        .edge("translator", END)
        .edge("summarizer", END)
        .build()?;

    // Execute
    let mut input = State::new();
    input.insert("input".to_string(), json!("AI is transforming how we work and live."));

    let result = agent.invoke(input, ExecutionConfig::new("thread-1".to_string())).await?;

    println!("Translation: {}", result.get("translation").and_then(|v| v.as_str()).unwrap_or(""));
    println!("Summary: {}", result.get("summary").and_then(|v| v.as_str()).unwrap_or(""));

    Ok(())
}
```

### Conditional Routing with LLM Classification

```rust
use adk_graph::{edge::Router, node::NodeOutput};

// Create a classifier agent
let classifier = Arc::new(
    LlmAgentBuilder::new("classifier")
        .model(model.clone())
        .instruction("Classify the sentiment as 'positive', 'negative', or 'neutral'. Reply with one word only.")
        .build()?
);

let classifier_node = AgentNode::new(classifier)
    .with_input_mapper(|state| {
        let msg = state.get("message").and_then(|v| v.as_str()).unwrap_or("");
        adk_core::Content::new("user").with_text(&format!("Classify: {}", msg))
    })
    .with_output_mapper(|events| {
        let mut updates = std::collections::HashMap::new();
        for event in events {
            if let Some(content) = event.content() {
                let text: String = content.parts.iter()
                    .filter_map(|p| p.text())
                    .collect::<Vec<_>>()
                    .join("")
                    .to_lowercase();

                let sentiment = if text.contains("positive") { "positive" }
                    else if text.contains("negative") { "negative" }
                    else { "neutral" };

                updates.insert("sentiment".to_string(), json!(sentiment));
            }
        }
        updates
    });

// Build conditional routing graph
let graph = StateGraph::with_channels(&["message", "sentiment", "response"])
    .add_node(classifier_node)
    .add_node(positive_handler_node)
    .add_node(negative_handler_node)
    .add_node(neutral_handler_node)
    .add_edge(START, "classifier")
    .add_conditional_edges(
        "classifier",
        Router::by_field("sentiment"),  // Route based on "sentiment" field
        [
            ("positive", "positive_handler"),
            ("negative", "negative_handler"),
            ("neutral", "neutral_handler"),
        ],
    )
    .add_edge("positive_handler", END)
    .add_edge("negative_handler", END)
    .add_edge("neutral_handler", END)
    .compile()?;
```

### Human-in-the-Loop with Risk Assessment

```rust
use adk_graph::{checkpoint::MemoryCheckpointer, error::GraphError};

let checkpointer = Arc::new(MemoryCheckpointer::new());

// Planner agent assesses risk
let planner_node = AgentNode::new(planner_agent)
    .with_output_mapper(|events| {
        let mut updates = std::collections::HashMap::new();
        for event in events {
            if let Some(content) = event.content() {
                let text: String = content.parts.iter()
                    .filter_map(|p| p.text())
                    .collect::<Vec<_>>()
                    .join("");

                // Extract risk level from LLM response
                let risk = if text.to_lowercase().contains("risk: high") { "high" }
                    else if text.to_lowercase().contains("risk: medium") { "medium" }
                    else { "low" };

                updates.insert("plan".to_string(), json!(text));
                updates.insert("risk_level".to_string(), json!(risk));
            }
        }
        updates
    });

let graph = StateGraph::with_channels(&["task", "plan", "risk_level", "approved", "result"])
    .add_node(planner_node)
    .add_node(executor_node)
    .add_node_fn("review", |ctx| async move {
        let risk = ctx.get("risk_level").and_then(|v| v.as_str()).unwrap_or("low");
        let approved = ctx.get("approved").and_then(|v| v.as_bool());

        if approved == Some(true) {
            return Ok(NodeOutput::new());  // Continue
        }

        if risk == "high" || risk == "medium" {
            // Interrupt for human approval
            return Ok(NodeOutput::interrupt_with_data(
                "Human approval required",
                json!({"risk_level": risk, "action": "Set 'approved' to true to continue"})
            ));
        }

        // Auto-approve low risk
        Ok(NodeOutput::new().with_update("approved", json!(true)))
    })
    .add_edge(START, "planner")
    .add_edge("planner", "review")
    .add_edge("review", "executor")
    .add_edge("executor", END)
    .compile()?
    .with_checkpointer_arc(checkpointer.clone());

// Execute - may pause for approval
let result = graph.invoke(input, ExecutionConfig::new("task-001".to_string())).await;

match result {
    Err(GraphError::Interrupted(interrupt)) => {
        println!("Paused: {}", interrupt.interrupt);

        // Human reviews and approves...
        graph.update_state("task-001", [("approved".to_string(), json!(true))]).await?;

        // Resume
        let final_result = graph.invoke(State::new(), ExecutionConfig::new("task-001".to_string())).await?;
    }
    Ok(result) => println!("Completed: {:?}", result),
    Err(e) => println!("Error: {}", e),
}
```

### ReAct Agent with Tools

```rust
use adk_core::Part;
use adk_tool::FunctionTool;

// Create agent with tools
let reasoner = Arc::new(
    LlmAgentBuilder::new("reasoner")
        .model(model)
        .instruction("Use tools to answer questions. Provide final answer when done.")
        .tool(Arc::new(FunctionTool::new("search", "Search for info", |_ctx, args| async move {
            Ok(json!({"result": "Search results..."}))
        })))
        .tool(Arc::new(FunctionTool::new("calculator", "Calculate", |_ctx, args| async move {
            Ok(json!({"result": "42"}))
        })))
        .build()?
);

let reasoner_node = AgentNode::new(reasoner)
    .with_output_mapper(|events| {
        let mut updates = std::collections::HashMap::new();
        let mut has_tool_calls = false;

        for event in events {
            if let Some(content) = event.content() {
                for part in &content.parts {
                    if let Part::FunctionCall { name, .. } = part {
                        has_tool_calls = true;
                    }
                }
            }
        }

        updates.insert("has_tool_calls".to_string(), json!(has_tool_calls));
        updates
    });

// Build ReAct graph with cycle
let graph = StateGraph::with_channels(&["input", "has_tool_calls", "iteration"])
    .add_node(reasoner_node)
    .add_node_fn("counter", |ctx| async move {
        let i = ctx.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);
        Ok(NodeOutput::new().with_update("iteration", json!(i + 1)))
    })
    .add_edge(START, "counter")
    .add_edge("counter", "reasoner")
    .add_conditional_edges(
        "reasoner",
        |state| {
            let has_tools = state.get("has_tool_calls").and_then(|v| v.as_bool()).unwrap_or(false);
            let iteration = state.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);

            if iteration >= 5 { return END.to_string(); }  // Safety limit
            if has_tools { "counter".to_string() } else { END.to_string() }
        },
        [("counter", "counter"), (END, END)],
    )
    .compile()?
    .with_recursion_limit(10);
```

## Node Types

### AgentNode

Wraps any ADK `Agent` (typically `LlmAgent`) as a graph node:

```rust
let node = AgentNode::new(llm_agent)
    .with_input_mapper(|state| {
        // Transform graph state to agent input Content
        adk_core::Content::new("user").with_text(state.get("input").unwrap().as_str().unwrap())
    })
    .with_output_mapper(|events| {
        // Transform agent events to state updates
        let mut updates = HashMap::new();
        // ... extract data from events
        updates
    });
```

### FunctionNode

Simple async functions for data processing:

```rust
.node_fn("process", |ctx| async move {
    let data = ctx.get("data").unwrap();
    let result = transform(data);
    Ok(NodeOutput::new().with_update("result", result))
})
```

## State Management

### Channels and Reducers

```rust
let schema = StateSchema::builder()
    .channel("current")                           // Overwrite (default)
    .list_channel("messages")                     // Append to list
    .channel_with_reducer("count", Reducer::Sum)  // Sum values
    .build();
```

## Checkpointing

```rust
// Memory (development)
let checkpointer = MemoryCheckpointer::new();

// SQLite (production)
let checkpointer = SqliteCheckpointer::new("state.db").await?;

// View checkpoint history
let checkpoints = checkpointer.list("thread-id").await?;
for cp in checkpoints {
    println!("Step {}: {:?}", cp.step, cp.state);
}
```

## Examples

All examples use real LLM integration with AgentNode:

```bash
# Parallel LLM agents with callbacks
cargo run --example graph_agent

# Sequential multi-agent pipeline
cargo run --example graph_workflow

# LLM-based sentiment classification and routing
cargo run --example graph_conditional

# ReAct pattern with tools
cargo run --example graph_react

# Multi-agent supervisor
cargo run --example graph_supervisor

# Human-in-the-loop with risk assessment
cargo run --example graph_hitl

# Checkpointing and time travel
cargo run --example graph_checkpoint
```

## Comparison with LangGraph

| Feature | LangGraph | adk-graph |
|---------|-----------|-----------|
| State Management | TypedDict + Reducers | StateSchema + Reducers |
| Execution Model | Pregel super-steps | Pregel super-steps |
| Checkpointing | Memory, SQLite, Postgres | Memory, SQLite |
| Human-in-Loop | interrupt_before/after | interrupt_before/after + dynamic |
| Streaming | 5 modes | 5 modes |
| Cycles | Native support | Native support |
| Type Safety | Python typing | Rust type system |
| LLM Integration | LangChain | AgentNode + ADK agents |

## Feature Flags

| Flag | Description |
|------|-------------|
| `sqlite` | Enable SQLite checkpointer |
| `full` | Enable all features |

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
