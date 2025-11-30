# Workflow Agents

Workflow agents provide deterministic execution patterns for orchestrating multiple agents. Unlike LlmAgent which uses AI reasoning to decide actions, workflow agents follow predefined execution paths, making them ideal for structured pipelines and predictable multi-step processes.

## Overview

ADK-Rust provides three workflow agent types:

| Agent | Execution Pattern | Use Case |
|-------|------------------|----------|
| `SequentialAgent` | Runs sub-agents one after another | Multi-step pipelines, data transformation chains |
| `ParallelAgent` | Runs sub-agents concurrently | Independent analyses, fan-out processing |
| `LoopAgent` | Runs sub-agents repeatedly | Iterative refinement, retry logic |

All workflow agents implement the `Agent` trait and can be nested within each other or used as sub-agents of an LlmAgent.

## SequentialAgent

`SequentialAgent` executes sub-agents in order, passing context between them. Each agent sees the accumulated conversation history from previous agents.

### Basic Usage

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

// Create sub-agents for the pipeline
let analyzer = LlmAgentBuilder::new("analyzer")
    .description("Analyzes the input")
    .instruction("Analyze the given topic and identify key points.")
    .model(model.clone())
    .build()?;

let expander = LlmAgentBuilder::new("expander")
    .description("Expands on analysis")
    .instruction("Take the analysis and expand on each key point with details.")
    .model(model.clone())
    .build()?;

let summarizer = LlmAgentBuilder::new("summarizer")
    .description("Summarizes content")
    .instruction("Create a concise summary of the expanded analysis.")
    .model(model.clone())
    .build()?;

// Create sequential pipeline
let pipeline = SequentialAgent::new(
    "analysis_pipeline",
    vec![Arc::new(analyzer), Arc::new(expander), Arc::new(summarizer)],
);
```

### Configuration Options

```rust
// Add a description
let pipeline = SequentialAgent::new("pipeline", sub_agents)
    .with_description("A three-step analysis pipeline");

// Add callbacks for monitoring
let pipeline = SequentialAgent::new("pipeline", sub_agents)
    .before_callback(Arc::new(|ctx| {
        println!("Starting pipeline execution");
        Box::pin(async move { Ok(None) })
    }))
    .after_callback(Arc::new(|ctx, events| {
        println!("Pipeline completed");
        Box::pin(async move { Ok(None) })
    }));
```

### How It Works

1. The first sub-agent receives the initial user message
2. Each subsequent agent sees all previous messages and responses
3. The pipeline completes when the last agent finishes
4. All events from all agents are streamed in order

## ParallelAgent

`ParallelAgent` executes all sub-agents concurrently, collecting their results as they complete. This is useful when you need multiple independent perspectives or analyses.

### Basic Usage

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

// Create agents for parallel analysis
let technical = LlmAgentBuilder::new("technical_analyst")
    .description("Provides technical analysis")
    .instruction("Analyze the topic from a technical perspective.")
    .model(model.clone())
    .build()?;

let business = LlmAgentBuilder::new("business_analyst")
    .description("Provides business analysis")
    .instruction("Analyze the topic from a business perspective.")
    .model(model.clone())
    .build()?;

let user_exp = LlmAgentBuilder::new("ux_analyst")
    .description("Provides UX analysis")
    .instruction("Analyze the topic from a user experience perspective.")
    .model(model.clone())
    .build()?;

// Create parallel agent
let parallel = ParallelAgent::new(
    "multi_perspective_analysis",
    vec![Arc::new(technical), Arc::new(business), Arc::new(user_exp)],
);
```

### Configuration Options

```rust
// Add a description
let parallel = ParallelAgent::new("analysis", sub_agents)
    .with_description("Concurrent multi-perspective analysis");

// Add callbacks
let parallel = ParallelAgent::new("analysis", sub_agents)
    .before_callback(Arc::new(|ctx| {
        println!("Starting parallel execution");
        Box::pin(async move { Ok(None) })
    }));
```

### How It Works

1. All sub-agents start executing simultaneously
2. Each agent receives the same initial context
3. Results are streamed as each agent completes (order may vary)
4. The parallel agent completes when all sub-agents finish

### Use Cases

- **Multi-perspective analysis**: Get technical, business, and user perspectives simultaneously
- **Fan-out processing**: Process the same input through multiple specialized agents
- **Redundancy**: Run the same task on multiple agents and compare results

## LoopAgent

`LoopAgent` executes sub-agents repeatedly until a termination condition is met. This is ideal for iterative refinement, retry logic, or processes that need multiple passes.

### Basic Usage

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

// Create an agent that can exit the loop
let refiner = LlmAgentBuilder::new("refiner")
    .description("Iteratively refines content")
    .instruction(
        "Review and improve the content. \
         If the content is good enough, call the exit_loop tool. \
         Otherwise, provide an improved version."
    )
    .model(model.clone())
    .tool(Arc::new(ExitLoopTool::new()))
    .build()?;

// Create loop agent with max iterations
let loop_agent = LoopAgent::new(
    "iterative_refiner",
    vec![Arc::new(refiner)],
).with_max_iterations(5);
```

### Configuration Options

```rust
// Set maximum iterations (required for safety)
let loop_agent = LoopAgent::new("refiner", sub_agents)
    .with_max_iterations(10);

// Add a description
let loop_agent = LoopAgent::new("refiner", sub_agents)
    .with_description("Iteratively refines content until quality threshold")
    .with_max_iterations(5);

// Add callbacks
let loop_agent = LoopAgent::new("refiner", sub_agents)
    .with_max_iterations(5)
    .before_callback(Arc::new(|ctx| {
        println!("Starting loop iteration");
        Box::pin(async move { Ok(None) })
    }));
```

### How It Works

1. Sub-agents execute in sequence (like SequentialAgent)
2. After all sub-agents complete, the loop repeats
3. The loop terminates when:
   - An agent calls `exit_loop` tool (sets `escalate = true`)
   - Maximum iterations are reached
4. All events from all iterations are streamed

## ExitLoopTool

The `ExitLoopTool` is a built-in tool that allows agents to signal loop termination. When called, it sets the `escalate` flag on the event actions, causing the LoopAgent to exit.

### Usage

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

// Add ExitLoopTool to an agent
let agent = LlmAgentBuilder::new("refiner")
    .instruction(
        "Improve the content. When satisfied with the quality, \
         call the exit_loop tool to finish."
    )
    .model(model.clone())
    .tool(Arc::new(ExitLoopTool::new()))
    .build()?;
```

### Tool Details

| Property | Value |
|----------|-------|
| Name | `exit_loop` |
| Description | "Exits the loop. Call this function only when you are instructed to do so." |
| Parameters | None |
| Returns | Empty object `{}` |

The tool works by setting `actions.escalate = true` on the event, which signals to the LoopAgent that it should stop iterating.

## Combining Workflow Agents

Workflow agents can be nested to create complex execution patterns.

### Sequential with Parallel

```rust
// First, analyze from multiple perspectives in parallel
let parallel_analysis = ParallelAgent::new(
    "parallel_analysis",
    vec![Arc::new(technical), Arc::new(business)],
);

// Then, synthesize the results
let synthesizer = LlmAgentBuilder::new("synthesizer")
    .instruction("Combine the analyses into a unified recommendation.")
    .model(model.clone())
    .build()?;

// Create a sequential pipeline: parallel analysis -> synthesis
let pipeline = SequentialAgent::new(
    "analyze_and_synthesize",
    vec![Arc::new(parallel_analysis), Arc::new(synthesizer)],
);
```

### Loop with Sequential

```rust
// Create a critique-refine loop
let critic = LlmAgentBuilder::new("critic")
    .instruction("Critique the content and suggest improvements.")
    .model(model.clone())
    .build()?;

let refiner = LlmAgentBuilder::new("refiner")
    .instruction(
        "Apply the critique to improve the content. \
         Call exit_loop when no more improvements needed."
    )
    .model(model.clone())
    .tool(Arc::new(ExitLoopTool::new()))
    .build()?;

// Sequential critique-refine steps inside a loop
let critique_refine = SequentialAgent::new(
    "critique_refine_step",
    vec![Arc::new(critic), Arc::new(refiner)],
);

let iterative_improvement = LoopAgent::new(
    "iterative_improvement",
    vec![Arc::new(critique_refine)],
).with_max_iterations(3);
```

## Best Practices

### Setting Max Iterations

Always set `max_iterations` on LoopAgent to prevent infinite loops:

```rust
// Good: Always set a reasonable limit
let loop_agent = LoopAgent::new("refiner", agents)
    .with_max_iterations(5);

// The agent can still exit early via ExitLoopTool
```

### Clear Exit Conditions

When using LoopAgent, make the exit condition clear in the agent's instruction:

```rust
let refiner = LlmAgentBuilder::new("refiner")
    .instruction(
        "Review the content for quality. \
         If the content meets these criteria: \
         1. Clear and concise \
         2. Well-structured \
         3. No grammatical errors \
         Then call exit_loop. Otherwise, improve it."
    )
    .tool(Arc::new(ExitLoopTool::new()))
    .build()?;
```

### State Management

Use session state to pass data between agents in a workflow:

```rust
// First agent stores result in state
let analyzer = LlmAgentBuilder::new("analyzer")
    .instruction("Analyze the input. Store key findings in 'analysis' state key.")
    .output_key("analysis")  // Automatically stores output in state
    .model(model.clone())
    .build()?;

// Second agent reads from state via instruction templating
let reporter = LlmAgentBuilder::new("reporter")
    .instruction("Based on the analysis: {analysis}, create a report.")
    .model(model.clone())
    .build()?;
```

## API Reference

### SequentialAgent

```rust
impl SequentialAgent {
    /// Create a new sequential agent with sub-agents
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self;
    
    /// Add a description
    pub fn with_description(self, desc: impl Into<String>) -> Self;
    
    /// Add a before-agent callback
    pub fn before_callback(self, callback: BeforeAgentCallback) -> Self;
    
    /// Add an after-agent callback
    pub fn after_callback(self, callback: AfterAgentCallback) -> Self;
}
```

### ParallelAgent

```rust
impl ParallelAgent {
    /// Create a new parallel agent with sub-agents
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self;
    
    /// Add a description
    pub fn with_description(self, desc: impl Into<String>) -> Self;
    
    /// Add a before-agent callback
    pub fn before_callback(self, callback: BeforeAgentCallback) -> Self;
    
    /// Add an after-agent callback
    pub fn after_callback(self, callback: AfterAgentCallback) -> Self;
}
```

### LoopAgent

```rust
impl LoopAgent {
    /// Create a new loop agent with sub-agents
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self;
    
    /// Add a description
    pub fn with_description(self, desc: impl Into<String>) -> Self;
    
    /// Set maximum iterations (recommended)
    pub fn with_max_iterations(self, max: u32) -> Self;
    
    /// Add a before-agent callback
    pub fn before_callback(self, callback: BeforeAgentCallback) -> Self;
    
    /// Add an after-agent callback
    pub fn after_callback(self, callback: AfterAgentCallback) -> Self;
}
```

### ExitLoopTool

```rust
impl ExitLoopTool {
    /// Create a new exit loop tool
    pub fn new() -> Self;
}
```

## Related

- [LlmAgent](./llm-agent.md) - AI-powered agents that can use workflow agents as sub-agents
- [Multi-Agent Systems](./multi-agent.md) - Building agent hierarchies
- [Callbacks](../callbacks/callbacks.md) - Monitoring and customizing agent execution
