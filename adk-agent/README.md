# adk-agent

Agent implementations for ADK-Rust (LLM, Custom, Workflow agents).

[![Crates.io](https://img.shields.io/crates/v/adk-agent.svg)](https://crates.io/crates/adk-agent)
[![Documentation](https://docs.rs/adk-agent/badge.svg)](https://docs.rs/adk-agent)
[![License](https://img.shields.io/crates/l/adk-agent.svg)](LICENSE)

## Overview

`adk-agent` provides ready-to-use agent implementations for [ADK-Rust](https://github.com/zavora-ai/adk-rust):

- `LlmAgent` — core agent powered by LLM reasoning with tools, callbacks, guardrails, and skills
- `CustomAgent` — define custom logic without LLM
- `SequentialAgent` — execute agents in sequence
- `ParallelAgent` — execute agents concurrently
- `LoopAgent` — iterate until exit condition or max iterations
- `ConditionalAgent` — branch based on a function condition
- `LlmConditionalAgent` — LLM-powered multi-way routing to sub-agents
- `LlmEventSummarizer` — LLM-based context compaction for long conversations

## Installation

```toml
[dependencies]
adk-agent = "0.5.0"
```

Or use the umbrella crate:

```toml
[dependencies]
adk-rust = { version = "0.5.0", features = ["agents"] }
```

## Quick Start

### LLM Agent

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use std::sync::Arc;

let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

let agent = LlmAgentBuilder::new("assistant")
    .description("Helpful AI assistant")
    .instruction("Be helpful and concise.")
    .model(model)
    .tool(Arc::new(calculator_tool))
    .build()?;
```

### LlmAgentBuilder Methods

| Method | Description |
|--------|-------------|
| `new(name)` | Create builder with agent name |
| `description(desc)` | Set agent description |
| `model(llm)` | Set the LLM model (required) |
| `instruction(text)` | Set static instruction |
| `instruction_provider(fn)` | Set dynamic instruction provider |
| `global_instruction(text)` | Set global instruction (shared across agents) |
| `global_instruction_provider(fn)` | Set dynamic global instruction provider |
| `generate_content_config(config)` | Set full `GenerateContentConfig` (temperature, top_p, etc.) |
| `temperature(f32)` | Shorthand for setting temperature only |
| `top_p(f32)` | Shorthand for setting top_p only |
| `top_k(i32)` | Shorthand for setting top_k only |
| `max_output_tokens(i32)` | Shorthand for setting max output tokens only |
| `with_skills(index)` | Attach a preloaded skills index |
| `with_auto_skills()` | Auto-load skills from `.skills/` in current directory |
| `with_skills_from_root(path)` | Auto-load skills from `.skills/` under a specific root |
| `with_skill_policy(policy)` | Configure matching policy (`top_k`, threshold, tags) |
| `with_skill_budget(chars)` | Cap injected skill content length (default: 2000) |
| `tool(tool)` | Add a static tool |
| `toolset(toolset)` | Add a dynamic toolset for per-invocation tool resolution |
| `sub_agent(agent)` | Add a sub-agent for transfers |
| `max_iterations(n)` | Set maximum LLM round-trips (default: 100) |
| `tool_timeout(duration)` | Set per-tool execution timeout (default: 5 min) |
| `default_retry_budget(budget)` | Set default retry policy for all tools |
| `tool_retry_budget(name, budget)` | Set retry policy for a specific tool |
| `circuit_breaker_threshold(n)` | Disable tools after N consecutive failures |
| `on_tool_error(callback)` | Add fallback handler for tool failures |
| `require_tool_confirmation(name)` | Require user confirmation for a specific tool |
| `require_tool_confirmation_for_all()` | Require user confirmation for all tools |
| `tool_confirmation_policy(policy)` | Set custom tool confirmation policy |
| `disallow_transfer_to_parent(bool)` | Prevent agent from transferring back to parent |
| `disallow_transfer_to_peers(bool)` | Prevent agent from transferring to sibling agents |
| `include_contents(mode)` | Control content inclusion in sub-agent context |
| `input_schema(json)` | Set input JSON schema |
| `output_schema(json)` | Set output JSON schema |
| `output_key(key)` | Set state key for output |
| `input_guardrails(set)` | Add input validation guardrails |
| `output_guardrails(set)` | Add output validation guardrails |
| `before_callback(fn)` | Add before-agent callback |
| `after_callback(fn)` | Add after-agent callback |
| `before_model_callback(fn)` | Add before-model callback |
| `after_model_callback(fn)` | Add after-model callback |
| `before_tool_callback(fn)` | Add before-tool callback |
| `after_tool_callback(fn)` | Add after-tool callback |
| `after_tool_callback_full(fn)` | Rich after-tool callback with tool, args, and response |
| `build()` | Build the LlmAgent |

### Generation Config

Control LLM generation parameters per-agent. Use the shorthand methods for common settings or provide a full config:

```rust
use adk_core::GenerateContentConfig;

// Shorthand
let agent = LlmAgentBuilder::new("creative")
    .model(model)
    .temperature(0.9)
    .max_output_tokens(4096)
    .build()?;

// Full config
let agent = LlmAgentBuilder::new("precise")
    .model(model)
    .generate_content_config(GenerateContentConfig {
        temperature: Some(0.2),
        top_p: Some(0.95),
        top_k: Some(40),
        max_output_tokens: Some(2048),
        ..Default::default()
    })
    .build()?;
```

### Skills

Skills are opt-in. No skill content is injected unless you call a skills method:

```rust
let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .with_auto_skills()? // loads .skills/**/*.md when present
    .build()?;
```

Skills are also supported on all workflow agents (`LoopAgent`, `SequentialAgent`, `ParallelAgent`, `ConditionalAgent`, `LlmConditionalAgent`).

### Workflow Agents

```rust
use adk_agent::{SequentialAgent, ParallelAgent, LoopAgent};
use std::sync::Arc;

// Sequential: A -> B -> C
let pipeline = SequentialAgent::new("pipeline", vec![
    agent_a.clone(),
    agent_b.clone(),
    agent_c.clone(),
]);

// Parallel: A, B, C simultaneously
let team = ParallelAgent::new("team", vec![
    analyst_a.clone(),
    analyst_b.clone(),
]);

// Loop: repeat until exit or max iterations
let iterator = LoopAgent::new("iterator", vec![worker.clone()])
    .with_max_iterations(10);
// Default max iterations is 1000 (DEFAULT_LOOP_MAX_ITERATIONS)
```

All workflow agents support `.with_description()`, `.before_callback()`, `.after_callback()`, and the full skills API (`with_skills`, `with_auto_skills`, `with_skill_policy`, `with_skill_budget`).

### Conditional Agents

```rust
use adk_agent::{ConditionalAgent, LlmConditionalAgent};

// Function-based condition
let conditional = ConditionalAgent::new(
    "router",
    |ctx| ctx
        .user_content()
        .parts
        .iter()
        .find_map(|part| part.text())
        .is_some_and(|text| text.contains("urgent")),
    urgent_agent,
).with_else(normal_agent);

// LLM-powered routing
let llm_router = LlmConditionalAgent::builder("smart_router", model)
    .instruction("Route to the appropriate specialist based on the query.")
    .route("support", support_agent)
    .route("sales", sales_agent)
    .default_route(fallback_agent)
    .build()?;
```

`LlmConditionalAgent` normalizes the LLM's classification to lowercase and does substring matching against route labels, so the LLM doesn't need to produce an exact match.

### Multi-Agent Systems

```rust
// Agent with sub-agents for transfer
let coordinator = LlmAgentBuilder::new("coordinator")
    .instruction("Route to appropriate specialist. Transfer when needed.")
    .model(model)
    .sub_agent(support_agent)
    .sub_agent(sales_agent)
    .build()?;
```

Control transfer behavior:

```rust
let agent = LlmAgentBuilder::new("leaf")
    .model(model)
    .disallow_transfer_to_parent(true)  // can't transfer back up
    .disallow_transfer_to_peers(true)   // can't transfer to siblings
    .build()?;
```

### Toolset Support

Use `.toolset()` for context-dependent tools that need per-invocation resolution — for example, per-user browser sessions from a pool. Toolsets are resolved at the start of each `run()` call using the invocation's `ReadonlyContext`.

```rust,ignore
use adk_agent::LlmAgentBuilder;
use adk_browser::{BrowserToolset, BrowserSessionPool, BrowserProfile};
use std::sync::Arc;

let pool = Arc::new(BrowserSessionPool::new(config, 10));

let browser_toolset = BrowserToolset::with_pool_and_profile(
    pool.clone(),
    BrowserProfile::Full,
);

let agent = LlmAgentBuilder::new("browser_agent")
    .description("Multi-tenant browser agent")
    .instruction("Help users browse the web.")
    .model(model)
    .toolset(Arc::new(browser_toolset))
    .build()?;
```

Static tools (`.tool()`) and dynamic toolsets (`.toolset()`) can be mixed on the same agent. Duplicate tool names across static tools and toolsets produce a deterministic error at resolution time.

### Retry Budget

Configure automatic retries for transient tool failures:

```rust,ignore
use adk_core::RetryBudget;
use std::time::Duration;

let agent = LlmAgentBuilder::new("resilient_agent")
    .model(model)
    .tool(Arc::new(my_tool))
    .default_retry_budget(RetryBudget::new(2, Duration::from_secs(1)))
    .tool_retry_budget("browser_navigate", RetryBudget::new(3, Duration::from_millis(500)))
    .build()?;
```

Per-tool budgets take precedence over the default. When no budget is configured, tools execute once.

### Circuit Breaker

Temporarily disable tools after repeated consecutive failures within an invocation:

```rust,ignore
let agent = LlmAgentBuilder::new("guarded_agent")
    .model(model)
    .toolset(Arc::new(browser_toolset))
    .circuit_breaker_threshold(5)
    .build()?;
```

After 5 consecutive failures for a given tool, the circuit breaker opens and returns an immediate error to the LLM without executing the tool. Resets at the start of each new invocation.

### Tool Error Callbacks

Register `on_tool_error` callbacks to provide fallback results when tools fail (after retries are exhausted):

```rust,ignore
let agent = LlmAgentBuilder::new("fallback_agent")
    .model(model)
    .tool(Arc::new(my_tool))
    .on_tool_error(Box::new(|ctx, tool, args, error| {
        Box::pin(async move {
            tracing::warn!(tool = tool.name(), error = %error, "tool failed");
            // Return Ok(Some(value)) to substitute a fallback result
            // Return Ok(None) to propagate the original error to the LLM
            Ok(None)
        })
    }))
    .build()?;
```

Multiple callbacks can be registered. They are tried in order — the first to return `Some(value)` wins.

### Rich After-Tool Callbacks

`after_tool_callback_full` receives the tool, arguments, and response value — aligned with the Python/Go ADK callback model. Return `Ok(None)` to keep the original response, or `Ok(Some(value))` to replace it:

```rust,ignore
let agent = LlmAgentBuilder::new("auditing_agent")
    .model(model)
    .after_tool_callback_full(Box::new(|ctx, tool, args, response| {
        Box::pin(async move {
            tracing::info!(tool = tool.name(), "tool completed");
            Ok(None) // keep original response
        })
    }))
    .build()?;
```

These run after the legacy `after_tool_callback` chain.

### Guardrails

```rust
use adk_agent::LlmAgentBuilder;
use adk_guardrail::{GuardrailSet, ContentFilter, PiiRedactor};

let input_guardrails = GuardrailSet::new()
    .with(ContentFilter::harmful_content())
    .with(PiiRedactor::new());

let agent = LlmAgentBuilder::new("safe_assistant")
    .model(model)
    .input_guardrails(input_guardrails)
    .build()?;
```

### Custom Agent

```rust
use adk_agent::CustomAgentBuilder;

let custom = CustomAgentBuilder::new("processor")
    .description("Custom data processor")
    .handler(|_ctx| async move {
        let mut event = Event::new("custom-invocation");
        event.author = "processor".to_string();
        event.llm_response.content = Some(Content::new("model").with_text("Processed!"));
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])) as adk_core::EventStream)
    })
    .build()?;
```

## Tool Call Markup Normalization

The `tool_call_markup` module handles LLMs that emit tool calls as text markup (e.g., `<tool_call>...</tool_call>`) instead of structured function calls. `normalize_content` parses these text blocks into proper `Part::FunctionCall` parts so the tool execution loop can handle them:

```rust
use adk_agent::normalize_content;

normalize_content(&mut content);
```

This is applied automatically inside `LlmAgent` — you only need it if building custom agent logic.

## Context Compaction

`LlmEventSummarizer` uses an LLM to summarize older conversation events, reducing context size for long-running sessions:

```rust
use adk_agent::LlmEventSummarizer;
use adk_core::EventsCompactionConfig;
use std::sync::Arc;

let summarizer = LlmEventSummarizer::new(model.clone());
// Optionally customize the prompt template:
// let summarizer = summarizer.with_prompt_template("Custom: {conversation_history}");

let compaction_config = EventsCompactionConfig {
    compaction_interval: 3,  // Compact every 3 invocations
    overlap_size: 1,         // Keep 1 event overlap for continuity
    summarizer: Arc::new(summarizer),
};
```

Pass `compaction_config` to `RunnerConfig` to enable automatic compaction.

## Features

| Feature | Description |
|---------|-------------|
| (default) | All agent types, callbacks, skills, toolsets, retry/circuit breaker |
| `guardrails` | Input/output guardrails via `adk-guardrail` |

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — Umbrella crate
- [adk-core](https://crates.io/crates/adk-core) — Core `Agent` trait
- [adk-model](https://crates.io/crates/adk-model) — LLM integrations
- [adk-tool](https://crates.io/crates/adk-tool) — Tool system
- [adk-guardrail](https://crates.io/crates/adk-guardrail) — Guardrails
- [adk-skill](https://crates.io/crates/adk-skill) — Skill discovery and injection

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
