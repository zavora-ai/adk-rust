# Graph Agents

The `adk-graph` crate provides LangGraph-style workflow orchestration for building complex, stateful agent workflows. It brings graph-based workflow capabilities to the ADK-Rust ecosystem while maintaining full compatibility with ADK's agent system.

## Overview

GraphAgent allows you to define workflows as directed graphs with nodes and edges, supporting:

- **AgentNode**: Wrap LLM agents as graph nodes with custom input/output mappers
- **Cyclic Workflows**: Native support for loops and iterative reasoning (ReAct pattern)
- **Conditional Routing**: Dynamic edge routing based on state
- **State Management**: Typed state with reducers (overwrite, append, sum, custom)
- **Checkpointing**: Persistent state for fault tolerance and human-in-the-loop
- **Streaming**: Multiple stream modes (values, updates, messages, debug)

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-graph = { version = "0.1", features = ["sqlite"] }
adk-agent = "0.1"
adk-model = "0.1"
adk-core = "0.1"
```

### Basic Graph with AgentNode

The most common pattern is using `AgentNode` to wrap LLM agents as graph nodes. Each AgentNode requires:
- **Input mapper**: Transforms graph state to agent input `Content`
- **Output mapper**: Transforms agent events to state updates

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
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create an LLM agent for translation
    let translator = Arc::new(
        LlmAgentBuilder::new("translator")
            .model(model.clone())
            .instruction("Translate the input text to French. Only output the translation.")
            .build()?
    );

    // Wrap as AgentNode with mappers
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

    // Build a simple graph
    let agent = GraphAgent::builder("text_processor")
        .description("Translates text")
        .channels(&["input", "translation"])
        .node(translator_node)
        .edge(START, "translator")
        .edge("translator", END)
        .build()?;

    // Execute
    let mut input = State::new();
    input.insert("input".to_string(), json!("Hello, world!"));

    let result = agent.invoke(input, ExecutionConfig::new("thread-1")).await?;
    println!("Translation: {}", result.get("translation").and_then(|v| v.as_str()).unwrap_or(""));

    Ok(())
}
```

## Node Types

### AgentNode

Wraps any ADK `Agent` (typically `LlmAgent`) as a graph node:

```rust
let node = AgentNode::new(llm_agent)
    .with_input_mapper(|state| {
        // Transform graph state to agent input Content
        let text = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
        adk_core::Content::new("user").with_text(text)
    })
    .with_output_mapper(|events| {
        // Transform agent events to state updates
        let mut updates = std::collections::HashMap::new();
        for event in events {
            if let Some(content) = event.content() {
                let text: String = content.parts.iter()
                    .filter_map(|p| p.text())
                    .collect::<Vec<_>>()
                    .join("");
                updates.insert("output".to_string(), json!(text));
            }
        }
        updates
    });
```

### Function Nodes

Simple async functions that process state:

```rust
.node_fn("process", |ctx| async move {
    let input = ctx.state.get("input").unwrap();
    let output = process_data(input).await?;
    Ok(NodeOutput::new().with_update("output", output))
})
```

## Edge Types

### Static Edges

Direct connections between nodes:

```rust
.edge(START, "first_node")
.edge("first_node", "second_node")
.edge("second_node", END)
```

### Conditional Edges

Dynamic routing based on state:

```rust
.conditional_edge(
    "router",
    |state| {
        match state.get("next").and_then(|v| v.as_str()) {
            Some("research") => "research_node".to_string(),
            Some("write") => "write_node".to_string(),
            _ => END.to_string(),
        }
    },
    [
        ("research_node", "research_node"),
        ("write_node", "write_node"),
        (END, END),
    ],
)
```

### Router Helpers

Use built-in routers for common patterns:

```rust
use adk_graph::edge::Router;

// Route based on a state field value
.conditional_edge("classifier", Router::by_field("sentiment"), [
    ("positive", "positive_handler"),
    ("negative", "negative_handler"),
    ("neutral", "neutral_handler"),
])

// Route based on boolean field
.conditional_edge("check", Router::by_bool("approved"), [
    ("true", "execute"),
    ("false", "reject"),
])

// Limit iterations
.conditional_edge("loop", Router::max_iterations("count", 5), [
    ("continue", "process"),
    ("done", END),
])
```

## Parallel Execution

Multiple edges from a single node execute in parallel:

```rust
let agent = GraphAgent::builder("parallel_processor")
    .channels(&["input", "translation", "summary", "analysis"])
    .node(translator_node)
    .node(summarizer_node)
    .node(analyzer_node)
    .node(combiner_node)
    // All three start simultaneously
    .edge(START, "translator")
    .edge(START, "summarizer")
    .edge(START, "analyzer")
    // Wait for all to complete before combining
    .edge("translator", "combiner")
    .edge("summarizer", "combiner")
    .edge("analyzer", "combiner")
    .edge("combiner", END)
    .build()?;
```

## Cyclic Graphs (ReAct Pattern)

Build iterative reasoning agents with cycles:

```rust
use adk_core::Part;

// Create agent with tools
let reasoner = Arc::new(
    LlmAgentBuilder::new("reasoner")
        .model(model)
        .instruction("Use tools to answer questions. Provide final answer when done.")
        .tool(search_tool)
        .tool(calculator_tool)
        .build()?
);

let reasoner_node = AgentNode::new(reasoner)
    .with_input_mapper(|state| {
        let question = state.get("question").and_then(|v| v.as_str()).unwrap_or("");
        adk_core::Content::new("user").with_text(question)
    })
    .with_output_mapper(|events| {
        let mut updates = std::collections::HashMap::new();
        let mut has_tool_calls = false;
        let mut response = String::new();

        for event in events {
            if let Some(content) = event.content() {
                for part in &content.parts {
                    match part {
                        Part::FunctionCall { name, .. } => {
                            has_tool_calls = true;
                        }
                        Part::Text { text } => {
                            response.push_str(text);
                        }
                        _ => {}
                    }
                }
            }
        }

        updates.insert("has_tool_calls".to_string(), json!(has_tool_calls));
        updates.insert("response".to_string(), json!(response));
        updates
    });

// Build graph with cycle
let react_agent = StateGraph::with_channels(&["question", "has_tool_calls", "response", "iteration"])
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

            // Safety limit
            if iteration >= 5 { return END.to_string(); }

            if has_tools {
                "counter".to_string()  // Loop back
            } else {
                END.to_string()  // Done
            }
        },
        [("counter", "counter"), (END, END)],
    )
    .compile()?
    .with_recursion_limit(10);
```

## Multi-Agent Supervisor

Route tasks to specialist agents:

```rust
// Create supervisor agent
let supervisor = Arc::new(
    LlmAgentBuilder::new("supervisor")
        .model(model.clone())
        .instruction("Route tasks to: researcher, writer, or coder. Reply with agent name only.")
        .build()?
);

let supervisor_node = AgentNode::new(supervisor)
    .with_output_mapper(|events| {
        let mut updates = std::collections::HashMap::new();
        for event in events {
            if let Some(content) = event.content() {
                let text: String = content.parts.iter()
                    .filter_map(|p| p.text())
                    .collect::<Vec<_>>()
                    .join("")
                    .to_lowercase();

                let next = if text.contains("researcher") { "researcher" }
                    else if text.contains("writer") { "writer" }
                    else if text.contains("coder") { "coder" }
                    else { "done" };

                updates.insert("next_agent".to_string(), json!(next));
            }
        }
        updates
    });

// Build supervisor graph
let graph = StateGraph::with_channels(&["task", "next_agent", "research", "content", "code"])
    .add_node(supervisor_node)
    .add_node(researcher_node)
    .add_node(writer_node)
    .add_node(coder_node)
    .add_edge(START, "supervisor")
    .add_conditional_edges(
        "supervisor",
        Router::by_field("next_agent"),
        [
            ("researcher", "researcher"),
            ("writer", "writer"),
            ("coder", "coder"),
            ("done", END),
        ],
    )
    // Agents report back to supervisor
    .add_edge("researcher", "supervisor")
    .add_edge("writer", "supervisor")
    .add_edge("coder", "supervisor")
    .compile()?;
```

## State Management

### State Schema with Reducers

Control how state updates are merged:

```rust
let schema = StateSchema::builder()
    .channel("current_step")                    // Overwrite (default)
    .list_channel("messages")                   // Append to list
    .channel_with_reducer("count", Reducer::Sum) // Sum values
    .channel_with_reducer("data", Reducer::Custom(Arc::new(|old, new| {
        // Custom merge logic
        merge_json(old, new)
    })))
    .build();

let agent = GraphAgent::builder("stateful")
    .state_schema(schema)
    // ... nodes and edges
    .build()?;
```

### Reducer Types

| Reducer | Behavior |
|---------|----------|
| `Overwrite` | Replace old value with new (default) |
| `Append` | Append to list |
| `Sum` | Add numeric values |
| `Custom` | Custom merge function |

## Checkpointing

Enable persistent state for fault tolerance and human-in-the-loop:

### In-Memory (Development)

```rust
use adk_graph::checkpoint::MemoryCheckpointer;

let checkpointer = Arc::new(MemoryCheckpointer::new());

let graph = StateGraph::with_channels(&["task", "result"])
    // ... nodes and edges
    .compile()?
    .with_checkpointer_arc(checkpointer.clone());
```

### SQLite (Production)

```rust
use adk_graph::checkpoint::SqliteCheckpointer;

let checkpointer = SqliteCheckpointer::new("checkpoints.db").await?;

let graph = StateGraph::with_channels(&["task", "result"])
    // ... nodes and edges
    .compile()?
    .with_checkpointer(checkpointer);
```

### Checkpoint History (Time Travel)

```rust
// List all checkpoints for a thread
let checkpoints = checkpointer.list("thread-id").await?;
for cp in checkpoints {
    println!("Step {}: {:?}", cp.step, cp.state.get("status"));
}

// Load a specific checkpoint
if let Some(checkpoint) = checkpointer.load_by_id(&checkpoint_id).await? {
    println!("State at step {}: {:?}", checkpoint.step, checkpoint.state);
}
```

## Human-in-the-Loop

Pause execution for human approval using dynamic interrupts:

```rust
use adk_graph::{error::GraphError, node::NodeOutput};

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

// Review node with dynamic interrupt
let graph = StateGraph::with_channels(&["task", "plan", "risk_level", "approved", "result"])
    .add_node(planner_node)
    .add_node(executor_node)
    .add_node_fn("review", |ctx| async move {
        let risk = ctx.get("risk_level").and_then(|v| v.as_str()).unwrap_or("low");
        let approved = ctx.get("approved").and_then(|v| v.as_bool());

        // Already approved - continue
        if approved == Some(true) {
            return Ok(NodeOutput::new());
        }

        // High/medium risk - interrupt for approval
        if risk == "high" || risk == "medium" {
            return Ok(NodeOutput::interrupt_with_data(
                &format!("{} RISK: Human approval required", risk.to_uppercase()),
                json!({
                    "plan": ctx.get("plan"),
                    "risk_level": risk,
                    "action": "Set 'approved' to true to continue"
                })
            ));
        }

        // Low risk - auto-approve
        Ok(NodeOutput::new().with_update("approved", json!(true)))
    })
    .add_edge(START, "planner")
    .add_edge("planner", "review")
    .add_edge("review", "executor")
    .add_edge("executor", END)
    .compile()?
    .with_checkpointer_arc(checkpointer.clone());

// Execute - may pause for approval
let thread_id = "task-001";
let result = graph.invoke(input, ExecutionConfig::new(thread_id)).await;

match result {
    Err(GraphError::Interrupted(interrupt)) => {
        println!("*** EXECUTION PAUSED ***");
        println!("Reason: {}", interrupt.interrupt);
        println!("Plan awaiting approval: {:?}", interrupt.state.get("plan"));

        // Human reviews and approves...

        // Update state with approval
        graph.update_state(thread_id, [("approved".to_string(), json!(true))]).await?;

        // Resume execution
        let final_result = graph.invoke(State::new(), ExecutionConfig::new(thread_id)).await?;
        println!("Final result: {:?}", final_result.get("result"));
    }
    Ok(result) => {
        println!("Completed without interrupt: {:?}", result);
    }
    Err(e) => {
        println!("Error: {}", e);
    }
}
```

### Static Interrupts

Use `interrupt_before` or `interrupt_after` for mandatory pause points:

```rust
let graph = StateGraph::with_channels(&["task", "plan", "result"])
    .add_node(planner_node)
    .add_node(executor_node)
    .add_edge(START, "planner")
    .add_edge("planner", "executor")
    .add_edge("executor", END)
    .compile()?
    .with_interrupt_before(&["executor"]);  // Always pause before execution
```

## Streaming Execution

Stream events as the graph executes:

```rust
use futures::StreamExt;
use adk_graph::stream::StreamMode;

let stream = agent.stream(input, config, StreamMode::Updates);

while let Some(event) = stream.next().await {
    match event? {
        StreamEvent::NodeStart(name) => println!("Starting: {}", name),
        StreamEvent::Updates { node, updates } => {
            println!("{} updated state: {:?}", node, updates);
        }
        StreamEvent::NodeEnd(name) => println!("Completed: {}", name),
        StreamEvent::Done(state) => println!("Final state: {:?}", state),
        _ => {}
    }
}
```

### Stream Modes

| Mode | Description |
|------|-------------|
| `Values` | Stream full state after each node |
| `Updates` | Stream only state changes |
| `Messages` | Stream message-type updates |
| `Debug` | Stream all internal events |

## ADK Integration

GraphAgent implements the ADK `Agent` trait, so it works with:

- **Runner**: Use with `adk-runner` for standard execution
- **Callbacks**: Full support for before/after callbacks
- **Sessions**: Works with `adk-session` for conversation history
- **Streaming**: Returns ADK `EventStream`

```rust
use adk_runner::Runner;

let graph_agent = GraphAgent::builder("workflow")
    .before_agent_callback(|ctx| async {
        println!("Starting graph execution for session: {}", ctx.session_id());
        Ok(())
    })
    .after_agent_callback(|ctx, event| async {
        if let Some(content) = event.content() {
            println!("Graph completed with content");
        }
        Ok(())
    })
    // ... graph definition
    .build()?;

// Use with standard ADK runner
let runner = Runner::new(Arc::new(graph_agent));
let events = runner.run(session, content).await?;
```

## Examples

All examples use real LLM integration with AgentNode:

```bash
# Parallel LLM agents with before/after callbacks
cargo run --example graph_agent

# Sequential multi-agent pipeline (extractor → analyzer → formatter)
cargo run --example graph_workflow

# LLM-based sentiment classification and conditional routing
cargo run --example graph_conditional

# ReAct pattern with tools and cyclic execution
cargo run --example graph_react

# Multi-agent supervisor routing to specialists
cargo run --example graph_supervisor

# Human-in-the-loop with risk-based interrupts
cargo run --example graph_hitl

# Checkpointing and time travel debugging
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

---

**Next**: [Workflow Agents →](workflow-agents.md)
