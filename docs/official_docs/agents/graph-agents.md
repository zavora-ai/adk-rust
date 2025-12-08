# Graph Agents

The `adk-graph` crate provides LangGraph-style workflow orchestration for building complex, stateful agent workflows. It brings graph-based workflow capabilities to the ADK-Rust ecosystem while maintaining full compatibility with ADK's agent system.

## Overview

GraphAgent allows you to define workflows as directed graphs with nodes and edges, supporting:

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
```

### Basic Linear Graph

```rust
use adk_graph::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = GraphAgent::builder("processor")
        .description("Process data through multiple steps")
        .node_fn("fetch", |ctx| async move {
            // Fetch data
            Ok(NodeOutput::new().with_update("data", json!({"items": [1, 2, 3]})))
        })
        .node_fn("transform", |ctx| async move {
            // Transform the data
            let data = ctx.state.get("data").unwrap();
            Ok(NodeOutput::new().with_update("result", transform(data)))
        })
        .edge(START, "fetch")
        .edge("fetch", "transform")
        .edge("transform", END)
        .build()?;

    // Execute
    let result = agent.invoke(State::new(), ExecutionConfig::new("thread_1")).await?;
    println!("Result: {:?}", result.get("result"));

    Ok(())
}
```

## Node Types

### Function Nodes

Simple async functions that process state:

```rust
.node_fn("process", |ctx| async move {
    let input = ctx.state.get("input").unwrap();
    let output = process_data(input).await?;
    Ok(NodeOutput::new().with_update("output", output))
})
```

### Agent Nodes

Wrap existing ADK agents as graph nodes:

```rust
use adk_agent::LlmAgentBuilder;

let llm_agent = Arc::new(
    LlmAgentBuilder::new("reasoner")
        .model(model)
        .instruction("Analyze the input and provide insights.")
        .build()?
);

let graph = GraphAgent::builder("analyzer")
    .node(AgentNode::new(llm_agent))
    .edge(START, "reasoner")
    .edge("reasoner", END)
    .build()?;
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
            Some("research") => "research_node",
            Some("write") => "write_node",
            _ => END,
        }
    },
    [
        ("research_node", "research_node"),
        ("write_node", "write_node"),
        (END, END),
    ],
)
```

## Cyclic Graphs (ReAct Pattern)

Build iterative reasoning agents:

```rust
let react_agent = GraphAgent::builder("react")
    .description("ReAct agent with tool use")
    .node(AgentNode::new(llm_agent))
    .node_fn("tools", |ctx| async move {
        let tool_calls = extract_tool_calls(&ctx.state)?;
        let results = execute_tools(tool_calls).await?;
        Ok(NodeOutput::new().with_update("messages", results))
    })
    .edge(START, "reasoner")
    .conditional_edge(
        "reasoner",
        |state| {
            if has_tool_calls(state) { "tools" } else { END }
        },
        [("tools", "tools"), (END, END)],
    )
    .edge("tools", "reasoner")  // Cycle back for iteration
    .recursion_limit(25)  // Prevent infinite loops
    .build()?;
```

## Multi-Agent Supervisor

Route tasks to specialist agents:

```rust
let supervisor = GraphAgent::builder("supervisor")
    .description("Routes tasks to specialist agents")
    // Supervisor LLM decides routing
    .node(AgentNode::new(supervisor_llm))
    // Specialist agents
    .node(AgentNode::new(research_agent))
    .node(AgentNode::new(writer_agent))
    .node(AgentNode::new(reviewer_agent))
    // Routing logic
    .edge(START, "supervisor")
    .conditional_edge(
        "supervisor",
        Router::by_field("next_agent"),
        [
            ("research", "research_agent"),
            ("writer", "writer_agent"),
            ("reviewer", "reviewer_agent"),
            ("done", END),
        ],
    )
    // All agents report back to supervisor
    .edge("research_agent", "supervisor")
    .edge("writer_agent", "supervisor")
    .edge("reviewer_agent", "supervisor")
    .build()?;
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
let checkpointer = MemoryCheckpointer::new();

let agent = GraphAgent::builder("durable")
    .checkpointer(checkpointer)
    // ... nodes and edges
    .build()?;
```

### SQLite (Production)

```rust
let checkpointer = SqliteCheckpointer::new("checkpoints.db").await?;

let agent = GraphAgent::builder("durable")
    .checkpointer(checkpointer)
    // ... nodes and edges
    .build()?;

// Later: Resume from checkpoint
let state = agent.get_state(&config).await?;
```

## Human-in-the-Loop

Pause execution for human approval:

```rust
let approval_workflow = GraphAgent::builder("approval_flow")
    .node_fn("plan", |ctx| async move {
        let action = plan_action(&ctx.state).await?;
        Ok(NodeOutput::new().with_update("pending_action", action))
    })
    .node_fn("execute", |ctx| async move {
        let action = ctx.state.get("pending_action").unwrap();
        let result = execute_action(action).await?;
        Ok(NodeOutput::new().with_update("result", result))
    })
    .edge(START, "plan")
    .edge("plan", "execute")
    .edge("execute", END)
    .checkpointer(SqliteCheckpointer::new("state.db").await?)
    .interrupt_after(&["plan"])  // Pause after planning
    .build()?;

// First run - pauses after planning
let config = ExecutionConfig::new("approval_thread");
match approval_workflow.invoke(input, config.clone()).await {
    Err(GraphError::Interrupted(interrupt)) => {
        println!("Pending action: {:?}", interrupt.state.get("pending_action"));

        // Human reviews and approves...

        // Resume execution
        let result = interrupt.resume(Some(json!({"approved": true}))).await?;
        println!("Final result: {:?}", result);
    }
    Ok(result) => println!("Completed without interrupt: {:?}", result),
    Err(e) => return Err(e.into()),
}
```

## Streaming Execution

Stream events as the graph executes:

```rust
use futures::StreamExt;

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
        println!("Starting graph execution");
        Ok(())
    })
    .after_agent_callback(|ctx, event| async {
        println!("Graph completed");
        Ok(())
    })
    // ... graph definition
    .build()?;

// Use with standard ADK runner
let runner = Runner::new(Arc::new(graph_agent));
let events = runner.run(session, content).await?;
```

## Examples

```bash
# Basic graph workflow
cargo run --example graph_workflow

# ReAct pattern with tool loop
cargo run --example graph_react

# Multi-agent supervisor
cargo run --example graph_supervisor

# Human-in-the-loop approval
cargo run --example graph_hitl

# State persistence with checkpointing
cargo run --example graph_checkpoint

# Conditional routing
cargo run --example graph_conditional
```

## Comparison with LangGraph

| Feature | LangGraph | adk-graph |
|---------|-----------|-----------|
| State Management | TypedDict + Reducers | StateSchema + Reducers |
| Execution Model | Pregel super-steps | Pregel super-steps |
| Checkpointing | Memory, SQLite, Postgres | Memory, SQLite |
| Human-in-Loop | interrupt_before/after | interrupt_before/after |
| Streaming | 5 modes | 5 modes |
| Cycles | Native support | Native support |
| Type Safety | Python typing | Rust type system |
| Integration | LangChain | ADK ecosystem |

---

**Next**: [Workflow Agents →](workflow-agents.md)
