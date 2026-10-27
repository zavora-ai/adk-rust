# adk-graph

Graph-based workflow orchestration for Rust Agent Development Kit (ADK-Rust) agents, inspired by LangGraph.

[![Crates.io](https://img.shields.io/crates/v/adk-graph.svg)](https://crates.io/crates/adk-graph)
[![Documentation](https://docs.rs/adk-graph/badge.svg)](https://docs.rs/adk-graph)
[![License](https://img.shields.io/crates/l/adk-graph.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

## Overview

`adk-graph` builds stateful agent workflows as graphs of nodes and edges. It brings LangGraph-style capabilities to ADK-Rust: a `GraphAgent` implements the ADK `Agent` trait, so a graph runs under a `Runner`, streams through it, and sits beside `LlmAgent` and the workflow agents.

## Features

- **Graph-Based Workflows**: Define agent workflows as directed graphs with nodes and edges
- **AgentNode**: Wrap LLM agents as graph nodes with custom input/output mappers. Under a `Runner` the agent sees the caller's session — its id, and its state with the graph's state on top, so instruction templates resolve — and `GraphAgent` streams the agent's events to the caller as they are produced, so the session records them. An agent's failure is `GraphError::AgentFailed`, which keeps the agent's `AdkError` category and code
- **Cyclic Support**: Native support for loops and iterative reasoning (ReAct pattern)
- **Conditional Routing**: Dynamic edge routing based on state
- **Fan-out / fan-in**: parallel branches run concurrently in a super-step. A node with more than one incoming direct edge is deferred **automatically**, so it runs once after the branches that can still reach it arrive — branches of unequal length, a merge after a conditional branch, a node re-entered by a loop, and an entry node that is also a join run correctly with no configuration. `mark_deferred` sets a `fan_in_timeout` or an *n*-of-*m* `min_predecessors` quorum
- **State Management**: Typed state with reducers (overwrite, append, sum, custom)
- **Checkpointing**: Persistent state after each step — memory, SQLite, PostgreSQL, Redis, MongoDB, Neo4j, Firestore — every backend proven by one conformance suite; per-thread versions refuse a second writer with `GraphError::CheckpointConflict` instead of interleaving
- **Durability modes**: `Sync` (default), `Async`, or `Exit` per run; a step with a declared `NonIdempotent` node writes earlier checkpoints first
- **Pending writes**: when one node of a parallel step fails, the finished nodes are not run again on resume
- **Durable Resume**: Automatically resume from the last checkpoint after a crash — skips nodes of every checkpointed step. Execution is at-least-once: a crash mid-step re-runs that step's nodes; see [Execution Guarantees](#execution-guarantees)
- **Human-in-the-Loop**: Interrupt before/after nodes, dynamic interrupts. A pause is resumable, including one raised inside a subgraph. Every pause of a step reaches the caller, and `NodeContext::resume_value` hands a paused node the answer to that pause alone
- **Subgraphs**: run a compiled graph as a node with mapped channels (`SubgraphNode`). A pause inside pauses the parent, and a channel mapping that names a channel neither side declares fails when the parent **compiles**
- **Routing from inside a node**: `NodeOutput::with_goto` names successors and replaces the node's declared edges; `AgentNode::with_goto_mapper` routes on what the agent answered; `with_goto_parent` hands control to a node of the parent graph
- **Reliability**: per-node retry with backoff and jitter that never exceeds `max_delay`, a concurrency bound, per-node timeouts, graph-wide `NodeDefaults`, and `with_node_error_handler` to recover once a retry budget is spent. A failure no handler recovers stops the step: queued nodes do not start and sibling retries stop waiting
- **Invoking a node directly**: `ctx.run_node_with` runs a node the graph has no edge to, sized from state, and records it so a resume does not pay for it twice; concurrent calls under one run id run the child once
- **As a tool**: `NodeTool` exposes a graph or a single node through the `Tool` trait, so an `LlmAgent` can call a whole graph
- **Bounded growth**: `RetentionPolicy` limits how many checkpoints a thread keeps, by count or age
- **Run records**: every checkpoint carries its run, the steps finished since the previous one (each node's start, duration, status, written keys and attempt) and a fork's lineage, so an observer reads a run's history from checkpoints alone; `CompiledGraph::shape` reports the graph's nodes, edges and entry nodes
- **Fork with edits** (feature: `time-travel`): `TimeTravelHandle::fork` branches a new thread from a checkpoint with channels overwritten, keeps waiting joins, and records the fork's lineage; the source thread is never written
- **Governed nodes**: `with_governed_node` puts a node's call on the governed path — approval, edits, the kill switch and the action ledger — so it runs at most once per call in its lineage, forks included; a run without an action ledger, or a graph without a checkpointer, does not run it
- **Strictness, opt-in**: `with_strict_channels` fails the run when a node writes a channel the schema does not declare
- **Streaming**: Five stream modes (values, updates, messages, custom, debug)
- **ADK Integration**: Full callback support, works with existing runners
- **Functional API** (feature: `functional`): Write workflows as async functions with automatic checkpointing
- **Typed State Reducers**: ReducedValue, UntrackedValue, MessagesValue containers
- **State Schema Validation**: Type-level validation at workflow boundaries
- **Proc Macros**: `#[entrypoint]` and `#[task]` (from `adk-rust-macros`) for zero-boilerplate workflow definition

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
adk-graph = "3.0"
adk-agent = "3.0"
adk-model = "3.0"
adk-core = "3.0"
anyhow = "1"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

### Basic Graph with AgentNode

The program needs a `GOOGLE_API_KEY`.

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Event};
use adk_graph::{
    agent::GraphAgent,
    edge::{END, START},
    node::{AgentNode, ExecutionConfig},
    state::State,
};
use adk_model::GeminiModel;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// Writes the text of the agent's final response to one state channel.
fn text_into(channel: &'static str) -> impl Fn(&[Event]) -> HashMap<String, Value> + Send + Sync {
    move |events| {
        let text = events
            .iter()
            .rev()
            .filter(|event| !event.llm_response.partial)
            .find_map(Event::content)
            .map(|content| content.parts.iter().filter_map(|part| part.text()).collect::<String>())
            .unwrap_or_default();
        HashMap::from([(channel.to_string(), json!(text))])
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

    // Create LLM agents
    let translator = LlmAgentBuilder::new("translator")
        .model(model.clone())
        .instruction("Translate the input text to French. Only output the translation.")
        .build()?;
    let summarizer = LlmAgentBuilder::new("summarizer")
        .model(model.clone())
        .instruction("Summarize the input text in one sentence.")
        .build()?;

    // Create AgentNodes with input/output mappers; each node is named after its agent
    let input = |state: &State| {
        Content::new("user").with_text(state.get("input").and_then(Value::as_str).unwrap_or(""))
    };
    let translator_node = AgentNode::new(Arc::new(translator))
        .with_input_mapper(input)
        .with_output_mapper(text_into("translation"));
    let summarizer_node = AgentNode::new(Arc::new(summarizer))
        .with_input_mapper(input)
        .with_output_mapper(text_into("summary"));

    // Build graph with parallel execution
    let agent = GraphAgent::builder("text_processor")
        .description("Translates and summarizes text in parallel")
        .channels(&["input", "translation", "summary"])
        .node(translator_node)
        .node(summarizer_node)
        .edge(START, "translator")
        .edge(START, "summarizer") // Both start in parallel
        .edge("translator", END)
        .edge("summarizer", END)
        .build()?;

    // Execute
    let mut input = State::new();
    input.insert("input".to_string(), json!("AI is transforming how we work and live."));

    let result = agent.invoke(input, ExecutionConfig::new("thread-1")).await?;

    println!("Translation: {}", result.get("translation").and_then(Value::as_str).unwrap_or(""));
    println!("Summary: {}", result.get("summary").and_then(Value::as_str).unwrap_or(""));
    Ok(())
}
```

### Conditional Routing with LLM Classification

```rust
use adk_core::{Agent, Content};
use adk_graph::{
    edge::{END, Router, START},
    error::Result,
    graph::{CompiledGraph, StateGraph},
    node::{AgentNode, NodeOutput},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// `classifier` is an `LlmAgent` instructed to reply with one word:
/// positive, negative, or neutral.
fn sentiment_graph(classifier: Arc<dyn Agent>) -> Result<CompiledGraph> {
    let classifier_node = AgentNode::new(classifier)
        .with_input_mapper(|state| {
            let msg = state.get("message").and_then(Value::as_str).unwrap_or("");
            Content::new("user").with_text(format!("Classify: {msg}"))
        })
        .with_output_mapper(|events| {
            let text = events
                .iter()
                .rev()
                .filter(|event| !event.llm_response.partial)
                .find_map(|event| event.content())
                .map(|content| content.parts.iter().filter_map(|part| part.text()).collect::<String>())
                .unwrap_or_default()
                .to_lowercase();
            let sentiment = if text.contains("positive") {
                "positive"
            } else if text.contains("negative") {
                "negative"
            } else {
                "neutral"
            };
            HashMap::from([("sentiment".to_string(), json!(sentiment))])
        });

    let reply = |text: &'static str| {
        move |_ctx| async move { Ok(NodeOutput::new().with_update("response", json!(text))) }
    };

    // The classifier node is named after its agent
    StateGraph::with_channels(&["message", "sentiment", "response"])
        .add_node(classifier_node)
        .add_node_fn("positive_handler", reply("Glad to hear it."))
        .add_node_fn("negative_handler", reply("Sorry about that."))
        .add_node_fn("neutral_handler", reply("Thanks for the note."))
        .add_edge(START, "classifier")
        .add_conditional_edges(
            "classifier",
            Router::by_field("sentiment"), // Route based on "sentiment" field
            [
                ("positive", "positive_handler"),
                ("negative", "negative_handler"),
                ("neutral", "neutral_handler"),
            ],
        )
        .add_edge("positive_handler", END)
        .add_edge("negative_handler", END)
        .add_edge("neutral_handler", END)
        .compile()
}
```

### Human-in-the-Loop with Risk Assessment

```rust
use adk_core::Agent;
use adk_graph::{
    checkpoint::MemoryCheckpointer,
    edge::{END, START},
    error::GraphError,
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig, NodeOutput},
    state::State,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

async fn run(planner: Arc<dyn Agent>, input: State) -> adk_graph::Result<()> {
    let checkpointer = Arc::new(MemoryCheckpointer::new());

    // The planner agent (named "planner") assesses risk
    let planner_node = AgentNode::new(planner).with_output_mapper(|events| {
        let text: String = events
            .iter()
            .rev()
            .filter(|event| !event.llm_response.partial)
            .find_map(|event| event.content())
            .map(|content| content.parts.iter().filter_map(|part| part.text()).collect::<String>())
            .unwrap_or_default();
        let lower = text.to_lowercase();
        let risk = if lower.contains("risk: high") {
            "high"
        } else if lower.contains("risk: medium") {
            "medium"
        } else {
            "low"
        };
        HashMap::from([
            ("plan".to_string(), json!(text)),
            ("risk_level".to_string(), json!(risk)),
        ])
    });

    let graph = StateGraph::with_channels(&["task", "plan", "risk_level", "approved", "result"])
        .add_node(planner_node)
        .add_node_fn("review", |ctx| async move {
            // Resuming this pause: the reviewer's answer, which is not kept in state
            if let Some(answer) = ctx.resume_value() {
                return Ok(NodeOutput::new().with_update("approved", json!(answer == "approve")));
            }

            let risk = ctx.get("risk_level").and_then(Value::as_str).unwrap_or("low");
            if risk == "high" || risk == "medium" {
                // Interrupt for human approval
                return Ok(NodeOutput::interrupt_with_data(
                    "Human approval required",
                    json!({ "risk_level": risk, "action": "Resume with \"approve\"" }),
                ));
            }

            // Auto-approve low risk
            Ok(NodeOutput::new().with_update("approved", json!(true)))
        })
        .add_node_fn("executor", |ctx| async move {
            let approved = ctx.get("approved").and_then(Value::as_bool).unwrap_or(false);
            Ok(NodeOutput::new().with_update("result", json!(if approved { "done" } else { "skipped" })))
        })
        .add_edge(START, "planner")
        .add_edge("planner", "review")
        .add_edge("review", "executor")
        .add_edge("executor", END)
        .compile()?
        .with_checkpointer_arc(checkpointer.clone());

    // Execute - may pause for approval
    match graph.invoke(input, ExecutionConfig::new("task-001")).await {
        Err(GraphError::Interrupted(interrupted)) => {
            println!("Paused: {}", interrupted.interrupt);

            // Human reviews and approves; the answer goes to the node that paused
            let config =
                ExecutionConfig::new("task-001").with_resume_value("review", json!("approve"));
            let final_state = graph.invoke(State::new(), config).await?;
            println!("Completed: {:?}", final_state.get("result"));
        }
        Ok(state) => println!("Completed: {:?}", state.get("result")),
        Err(e) => return Err(e),
    }
    Ok(())
}
```

`GraphError::Interrupted` carries an `InterruptedExecution`: its `interrupt` is the first pause
and `interrupts` holds every pause of the step, in node-name order.

### ReAct Agent with Tools

```rust
use adk_core::{Agent, Part};
use adk_graph::{
    edge::{END, START},
    error::Result,
    graph::{CompiledGraph, StateGraph},
    node::{AgentNode, NodeOutput},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// `reasoner` is an `LlmAgent` (named "reasoner") with tools.
fn react_graph(reasoner: Arc<dyn Agent>) -> Result<CompiledGraph> {
    let reasoner_node = AgentNode::new(reasoner).with_output_mapper(|events| {
        let has_tool_calls = events
            .iter()
            .filter_map(|event| event.content())
            .flat_map(|content| content.parts.iter())
            .any(|part| matches!(part, Part::FunctionCall { .. }));
        HashMap::from([("has_tool_calls".to_string(), json!(has_tool_calls))])
    });

    // Build ReAct graph with cycle
    StateGraph::with_channels(&["input", "has_tool_calls", "iteration"])
        .add_node(reasoner_node)
        .add_node_fn("counter", |ctx| async move {
            let i = ctx.get("iteration").and_then(Value::as_i64).unwrap_or(0);
            Ok(NodeOutput::new().with_update("iteration", json!(i + 1)))
        })
        .add_edge(START, "counter")
        .add_edge("counter", "reasoner")
        .add_conditional_edges(
            "reasoner",
            |state| {
                let has_tools = state.get("has_tool_calls").and_then(Value::as_bool).unwrap_or(false);
                let iteration = state.get("iteration").and_then(Value::as_i64).unwrap_or(0);
                if iteration >= 5 || !has_tools { END.to_string() } else { "counter".to_string() }
            },
            [("counter", "counter"), (END, END)],
        )
        .compile()
}
```

An `LlmAgent` already runs its own tool loop inside one node; a graph cycle like this one fits
when each reasoning step must be checkpointed or routed.

## Node Types

### AgentNode

Wraps any ADK `Agent` (typically `LlmAgent`) as a graph node named after the agent:

```rust
use adk_core::{Agent, Content};
use adk_graph::node::AgentNode;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

fn node(agent: Arc<dyn Agent>) -> AgentNode {
    AgentNode::new(agent)
        .with_input_mapper(|state| {
            // Transform graph state to agent input Content
            Content::new("user").with_text(state.get("input").and_then(Value::as_str).unwrap_or(""))
        })
        .with_output_mapper(|events| {
            // Transform agent events to state updates
            HashMap::from([("turns".to_string(), json!(events.len()))])
        })
}
```

Without mappers, the input is the last `messages` entry or the `input` channel, and the output is
the agent's text.

### FunctionNode

Simple async functions for data processing:

```rust
use adk_graph::{graph::StateGraph, node::NodeOutput};
use serde_json::{Value, json};

fn graph() -> StateGraph {
    StateGraph::with_channels(&["data", "result"]).add_node_fn("process", |ctx| async move {
        let words = ctx.get("data").and_then(Value::as_str).unwrap_or("").split_whitespace().count();
        Ok(NodeOutput::new().with_update("result", json!(words)))
    })
}
```

### SubgraphNode

Run a compiled graph as a node. The inner graph keeps its own channels, edges and
interrupt gates, and exchanges named channels with its parent.

```rust
use adk_graph::{
    edge::{END, START},
    error::Result,
    graph::{CompiledGraph, StateGraph},
    subgraph::SubgraphNode,
};
use std::sync::Arc;

/// `inner` reads `text` and writes `length`.
fn outer(inner: CompiledGraph) -> Result<CompiledGraph> {
    StateGraph::with_channels(&["document", "size"])
        .add_node(
            SubgraphNode::new("measure", Arc::new(inner))
                .with_input("document", "text") // parent -> subgraph
                .with_output("length", "size"), // subgraph -> parent
        )
        .add_edge(START, "measure")
        .add_edge("measure", END)
        .compile()
}
```

Channels both schemas declare under one name pass through; `isolated()` requires
every exchange to be named. Each activation runs on its own thread,
`<parent thread>/<node name>@<parent step>`, so a later pass of a loop or a later turn
runs the subgraph again. A pause inside pauses the parent and resumes without
re-running finished work. A mapping naming a channel neither side declares fails at
`compile()`, as does a subgraph with an interrupt gate but no checkpointer — that
one would re-enter at its first node and pay for its work twice. A failure of an agent
inside names its path, such as `drafting.writer`.

A node inside can hand control to its parent:

```rust
use adk_graph::node::NodeOutput;
use serde_json::json;

fn escalate() -> NodeOutput {
    NodeOutput::new().with_update("reason", json!("no confident answer")).with_goto_parent(["escalate"])
}
```

## Routing From Inside a Node

A conditional edge fixes its targets when the graph is built. A goto does not, and
may name any node in the graph:

```rust
use adk_core::Agent;
use adk_graph::node::{AgentNode, NodeOutput};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

// A plain node.
fn route(level: &str, next: &str) -> NodeOutput {
    NodeOutput::new().with_update("risk", json!(level)).with_goto([next])
}

// An LLM-backed node, routing on what it answered.
fn desk_router(classifier: Arc<dyn Agent>) -> AgentNode {
    AgentNode::new(classifier)
        .with_output_mapper(|events| {
            let text = events
                .iter()
                .rev()
                .filter(|event| !event.llm_response.partial)
                .find_map(|event| event.content())
                .map(|content| content.parts.iter().filter_map(|part| part.text()).collect::<String>())
                .unwrap_or_default();
            HashMap::from([("category".to_string(), json!(text.trim().to_lowercase()))])
        })
        .with_goto_mapper(|updates| match updates.get("category").and_then(Value::as_str) {
            Some("refund") => Some(vec!["refund_desk".to_string()]),
            Some(_) => Some(vec!["general_desk".to_string()]),
            None => None,
        })
}
```

A goto **replaces** that node's declared edges. Naming `END` stops the branch, and
an unknown name fails the run with `GraphError::UnknownRouteTarget`.

## Reliability and Limits

Each of these is off by default.

```rust
use adk_core::ToolEffect;
use adk_graph::{
    checkpoint::RetentionPolicy,
    graph::{CompiledGraph, NodeDefaults},
    node::NodeOutput,
    retry::RetryPolicy,
};
use serde_json::json;

fn harden(graph: CompiledGraph) -> CompiledGraph {
    graph
        // Every node retries three times unless it says otherwise.
        .with_node_defaults(NodeDefaults::new().with_retry(RetryPolicy::new(3)))
        // This one gets ten.
        .with_node_retry("call_model", RetryPolicy::new(10))
        // At most four nodes at once, so a wide fan-out cannot trip a rate limit.
        .with_max_concurrency(4)
        // Recover instead of ending the run, once the retry budget is spent.
        .with_node_error_handler("charge", |node, error, _state| {
            Ok(NodeOutput::new()
                .with_update("status", json!(format!("{node}: {error}")))
                .with_goto(["compensate"]))
        })
        // A step with this node writes earlier checkpoints first.
        .with_node_effect("charge", ToolEffect::NonIdempotent)
        // Keep a long-lived thread from growing without bound.
        .with_checkpoint_retention(RetentionPolicy::keep_last(50))
}
```

| Default | Value |
|---------|-------|
| Super-steps per run | 100 (`CompiledGraph::with_recursion_limit` or `GraphAgentBuilder::recursion_limit`; `ExecutionConfig::with_recursion_limit` overrides it for one run) |
| Retry when no policy is attached | one attempt |
| `RetryPolicy::default()` | ten attempts; 1s initial delay doubling to a 60s cap, with full jitter — the nine sleeps average about 151s in total |
| Concurrency | the whole frontier |
| Checkpoints kept | every one, until a retention policy is set |
| Durability | `Durability::Sync`: each super-step's checkpoint is written before the next starts |

An interrupt is never retried and never reaches an error handler: a pause is not a
failure.

## Invoking a Node Directly

When the number of sub-tasks comes from state rather than the graph's shape:

```rust
use adk_graph::{child::RunNodeOptions, node::NodeContext};
use serde_json::{Value, json};

async fn review(ctx: &NodeContext, aspect: &str) -> adk_graph::Result<Value> {
    ctx.run_node_with("reviewer", json!({ "aspect": aspect }), RunNodeOptions::with_run_id(aspect))
        .await
}
```

The target needs no edge. Each completed child is recorded under
`<parent>@<step>/<child>@<run_id>` while the parent's step runs, so a parent that is
retried or resumed is served the recorded answer instead of executing the child again —
which matters when the child costs a model call or charges a card. The record is
dropped once the step completes, so a parent re-entered by a loop runs its children
again. A child with a policy set by `with_node_retry` is retried under it.

## A Graph as a Tool

```rust
use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::Llm;
use adk_graph::{graph::CompiledGraph, tool::NodeTool};
use std::sync::Arc;

fn analyst(model: Arc<dyn Llm>, graph: CompiledGraph) -> adk_core::Result<LlmAgent> {
    let desk = NodeTool::for_graph(Arc::new(graph))
        .with_name("research_desk")
        .with_description("Ask a research question. Pass it as `topic`.");

    LlmAgentBuilder::new("analyst").model(model).tool(Arc::new(desk)).build()
}
```

The parameter schema is derived from the graph's channels, so the tool description
and the graph cannot drift apart. It reports itself long-running, so a graph that
pauses travels the existing tool-confirmation path. Each call runs on its own
checkpoint thread, `tool_call_thread_id(app, user, session, function_call_id)`, so a
second call runs the graph again. A graph failure reaches the agent as the graph's
`AdkError`.

## State Management

### Channels and Reducers

```rust
use adk_graph::{
    graph::StateGraph,
    state::{Reducer, StateSchema},
};

fn graph() -> StateGraph {
    let schema = StateSchema::builder()
        .channel("current") // Overwrite (default)
        .list_channel("messages") // Append to list
        .channel_with_reducer("count", Reducer::Sum) // Sum values
        .build();
    StateGraph::new(schema)
}
```

## Checkpointing

The SQLite checkpointer needs the `sqlite` feature.

```rust
use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer, SqliteCheckpointer};

async fn checkpointers() -> adk_graph::Result<()> {
    // Memory (development)
    let memory = MemoryCheckpointer::new();

    // SQLite; `mode=rwc` creates the file when it does not exist
    let sqlite = SqliteCheckpointer::new("sqlite:state.db?mode=rwc").await?;

    // View checkpoint history, oldest version first
    for cp in sqlite.list("thread-id").await? {
        println!("v{} step {}: {:?}", cp.version, cp.step, cp.state);
    }
    Ok(())
}
```

The shared-database checkpointers are `PostgresCheckpointer`, `RedisCheckpointer`,
`MongoCheckpointer`, `Neo4jCheckpointer`, and `FirestoreCheckpointer`, each behind the feature of
the same name. Every checkpoint carries a per-thread `version`: `save` refuses a version that is
not greater than every stored version of the thread with `GraphError::CheckpointConflict`
(`graph.checkpoint.conflict`), and `load` and `list` order by version.

### Durable Resume

When a checkpointer is configured, the executor automatically checks for existing checkpoints before starting. If a checkpoint exists for the thread ID, execution resumes from where it left off — skipping already-completed nodes:

```rust
use adk_graph::prelude::*;
use std::sync::Arc;

async fn run() -> adk_graph::Result<State> {
    let checkpointer = Arc::new(MemoryCheckpointer::default());

    let graph = GraphAgent::builder("resilient_workflow")
        .description("Workflow that survives crashes")
        .node_fn("step_a", |_ctx| async move { Ok(NodeOutput::new().with_update("a_done", json!(true))) })
        .node_fn("step_b", |_ctx| async move { Ok(NodeOutput::new().with_update("b_done", json!(true))) })
        .edge(START, "step_a")
        .edge("step_a", "step_b")
        .edge("step_b", END)
        .checkpointer_arc(checkpointer)
        .build()?;

    // First run — completes step_a, saves checkpoint, then crashes before step_b
    // Second run — resumes from checkpoint, skips step_a, runs only step_b
    let config = ExecutionConfig::new("my-thread").with_durability(Durability::Sync);
    graph.invoke(State::new(), config).await
}
```

When streaming, a `StreamEvent::Resumed` event is emitted to indicate execution was restored from a checkpoint.

A checkpoint records the frontier still to run and the arrivals a fan-in node is
waiting on. A dynamic or tool-confirmation pause saves the successors of every node
that completed in the step plus the paused node, so a sibling's downstream work
still runs after the resume.

### Execution Guarantees

Node execution is **at-least-once**. A super-step runs its nodes and applies their
updates before it saves its checkpoint:

| Event | Effect on resume |
|-------|------------------|
| Crash while a super-step is running | Every node of that step runs again, including nodes that had finished |
| A node of a super-step fails | No queued node of the step starts; finished nodes are kept as pending writes; a resume runs only the nodes that did not finish |
| Pause or completed step | No node of a checkpointed step runs again |

Make a node with external side effects idempotent, for example with an idempotency
key derived from the thread id and step.

### GraphAgent Turns

`GraphAgent::run` keys its thread by `session_thread_id(app_name, user_id,
session_id)` — the three parts joined with `:`, with `%` and `:` percent-encoded inside each —
so two users who choose the same session id never share checkpoints, and
each call is a turn. A
paused thread resumes; a finished thread runs again from its entry nodes, starting
from the previous turn's final state with the new input merged on top. The recursion
limit applies per turn. `invoke` keeps the direct-graph behaviour, where a finished
thread returns its final state. A turn that resumes a paused node answers it: the node
reads the turn's text with `ctx.resume_value()`, or what
`GraphAgentBuilder::resume_mapper` returns.

A turn yields the agent nodes' events before the output mapper's, and every event it builds
carries the turn's invocation id. A step with several pauses yields one event per pause; read
each with `GraphInterruptPayload::from_event`.

### Run Records, Forks and Governed Nodes

| Capability | API | Where it lives |
|------------|-----|----------------|
| Graph shape | `CompiledGraph::shape()` → `GraphShape { nodes, edges, entry }` | Computed from the compiled graph |
| Run and step records | `Checkpoint::run_record()`, `step_records()`, `lineage()` | Checkpoint metadata `adk.graph.run`, `adk.graph.steps`, `adk.graph.lineage` |
| Fork with edits | `TimeTravelHandle::fork(ForkSpec)`, `time_travel::fork_thread` | A new thread whose lineage names its root, parent and parent checkpoint |
| Governed nodes | `CompiledGraph::with_governed_node`, `GraphAgentBuilder::governed_node`, `Node::call_args`, `Node::run_call` | The approval store, the action ledger and the effect log `{root}:effects` |

A pause checkpoint has the same step number as the completed checkpoint before it, so
checkpoints are addressed by id. A governed node's failures are `NodeExecutionFailed`
messages that start with a stable code (`graph.not_run`, `graph.refused`, …), read back
with `GovernedError::code_of`. See
[Graph Checkpointing](../docs/official_docs/agents/graph-checkpointing.md#run-and-step-records)
and [Graph Agents](../docs/official_docs/agents/graph-agents.md#governed-nodes).

## Examples

Standalone examples in this repository:

| Example | Shows |
|---------|-------|
| [`graph_runtime`](https://github.com/zavora-ai/adk-rust/tree/main/examples/graph_runtime) | Dispatch, streaming, retries, pauses, and cancellation under a `Runner`, over memory, SQLite, and Redis checkpointers |
| [`graph_correctness`](https://github.com/zavora-ai/adk-rust/tree/main/examples/graph_correctness) | Joins after conditional branches and loops, agent errors, subgraphs, `run_node` children, and graph tools |
| [`coding_graph`](https://github.com/zavora-ai/adk-rust/tree/main/examples/coding_graph) | Implement, parallel reviewers, synthesize, and a revise loop |
| [`graph_goto_routing`](https://github.com/zavora-ai/adk-rust/tree/main/examples/graph_goto_routing) | An `LlmAgent` classifier that names the next node |
| [`graph_subgraph_claims`](https://github.com/zavora-ai/adk-rust/tree/main/examples/graph_subgraph_claims) | A claim pipeline built from nested graphs |
| [`graph_resilient_research`](https://github.com/zavora-ai/adk-rust/tree/main/examples/graph_resilient_research) | A graph exposed to an `LlmAgent` as a tool |
| [`competitive_graph_resume`](https://github.com/zavora-ai/adk-rust/tree/main/examples/competitive_graph_resume) | Resume from a checkpoint |
| [`functional_workflow`](https://github.com/zavora-ai/adk-rust/tree/main/examples/functional_workflow) | The Functional API, with no API key |

```bash
cargo run --manifest-path examples/functional_workflow/Cargo.toml
```

## Comparison with LangGraph

| Feature | LangGraph | adk-graph |
|---------|-----------|-----------|
| State Management | TypedDict + Reducers | StateSchema + Reducers |
| Execution Model | Pregel super-steps | Pregel super-steps |
| Checkpointing | Memory, SQLite, Postgres, Redis, MongoDB | Memory, SQLite, PostgreSQL, Redis, MongoDB, Neo4j, Firestore |
| Human-in-Loop | interrupt_before/after | interrupt_before/after + dynamic |
| Streaming | 5 modes | 5 modes |
| Cycles | Native support | Native support |
| Type Safety | Python typing | Rust type system |
| LLM Integration | LangChain | AgentNode + ADK agents |

## Feature Flags

No feature is enabled by default.

| Flag | Description |
|------|-------------|
| `sqlite` | Enable SQLite checkpointer |
| `postgres`, `redis`, `mongodb`, `neo4j`, `firestore` | Checkpointers for shared databases |
| `conformance` | `CheckpointerHarness` and `checkpointer_conformance_tests!` for testing a backend |
| `functional` | Functional API: TaskContext, typed reducers, schema validation, cancellation |
| `node-cache` | Per-node result caching; `redis-cache` adds a Redis store |
| `delta-checkpoint` | Store the difference between super-steps instead of full state |
| `time-travel` | Step, fork, and state history |
| `action` | `ActionNodeExecutor` for `adk-action` node configurations |
| `action-http`, `action-rss`, `action-trigger` | HTTP and notification nodes, RSS feeds, cron/webhook trigger runtime |
| `action-db`, `action-db-mongo`, `action-db-redis`, `action-email`, `action-code` | Placeholders only — see [Action Nodes](#action-nodes) |
| `action-full` | `action`, `action-trigger`, `action-http`, `action-db`, `action-code`, `action-email`, `action-rss` |
| `full` | `sqlite` and `action-full` |

## Action Nodes

`ActionNodeExecutor` runs an `adk-action` configuration as a graph node.

| Node type | Status |
|-----------|--------|
| Trigger, Set, Transform, Switch, Loop, Merge, Wait, File | Implemented |
| Code with `language: rust` | Implemented (evaluates JSON or interpolates text) |
| HTTP, Notification (`action-http`), RSS (`action-rss`) | Implemented |
| Database, Email | **Not implemented** in any feature configuration; rejected when the graph compiles |
| Code with `language: javascript` or `typescript` | **Not implemented**; rejected when the graph compiles |

Paths and URLs come from workflow state, so both are confined:

- **File** nodes stay inside the allowed roots — the working directory unless
  `with_file_roots` names others. A `..` component, or a path that resolves outside
  every root through a symbolic link, fails the node.
- **HTTP**, **notification**, and **RSS** nodes accept only the schemes, hosts, and
  redirect count an `HttpActionPolicy` permits (`with_http_policy`). The default allows
  `https` and `http` to any host and up to 5 redirects, each re-checked. Logs omit the
  query string, and the notification webhook URL is logged by host only.

## SQLx compatibility

ADK-Rust 3.0 SQL backends use SQLx 0.9. SQLx types appear in this crate's public
API — `SqliteCheckpointer::from_pool` and `PostgresCheckpointer::from_pool` — so an application that passes in its own pool must depend on
SQLx 0.9 too. Applications that let ADK open its own connections need no SQLx
dependency. The [SQLx 0.9 migration guide](../docs/official_docs/migration/sqlx-0.9.md)
lists who is affected and what changed, and its
[TLS behaviour](../docs/official_docs/migration/sqlx-0.9.md#tls-behaviour) section
covers PostgreSQL certificate verification.

## Functional API

The Functional API (feature: `functional`) provides a higher-level programming model that lets you write agent workflows as normal async Rust functions with automatic checkpointing, typed state reducers, and interrupt/resume support.

### Quick Start

```toml
[dependencies]
adk-graph = { version = "3.0", features = ["functional"] }
adk-rust-macros = "3.0"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

`#[task]` generates a `__task_<name>` wrapper that records the call in the run's
`ExecutionLog`; call the wrapper so a resumed run replays the recorded result.
`#[entrypoint]` generates `<Name>Agent` with `new(checkpointer)` and `invoke(state, config)`.

```rust
use adk_graph::{
    checkpoint::{Checkpointer, MemoryCheckpointer},
    error::Result,
    functional::TaskContext,
    node::ExecutionConfig,
    state::State,
};
use adk_rust_macros::{entrypoint, task};
use serde_json::{Value, json};
use std::sync::Arc;

#[task]
async fn fetch_order(_ctx: &mut TaskContext, order_id: String) -> Result<Value> {
    Ok(json!({ "id": order_id, "total": 42 }))
}

#[task(retry(max_attempts = 3, backoff = "1s"))]
async fn charge(_ctx: &mut TaskContext, total: i64) -> Result<Value> {
    Ok(json!({ "charged": total }))
}

#[entrypoint]
async fn checkout(ctx: &mut TaskContext) -> Result<Value> {
    let order = __task_fetch_order(ctx, "order-7".to_string()).await?;
    let total = order["total"].as_i64().unwrap_or(0);
    let receipt = __task_charge(ctx, total).await?;
    ctx.set("receipt", receipt.clone());
    Ok(receipt)
}

#[tokio::main]
async fn main() -> Result<()> {
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let workflow = CheckoutAgent::new(checkpointer);
    let state = workflow.invoke(State::new(), ExecutionConfig::new("checkout-1")).await?;
    println!("{:?}", state.get("receipt"));
    Ok(())
}
```

The typed containers work on their own:

```rust
use adk_graph::functional::{ExecutionLog, ReducedValue, UntrackedValue};

fn main() {
    // ReducedValue — append-only accumulator persisted across checkpoints
    let mut results: ReducedValue<String> = ReducedValue::new();
    results.push("step 1 complete".to_string());
    results.push("step 2 complete".to_string());
    assert_eq!(results.len(), 2);

    // UntrackedValue — transient data excluded from checkpoints
    let mut scratch: UntrackedValue<Vec<u8>> = UntrackedValue::new();
    scratch.set(vec![1, 2, 3]);

    // ExecutionLog — resume-skip behavior
    let mut log = ExecutionLog::new();
    log.record_start("fetch_data");
    log.record_completion("fetch_data", serde_json::json!({ "ok": true }));
    assert!(log.is_completed("fetch_data")); // Skip on resume
}
```

### Features

| Type | Purpose |
|------|---------|
| `TaskContext` | Runtime context for tasks (state, checkpointing, streaming, interrupts) |
| `ReducedValue<T>` | Append-only accumulator persisted across checkpoints |
| `UntrackedValue<T>` | Transient data excluded from checkpoints |
| `MessagesValue` | Chat messages with ID-based deduplication |
| `StateSchemaValidator` | Type validation for state and task output |
| `ExecutionLog` | Task completion tracking for resume-skip, and recorded interrupt answers |
| `TypedReducer` | Custom merge strategies (`ReplaceReducer`, `AppendReducer`, `MergeReducer`) |

### Resume, Interrupts, and Cancellation

- **Resume from a checkpoint** — the generated `invoke` loads the checkpoint that
  `ExecutionConfig::with_resume_from(checkpoint_id)` names, and returns
  `GraphError::CheckpointError` without running a task when the thread has no checkpoint with
  that id. To continue where a thread left off, pass the id `Checkpointer::load(thread_id)`
  returns.
- **Interrupts** — `TaskContext::interrupt` suspends the run with `GraphError::Suspended`, which
  names the interrupt's `continuation_key`. Supply the answer with
  `ExecutionConfig::with_resume_value(continuation_key, value)` and invoke again. The first
  answer is recorded in the execution log and checkpointed, so a run resumed from any later
  checkpoint replays it.
- **Cancellation** — `ExecutionConfig::with_cancellation_token(token)`; cancelling the token
  stops the run, which returns `GraphError::Cancelled` and starts no further task.

A `#[task]` retry policy never retries a suspension or a cancellation.

### Task Identity

`#[task]` records each call of a task under its own key,
`{task}#{ordinal}:{arg_hash}` (`task_call_id`): the ordinal counts the task's calls
within the run, and the hash covers the arguments that implement `Serialize`. A
task called in a loop records one result per call, and a resumed run replays each
call's own result. A call whose arguments differ from the recorded call at the same
position runs again.

Tasks are **at-least-once**: a crash after a task's body finishes but before its
completion is checkpointed runs it again on resume.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
