# Graph Agents

Build complex, stateful workflows using LangGraph-style orchestration with native ADK-Rust integration.

## Overview

GraphAgent allows you to define workflows as directed graphs with nodes and edges, supporting:

- **AgentNode**: Wrap LLM agents as graph nodes with custom input/output mappers
- **Cyclic Workflows**: Native support for loops and iterative reasoning (ReAct pattern)
- **Conditional Routing**: Dynamic edge routing based on state
- **State Management**: Typed state with reducers (overwrite, append, sum, custom)
- **Checkpointing**: Persistent state for fault tolerance and human-in-the-loop
- **Streaming**: Multiple stream modes (values, updates, messages, debug)

The `adk-graph` crate provides LangGraph-style workflow orchestration for building complex, stateful agent workflows. It brings graph-based workflow capabilities to the ADK-Rust ecosystem while maintaining full compatibility with ADK's agent system.

**Key Benefits:**
- **Visual Workflow Design**: Define complex logic as intuitive node-and-edge graphs
- **Parallel Execution**: Multiple nodes can run simultaneously for better performance
- **State Persistence**: Built-in checkpointing for fault tolerance and human-in-the-loop
- **LLM Integration**: Native support for wrapping ADK agents as graph nodes
- **Flexible Routing**: Static edges, conditional routing, and dynamic decision making

## What You'll Build

In this guide, you'll create a **Text Processing Pipeline** that runs translation and summarization in parallel:

```
                        ┌─────────────────────┐
       User Input       │                     │
      ────────────────▶ │       START         │
                        │                     │
                        └──────────┬──────────┘
                                   │
                   ┌───────────────┴───────────────┐
                   │                               │
                   ▼                               ▼
        ┌──────────────────┐            ┌──────────────────┐
        │   TRANSLATOR     │            │   SUMMARIZER     │
        │                  │            │                  │
        │  🇫🇷 French       │            │  📝 One sentence │
        │     Translation  │            │     Summary      │
        └─────────┬────────┘            └─────────┬────────┘
                  │                               │
                  └───────────────┬───────────────┘
                                  │
                                  ▼
                        ┌─────────────────────┐
                        │      COMBINE        │
                        │                     │
                        │  📋 Merge Results   │
                        └──────────┬──────────┘
                                   │
                                   ▼
                        ┌─────────────────────┐
                        │        END          │
                        │                     │
                        │   ✅ Complete       │
                        └─────────────────────┘
```

**Key Concepts:**
- **Nodes** - Processing units that perform work (LLM agents, functions, or custom logic)
- **Edges** - Control flow between nodes (static connections or conditional routing)
- **State** - Shared data that flows through the graph and persists between nodes
- **Parallel Execution** - Multiple nodes can run simultaneously for better performance

### Understanding the Core Components

**🔧 Nodes: The Workers**
Nodes are where the actual work happens. Each node can:
- **AgentNode**: Wrap an LLM agent to process natural language
- **Function Node**: Execute custom Rust code for data processing
- **Built-in Nodes**: Use predefined logic like counters or validators

Think of nodes as specialized workers in an assembly line - each has a specific job and expertise.

**🔀 Edges: The Flow Control**
Edges determine how execution moves through your graph:
- **Static Edges**: Direct connections (`A → B → C`)
- **Conditional Edges**: Dynamic routing based on state (`if sentiment == "positive" → positive_handler`)
- **Parallel Edges**: Multiple paths from one node (`START → [translator, summarizer]`)

Edges are like traffic signals and road signs that direct the flow of work.

**💾 State: The Shared Memory**
State is a key-value store that all nodes can read from and write to:
- **Input Data**: Initial information fed into the graph
- **Intermediate Results**: Output from one node becomes input for another
- **Final Output**: The completed result after all processing

State acts like a shared whiteboard where nodes can leave information for others to use.

**⚡ Parallel Execution: The Speed Boost**
When multiple edges leave a node, those target nodes run simultaneously:
- **Faster Processing**: Independent tasks run at the same time
- **Resource Efficiency**: Better utilization of CPU and I/O
- **Scalability**: Handle more complex workflows without linear slowdown

This is like having multiple workers tackle different parts of a job simultaneously instead of waiting in line.

---

## Quick Start

### 1. Create Your Project

```bash
cargo new graph_demo
cd graph_demo
```

Add dependencies to `Cargo.toml`:

```toml
[dependencies]
adk-graph = { version = "0.1", features = ["sqlite"] }
adk-agent = "0.1"
adk-model = "0.1"
adk-core = "0.1"
tokio = { version = "1", features = ["full"] }
dotenvy = "0.15"
serde_json = "1.0"
```

Create `.env` with your API key:

```bash
echo 'GOOGLE_API_KEY=your-api-key' > .env
```

### 2. Parallel Processing Example

Here's a complete working example that processes text in parallel:

```rust
use adk_agent::LlmAgentBuilder;
use adk_graph::{
    agent::GraphAgent,
    edge::{END, START},
    node::{AgentNode, ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create specialized LLM agents
    let translator_agent = Arc::new(
        LlmAgentBuilder::new("translator")
            .description("Translates text to French")
            .model(model.clone())
            .instruction("Translate the input text to French. Only output the translation.")
            .build()?,
    );

    let summarizer_agent = Arc::new(
        LlmAgentBuilder::new("summarizer")
            .description("Summarizes text")
            .model(model.clone())
            .instruction("Summarize the input text in one sentence.")
            .build()?,
    );

    // Wrap agents as graph nodes with input/output mappers
    let translator_node = AgentNode::new(translator_agent)
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

    let summarizer_node = AgentNode::new(summarizer_agent)
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

    // Build the graph with parallel execution
    let agent = GraphAgent::builder("text_processor")
        .description("Processes text with translation and summarization in parallel")
        .channels(&["input", "translation", "summary", "result"])
        .node(translator_node)
        .node(summarizer_node)
        .node_fn("combine", |ctx| async move {
            let translation = ctx.get("translation").and_then(|v| v.as_str()).unwrap_or("N/A");
            let summary = ctx.get("summary").and_then(|v| v.as_str()).unwrap_or("N/A");

            let result = format!(
                "=== Processing Complete ===\n\n\
                French Translation:\n{}\n\n\
                Summary:\n{}",
                translation, summary
            );

            Ok(NodeOutput::new().with_update("result", json!(result)))
        })
        // Parallel execution: both nodes start simultaneously
        .edge(START, "translator")
        .edge(START, "summarizer")
        .edge("translator", "combine")
        .edge("summarizer", "combine")
        .edge("combine", END)
        .build()?;

    // Execute the graph
    let mut input = State::new();
    input.insert("input".to_string(), json!("AI is transforming how we work and live."));

    let result = agent.invoke(input, ExecutionConfig::new("thread-1")).await?;
    println!("{}", result.get("result").and_then(|v| v.as_str()).unwrap_or(""));

    Ok(())
}
```

**Example Output:**
```
=== Processing Complete ===

French Translation:
L'IA transforme notre façon de travailler et de vivre.

Summary:
AI is revolutionizing work and daily life through technological transformation.
```

## How Graph Execution Works

### The Big Picture

Graph agents execute in **super-steps** - all ready nodes run in parallel, then the graph waits for all to complete before the next step:

```
Step 1: START ──┬──▶ translator (running)
                └──▶ summarizer (running)
                
                ⏳ Wait for both to complete...
                
Step 2: translator ──┬──▶ combine (running)
        summarizer ──┘
        
                ⏳ Wait for combine to complete...
                
Step 3: combine ──▶ END ✅
```

### State Flow Through Nodes

Each node can read from and write to the shared state:

```
┌─────────────────────────────────────────────────────────────────────┐
│ STEP 1: Initial state                                               │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   State: { "input": "AI is transforming how we work" }             │
│                                                                     │
│                              ↓                                      │
│                                                                     │
│   ┌──────────────────┐              ┌──────────────────┐           │
│   │   translator     │              │   summarizer     │           │
│   │  reads "input"   │              │  reads "input"   │           │
│   └──────────────────┘              └──────────────────┘           │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────────┐
│ STEP 2: After parallel execution                                    │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   State: {                                                          │
│     "input": "AI is transforming how we work",                     │
│     "translation": "L'IA transforme notre façon de travailler",    │
│     "summary": "AI is revolutionizing work through technology"     │
│   }                                                                 │
│                                                                     │
│                              ↓                                      │
│                                                                     │
│   ┌──────────────────────────────────────┐                         │
│   │           combine                    │                         │
│   │  reads "translation" + "summary"     │                         │
│   │  writes "result"                     │                         │
│   └──────────────────────────────────────┘                         │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────────┐
│ STEP 3: Final state                                                 │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   State: {                                                          │
│     "input": "AI is transforming how we work",                     │
│     "translation": "L'IA transforme notre façon de travailler",    │
│     "summary": "AI is revolutionizing work through technology",    │
│     "result": "=== Processing Complete ===\n\nFrench..."          │
│   }                                                                 │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### What Makes It Work

| Component | Role |
|-----------|------|
| `AgentNode` | Wraps LLM agents with input/output mappers |
| `input_mapper` | Transforms state → agent input `Content` |
| `output_mapper` | Transforms agent events → state updates |
| `channels` | Declares state fields the graph will use |
| `edge()` | Defines execution flow between nodes |
| `ExecutionConfig` | Provides thread ID for checkpointing |

---

## Conditional Routing with LLM Classification

Build smart routing systems where LLMs decide the execution path:

### Visual: Sentiment-Based Routing

```
                        ┌─────────────────────┐
       User Feedback    │                     │
      ────────────────▶ │    CLASSIFIER       │
                        │  🧠 Analyze tone    │
                        └──────────┬──────────┘
                                   │
                   ┌───────────────┼───────────────┐
                   │               │               │
                   ▼               ▼               ▼
        ┌──────────────────┐ ┌──────────────────┐ ┌──────────────────┐
        │   POSITIVE       │ │    NEGATIVE      │ │    NEUTRAL       │
        │                  │ │                  │ │                  │
        │  😊 Thank you!   │ │  😔 Apologize    │ │  😐 Ask more     │
        │     Celebrate    │ │     Help fix     │ │     questions    │
        └──────────────────┘ └──────────────────┘ └──────────────────┘
```

### Complete Example Code

```rust
use adk_agent::LlmAgentBuilder;
use adk_graph::{
    edge::{END, Router, START},
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig},
    state::State,
};
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create classifier agent
    let classifier_agent = Arc::new(
        LlmAgentBuilder::new("classifier")
            .description("Classifies text sentiment")
            .model(model.clone())
            .instruction(
                "You are a sentiment classifier. Analyze the input text and respond with \
                ONLY one word: 'positive', 'negative', or 'neutral'. Nothing else.",
            )
            .build()?,
    );

    // Create response agents for each sentiment
    let positive_agent = Arc::new(
        LlmAgentBuilder::new("positive")
            .description("Handles positive feedback")
            .model(model.clone())
            .instruction(
                "You are a customer success specialist. The customer has positive feedback. \
                Express gratitude, reinforce the positive experience, and suggest ways to \
                share their experience. Be warm and appreciative. Keep response under 3 sentences.",
            )
            .build()?,
    );

    let negative_agent = Arc::new(
        LlmAgentBuilder::new("negative")
            .description("Handles negative feedback")
            .model(model.clone())
            .instruction(
                "You are a customer support specialist. The customer has a complaint. \
                Acknowledge their frustration, apologize sincerely, and offer help. \
                Be empathetic. Keep response under 3 sentences.",
            )
            .build()?,
    );

    let neutral_agent = Arc::new(
        LlmAgentBuilder::new("neutral")
            .description("Handles neutral feedback")
            .model(model.clone())
            .instruction(
                "You are a customer service representative. The customer has neutral feedback. \
                Ask clarifying questions to better understand their needs. Be helpful and curious. \
                Keep response under 3 sentences.",
            )
            .build()?,
    );

    // Create AgentNodes with mappers
    let classifier_node = AgentNode::new(classifier_agent)
        .with_input_mapper(|state| {
            let text = state.get("feedback").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(text)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("")
                        .to_lowercase()
                        .trim()
                        .to_string();
                    
                    let sentiment = if text.contains("positive") { "positive" }
                        else if text.contains("negative") { "negative" }
                        else { "neutral" };
                    
                    updates.insert("sentiment".to_string(), json!(sentiment));
                }
            }
            updates
        });

    // Response nodes (similar pattern for each)
    let positive_node = AgentNode::new(positive_agent)
        .with_input_mapper(|state| {
            let text = state.get("feedback").and_then(|v| v.as_str()).unwrap_or("");
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
                    updates.insert("response".to_string(), json!(text));
                }
            }
            updates
        });

    // Build graph with conditional routing
    let graph = StateGraph::with_channels(&["feedback", "sentiment", "response"])
        .add_node(classifier_node)
        .add_node(positive_node)
        // ... add negative_node and neutral_node similarly
        .add_edge(START, "classifier")
        .add_conditional_edges(
            "classifier",
            Router::by_field("sentiment"),  // Route based on sentiment field
            [
                ("positive", "positive"),
                ("negative", "negative"),
                ("neutral", "neutral"),
            ],
        )
        .add_edge("positive", END)
        .add_edge("negative", END)
        .add_edge("neutral", END)
        .compile()?;

    // Test with different feedback
    let mut input = State::new();
    input.insert("feedback".to_string(), json!("Your product is amazing! I love it!"));

    let result = graph.invoke(input, ExecutionConfig::new("feedback-1")).await?;
    println!("Sentiment: {}", result.get("sentiment").and_then(|v| v.as_str()).unwrap_or(""));
    println!("Response: {}", result.get("response").and_then(|v| v.as_str()).unwrap_or(""));

    Ok(())
}
```

**Example Flow:**
```
Input: "Your product is amazing! I love it!"
       ↓
Classifier: "positive"
       ↓
Positive Agent: "Thank you so much for the wonderful feedback! 
                We're thrilled you love our product. 
                Would you consider leaving a review to help others?"
```

## ReAct Pattern: Reasoning + Acting

Build agents that can use tools iteratively to solve complex problems:

### Visual: ReAct Cycle

```
                        ┌─────────────────────┐
       User Question    │                     │
      ────────────────▶ │      REASONER       │
                        │  🧠 Think + Act     │
                        └──────────┬──────────┘
                                   │
                                   ▼
                        ┌─────────────────────┐
                        │   Has tool calls?   │
                        │                     │
                        └──────────┬──────────┘
                                   │
                   ┌───────────────┴───────────────┐
                   │                               │
                   ▼                               ▼
        ┌──────────────────┐            ┌──────────────────┐
        │       YES        │            │        NO        │
        │                  │            │                  │
        │  🔄 Loop back    │            │  ✅ Final answer │
        │     to reasoner  │            │      END         │
        └─────────┬────────┘            └──────────────────┘
                  │
                  └─────────────────┐
                                    │
                                    ▼
                        ┌─────────────────────┐
                        │      REASONER       │
                        │  🧠 Think + Act     │
                        │   (next iteration)  │
                        └─────────────────────┘
```

### Complete ReAct Example

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{Part, Tool};
use adk_graph::{
    edge::{END, START},
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::GeminiModel;
use adk_tool::FunctionTool;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create tools
    let weather_tool = Arc::new(FunctionTool::new(
        "get_weather",
        "Get the current weather for a location. Takes a 'location' parameter (city name).",
        |_ctx, args| async move {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("unknown");
            Ok(json!({
                "location": location,
                "temperature": "72°F",
                "condition": "Sunny",
                "humidity": "45%"
            }))
        },
    )) as Arc<dyn Tool>;

    let calculator_tool = Arc::new(FunctionTool::new(
        "calculator",
        "Perform mathematical calculations. Takes an 'expression' parameter (string).",
        |_ctx, args| async move {
            let expr = args.get("expression").and_then(|v| v.as_str()).unwrap_or("0");
            let result = match expr {
                "2 + 2" => "4",
                "10 * 5" => "50",
                "100 / 4" => "25",
                "15 - 7" => "8",
                _ => "Unable to evaluate",
            };
            Ok(json!({ "result": result, "expression": expr }))
        },
    )) as Arc<dyn Tool>;

    // Create reasoner agent with tools
    let reasoner_agent = Arc::new(
        LlmAgentBuilder::new("reasoner")
            .description("Reasoning agent with tools")
            .model(model.clone())
            .instruction(
                "You are a helpful assistant with access to tools. Use tools when needed to answer questions. \
                When you have enough information, provide a final answer without using more tools.",
            )
            .tool(weather_tool)
            .tool(calculator_tool)
            .build()?,
    );

    // Create reasoner node that detects tool usage
    let reasoner_node = AgentNode::new(reasoner_agent)
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
                            Part::FunctionCall { .. } => {
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

    // Build ReAct graph with cycle
    let graph = StateGraph::with_channels(&["question", "has_tool_calls", "response", "iteration"])
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
                    "counter".to_string()  // Loop back for more reasoning
                } else {
                    END.to_string()  // Done - final answer
                }
            },
            [("counter", "counter"), (END, END)],
        )
        .compile()?
        .with_recursion_limit(10);

    // Test the ReAct agent
    let mut input = State::new();
    input.insert("question".to_string(), json!("What's the weather in Paris and what's 15 + 25?"));

    let result = graph.invoke(input, ExecutionConfig::new("react-1")).await?;
    println!("Final answer: {}", result.get("response").and_then(|v| v.as_str()).unwrap_or(""));
    println!("Iterations: {}", result.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0));

    Ok(())
}
```

**Example Flow:**
```
Question: "What's the weather in Paris and what's 15 + 25?"

Iteration 1:
  Reasoner: "I need to get weather info and do math"
  → Calls get_weather(location="Paris") and calculator(expression="15 + 25")
  → has_tool_calls = true → Loop back

Iteration 2:
  Reasoner: "Based on the results: Paris is 72°F and sunny, 15 + 25 = 40"
  → No tool calls → has_tool_calls = false → END

Final Answer: "The weather in Paris is 72°F and sunny with 45% humidity. 
              And 15 + 25 equals 40."
```

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

// GraphAgent implements Agent trait - use with Launcher or Runner
// See adk-runner README for Runner configuration
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

**Previous**: [← Multi-Agent Systems](./multi-agent.md) | **Next**: [Realtime Agents →](./realtime-agents.md)
