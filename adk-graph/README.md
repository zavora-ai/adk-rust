# adk-graph

Graph-based workflow orchestration for Rust Agent Development Kit (ADK-Rust) agents, inspired by LangGraph.

## Overview

`adk-graph` provides a powerful way to build complex, stateful agent workflows using a graph-based approach. It brings LangGraph-style capabilities to the Rust ADK ecosystem while maintaining full compatibility with ADK's agent system, callbacks, and streaming infrastructure.

## Features

- **Graph-Based Workflows**: Define agent workflows as directed graphs with nodes and edges
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
adk-graph = { version = "0.1", features = ["sqlite"] }
```

### Basic Graph

```rust
use adk_graph::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Define a simple linear graph
    let agent = GraphAgent::builder("processor")
        .description("Process data through multiple steps")
        .node_fn("fetch", |ctx| async move {
            // Fetch data
            Ok(NodeOutput::new().with_update("data", json!({"items": [1, 2, 3]})))
        })
        .node_fn("transform", |ctx| async move {
            // Transform the data
            let data = ctx.state.get("data").unwrap();
            let transformed = transform(data);
            Ok(NodeOutput::new().with_update("result", transformed))
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

### ReAct Agent with Tool Loop

```rust
use adk_graph::prelude::*;
use adk_agent::LlmAgent;

// Create the reasoning agent
let llm = Arc::new(LlmAgent::builder("reasoner")
    .model(model)
    .instruction("Reason step by step. Use tools when needed.")
    .tool(search_tool)
    .tool(calculator_tool)
    .build()?);

// Build graph with cycle
let react_agent = GraphAgent::builder("react")
    .description("ReAct agent with tool use")
    .node(AgentNode::new(llm))
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
    .edge("tools", "reasoner")  // Cycle back
    .recursion_limit(25)
    .build()?;
```

### Multi-Agent Supervisor

```rust
let supervisor = GraphAgent::builder("supervisor")
    .description("Routes tasks to specialist agents")
    // Supervisor decides routing
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
    // All agents report back
    .edge("research_agent", "supervisor")
    .edge("writer_agent", "supervisor")
    .edge("reviewer_agent", "supervisor")
    .build()?;
```

### Human-in-the-Loop Approval

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
    // Enable checkpointing and interrupt
    .checkpointer(SqliteCheckpointer::new("state.db").await?)
    .interrupt_after(&["plan"])  // Pause for human approval
    .build()?;

// First run - pauses after planning
let config = ExecutionConfig::new("approval_thread");
match approval_workflow.invoke(input, config.clone()).await {
    Err(GraphError::Interrupted(interrupt)) => {
        println!("Pending: {:?}", interrupt.state.get("pending_action"));

        // Human approves...
        let result = interrupt.resume(Some(json!({"approved": true}))).await?;
    }
    Ok(result) => println!("Completed: {:?}", result),
    Err(e) => return Err(e.into()),
}
```

### Streaming Execution

```rust
let stream = agent.stream(input, config, StreamMode::Updates);

while let Some(event) = stream.next().await {
    match event? {
        StreamEvent::NodeStart(name) => println!("Starting: {}", name),
        StreamEvent::Updates { node, updates } => {
            println!("{} updated: {:?}", node, updates);
        }
        StreamEvent::NodeEnd(name) => println!("Completed: {}", name),
        StreamEvent::Done(state) => println!("Final: {:?}", state),
        _ => {}
    }
}
```

## State Management

### Reducers

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
    // ...
    .build()?;
```

## Checkpointing

Enable persistent state for fault tolerance and human-in-the-loop:

```rust
// In-memory (development)
let checkpointer = MemoryCheckpointer::new();

// SQLite (production)
let checkpointer = SqliteCheckpointer::new("checkpoints.db").await?;

let agent = GraphAgent::builder("durable")
    .checkpointer(checkpointer)
    // ...
    .build()?;

// Resume from checkpoint
let state = agent.get_state(&config).await?;
```

## ADK Integration

GraphAgent implements the ADK `Agent` trait, so it works seamlessly with:

- **Runner**: Use with `adk-runner` for execution
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

## Feature Flags

| Flag | Description |
|------|-------------|
| `sqlite` | Enable SQLite checkpointer |
| `full` | Enable all features |

## License

Apache-2.0
