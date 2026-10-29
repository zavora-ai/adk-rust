# adk-agent

Agent implementations for ADK-Rust (LLM, Custom, Workflow agents).

[![Crates.io](https://img.shields.io/crates/v/adk-agent.svg)](https://crates.io/crates/adk-agent)
[![Documentation](https://docs.rs/adk-agent/badge.svg)](https://docs.rs/adk-agent)
[![License](https://img.shields.io/crates/l/adk-agent.svg)](../LICENSE)

## Overview

`adk-agent` provides ready-to-use agent implementations for [ADK-Rust](https://github.com/zavora-ai/adk-rust):

| Agent | Purpose |
|-------|---------|
| `LlmAgent` | Model-driven agent with tools, callbacks, guardrails, and skills |
| `CustomAgent` | Your own logic without a model |
| `SequentialAgent` | Runs sub-agents in order |
| `ParallelAgent` | Runs sub-agents concurrently, optionally sharing a `SharedState` |
| `LoopAgent` | Repeats sub-agents until one escalates or the iteration limit is reached |
| `ConditionalAgent` | Branches on a function of the invocation context |
| `LlmConditionalAgent` | Routes to one of several sub-agents by a model classification |
| `TeamSpec` / `CompiledTeam` | A portable, validated team topology compiled from existing agents |
| `CodeActAgent` | (feature `codeact`) Acts by writing and running code; see [CodeAct Agent](#codeact-agent-feature-codeact) |
| `CodingAgent` | (feature `coding`) A coding harness over `LlmAgent` with the [`adk-devtools`](https://crates.io/crates/adk-devtools) toolset and a `write_todos` planning tool; see the [Coding Agent guide](../docs/official_docs/coding-agent/index.md) |
| `AmbientAgent` | (feature `ambient`) Runs an agent from cron, webhook, or file-watch triggers |

`LlmEventSummarizer` summarizes older events for context compaction.

## Installation

```toml
[dependencies]
adk-agent = "3.0.0"
```

Or use the umbrella crate, whose default `minimal` tier includes agents:

```toml
[dependencies]
adk-rust = "3.0.0"
```

## Feature Flags

| Feature | Default | Enables |
|---------|---------|---------|
| `skills` | yes | `with_skills`, `with_auto_skills`, and the other skill methods on every agent builder |
| `skills-progressive-disclosure` | no | `adk-skill`'s progressive-disclosure skill tools (implies `skills`) |
| `guardrails` | no | `input_guardrails`, `output_guardrails`, and `tool_guardrails` through `adk-guardrail` |
| `enhanced-plugins` | no | `enhanced_plugin` / `enhanced_plugins`: the `EnhancedPlugin` pipeline intercepting tool and model calls |
| `team-tools` | no | `Delegate` relationships in a `TeamSpec`, lowered to `AgentTool`s |
| `codeact` | no | `CodeActAgent` |
| `coding` | no | `CodingAgent` over `adk-devtools` |
| `sandbox` | no | `LlmAgentBuilder::sandbox_config` through `adk-sandbox` |
| `ambient` | no | `AmbientAgent` with `CronTrigger`, `WebhookTrigger`, and `FileWatchTrigger` |
| `record-payloads` | no | Records trace payloads in full, instead of truncated to `RunConfig::trace_payload_max_bytes`, when `RunConfig::record_payloads` is set |

## Quick Start

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::Content;
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

    let agent = LlmAgentBuilder::new("assistant")
        .description("Helpful AI assistant")
        .instruction("Be helpful and concise.")
        .model(model)
        .build()?;

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "quickstart".into(),
            user_id: "user".into(),
            session_id: Some("session-1".into()),
            state: Default::default(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("quickstart")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()?;

    let message = Content::new("user").with_text("What is the capital of France?");
    let mut events = runner.run_str("user", "session-1", message).await?;
    while let Some(event) = events.next().await {
        let event = event?;
        // Streamed chunks arrive as partial events; the final event carries the whole answer.
        if !event.is_final_response() {
            continue;
        }
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Some(text) = part.text() {
                    println!("{text}");
                }
            }
        }
    }
    Ok(())
}
```

The example needs `adk-agent`, `adk-core`, `adk-model`, `adk-runner`, `adk-session`, `futures`, and `tokio`, and `GOOGLE_API_KEY` in the environment.

### LlmAgentBuilder Methods

| Method | Description |
|--------|-------------|
| `new(name)` | Create a builder with the agent name |
| `description(desc)` | Set the agent description |
| `model(llm)` | Set the model (required) |
| `instruction(text)` | Set the static instruction, sent as `system` content |
| `instruction_provider(fn)` | Set a dynamic instruction provider |
| `global_instruction(text)` | Set the global instruction shared across agents |
| `global_instruction_provider(fn)` | Set a dynamic global instruction provider |
| `generate_content_config(config)` | Set the full `GenerateContentConfig` |
| `temperature(f32)` / `top_p(f32)` / `top_k(i32)` | Set one sampling parameter |
| `max_output_tokens(i32)` | Set the output token limit |
| `with_skills(index)` | Attach a preloaded skills index (`skills`) |
| `with_auto_skills()` | Load skills from `.skills/` in the current directory (`skills`) |
| `with_skills_from_root(path)` | Load skills from `.skills/` under a root (`skills`) |
| `with_skill_policy(policy)` | Configure skill matching (`top_k`, threshold, tags) (`skills`) |
| `with_skill_budget(chars)` | Cap injected skill content length (default 2000) (`skills`) |
| `tool(tool)` | Add a static tool |
| `toolset(toolset)` | Add a toolset resolved per invocation |
| `strict_toolsets(bool)` | Fail the turn when a toolset fails to resolve, instead of skipping it |
| `sub_agent(agent)` | Add a sub-agent for transfers |
| `max_iterations(n)` | Set the maximum model round-trips (default 100, `DEFAULT_MAX_ITERATIONS`) |
| `retain_prepared_history(bool)` | Continue the turn from contents a before-model callback changed, for a callback that owns history preparation (default `false`) |
| `tool_timeout(duration)` | Set the per-tool timeout (default five minutes, `DEFAULT_TOOL_TIMEOUT`) |
| `expose_tool_error_messages(bool)` | Send every failed tool's error message to the model; by default only a rejection's message (`ErrorCategory::is_rejection`) joins its component, category, and code |
| `default_retry_budget(budget)` | Set the retry budget for every tool |
| `tool_retry_budget(name, budget)` | Set the retry budget for one tool |
| `circuit_breaker_threshold(n)` | Disable a tool after `n` consecutive failures in an invocation |
| `on_tool_error(callback)` | Add a fallback handler for tool failures |
| `require_tool_confirmation(name)` | Require confirmation for one tool |
| `require_tool_confirmation_for_all()` | Require confirmation for every tool |
| `tool_confirmation_policy(policy)` | Set a `ToolConfirmationPolicy` |
| `tool_execution_strategy(strategy)` | `Sequential`, `Parallel`, or `Auto` dispatch |
| `parallelize_agent_delegations(bool)` | Overlap consecutive agent-delegation calls without parallelizing ordinary tools |
| `disallow_transfer_to_parent(bool)` | Refuse transfers back to the parent |
| `disallow_transfer_to_peers(bool)` | Refuse transfers to sibling agents |
| `include_contents(mode)` | Control which history the agent receives |
| `input_schema(json)` | Set the input JSON schema |
| `output_schema(json)` / `output_type::<T>()` | Set the output JSON schema, directly or from a `schemars::JsonSchema` type |
| `output_max_retries(n)` | Correction retries when output fails the schema (default 3) |
| `output_key(key)` | Write the final response text to this state key |
| `input_guardrails(set)` | Screen each user message (`guardrails`) |
| `output_guardrails(set)` | Screen the final response and any text written beside a tool call (`guardrails`) |
| `tool_guardrails(set)` | Screen each call's final arguments, and each result before the model sees it (`guardrails`) |
| `enhanced_plugin(plugin)` / `enhanced_plugins(plugins)` | Add `EnhancedPlugin`s (`enhanced-plugins`) |
| `sandbox_config(config)` | Run tools under an `adk-sandbox` workspace configuration (`sandbox`) |
| `before_callback(fn)` / `after_callback(fn)` | Add before-agent and after-agent callbacks |
| `before_model_callback(fn)` / `after_model_callback(fn)` | Add model callbacks |
| `before_tool_callback(fn)` / `after_tool_callback(fn)` | Add tool callbacks |
| `after_tool_callback_full(fn)` | Add an after-tool callback that receives the tool, arguments, and response |
| `build()` | Build the `LlmAgent` |

`build()` fails with `agent.duplicate_tool_name` when two static tools share a name.

### Governed Tool Execution

`LlmAgent` and `CodeActAgent` authorize each tool call with `adk_core::authorize_tool_call` after before-tool plugins and callbacks have run, so the run's `ToolPolicy`, the agent's tool guardrails, and confirmation all see the arguments that will execute. A call held for confirmation is answered with an error result, its sibling calls in the turn still run, and the hold is surfaced as a `ToolConfirmationRequest` event. A decision recorded against `request.fingerprint()` authorizes the same agent's same call, to a tool with the same origin and declaration, when the next run resumes it or the model issues it again. A resumed call whose tool now resolves to another publisher, or was redefined, is answered as not run. A frozen `GovernanceControl` executes no further tool call — `LlmAgent` answers each one `not run`, `CodeActAgent` ends the run there — and ends the run before its next model call.

A workflow agent stops when a sub-agent holds a call and resumes at that sub-agent on the next run, without repeating finished sub-agents, loop iterations, or routing. A call its provider sent without an ID gets a generated `adk-call-` ID, so same-name calls keep their own arguments. See [Tool Authorization](../docs/official_docs/security/tool-authorization.md).

### Tool Failures

The function response for a failed tool call names the error's component, category, and code (`{"error": ...}`) instead of quoting its message, so a provider never receives text a tool did not mean to share. `expose_tool_error_messages(true)` sends the full message; an `on_tool_error` callback, or a tool that returns `Ok(json!({"error": "..."}))`, chooses the text for one failure. Argument validation errors (`tool.invalid_arguments`) keep their message so the model can correct the call.

A toolset whose `tools()` call fails is skipped for that turn with a `warn` log; `strict_toolsets(true)` fails the turn instead. A final model turn with no answer — no parts, only thinking, or only whitespace — fails the run with `model.empty_response`.

### Generation Config

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{GenerateContentConfig, Llm, Result};

fn agents(model: Arc<dyn Llm>) -> Result<(LlmAgent, LlmAgent)> {
    // Provider-portable shorthand
    let short = LlmAgentBuilder::new("assistant")
        .model(model.clone())
        .max_output_tokens(4096)
        .build()?;

    // Full config
    let full = LlmAgentBuilder::new("precise")
        .model(model)
        .generate_content_config(GenerateContentConfig {
            max_output_tokens: Some(2048),
            ..Default::default()
        })
        .build()?;
    Ok((short, full))
}
```

Sampling controls such as `temperature`, `top_p`, and `top_k` are model-specific. `GeminiModel` refuses them for `gemini-3.6-flash` and `gemini-3.7-flash` with `model.gemini.sampling_unsupported`; a field a provider client cannot send fails with `model.unsupported_config` before the request leaves.

### Skills

Skills are opt-in. No skill content is injected unless you call a skills method:

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Result};

fn skilled(model: Arc<dyn Llm>) -> Result<LlmAgent> {
    LlmAgentBuilder::new("assistant")
        .model(model)
        .with_auto_skills()? // loads .skills/**/*.md when present
        .build()
}
```

The selected skill is injected into the current user turn, after global and agent instructions plus prior conversation history, so the stable prompt prefix stays reusable by provider prompt caches across turns. Every workflow agent (`LoopAgent`, `SequentialAgent`, `ParallelAgent`, `ConditionalAgent`, `LlmConditionalAgent`) takes the same skills methods.

### Workflow Agents

```rust
use std::sync::Arc;

use adk_agent::{LoopAgent, ParallelAgent, SequentialAgent};
use adk_core::Agent;

fn workflows(a: Arc<dyn Agent>, b: Arc<dyn Agent>, c: Arc<dyn Agent>) {
    // Sequential: A -> B -> C
    let _pipeline = SequentialAgent::new("pipeline", vec![a.clone(), b.clone(), c.clone()]);

    // Parallel: A and B simultaneously
    let _team = ParallelAgent::new("team", vec![a.clone(), b.clone()]).with_shared_state();

    // Loop: repeat until a sub-agent escalates or the limit is reached
    let _iterator = LoopAgent::new("iterator", vec![c]).with_max_iterations(10);
}
```

`LoopAgent` defaults to 1000 iterations (`DEFAULT_LOOP_MAX_ITERATIONS`). Every workflow agent takes `with_description`, `before_callback`, `after_callback`, and the skills methods. A workflow sub-agent that did not receive control by transfer sees other agents' turns as quoted user-role context (`adk_core::history::OTHER_AGENT_CONTEXT_PREAMBLE`) instead of as its own model turns.

### Portable Teams

`TeamSpec` keeps orchestration semantics in ADK-Rust rather than a UI or deployment adapter. It is serializable, validates the complete topology, and compiles named bindings into one executable `CompiledTeam` root. Members can be LLM, CodeAct, workflow, graph, realtime, or remote agents because compilation binds the existing `Agent` contract.

Enable the `team-tools` feature when the spec contains `Delegate` relationships; without it `compile` fails with `TeamError::DelegationFeatureDisabled`. Handoff-only teams do not pull in `adk-tool`.

```rust
use std::sync::Arc;

use adk_agent::{
    Agent, CompiledTeam, RelationshipKind, TeamError, TeamMemberSpec, TeamRelationship,
    TeamSpec,
};

fn compile(
    supervisor: Arc<dyn Agent>,
    billing: Arc<dyn Agent>,
    researcher: Arc<dyn Agent>,
) -> Result<CompiledTeam, TeamError> {
    let spec = TeamSpec {
        name: "customer_service".into(),
        description: "Routes and resolves customer requests".into(),
        coordinator: "supervisor".into(),
        members: vec![
            TeamMemberSpec::new("supervisor"),
            TeamMemberSpec::new("billing"),
            TeamMemberSpec::new("researcher"),
        ],
        relationships: vec![
            TeamRelationship::new("supervisor", "billing", RelationshipKind::Handoff),
            TeamRelationship::new("supervisor", "researcher", RelationshipKind::Delegate),
        ],
        policy: Default::default(),
    };

    spec.compile(vec![supervisor, billing, researcher])
}
```

`Handoff` changes the active agent through Runner transfer mechanics. `Delegate` lowers to an `AgentTool`, returns the member's result to its caller, and requires the `team-tools` feature. Per-member handoff and delegate targets are exact; undeclared edges fail instead of expanding to every sibling. Team policies bound handoff depth, nested delegation, delegate concurrency, aggregate resource use, and termination. Exact edge contracts can restrict schemas, context/history, state keys, timeout, approval, retries, fallbacks, and circuit breaking. Schema, projection, timeout, approval, and circuit-breaker fields are delegate-only and are rejected on handoff edges instead of being ignored.

Each run emits a durable execution receipt with a frozen roster, causal edge ledger, usage, and status. `TeamAgentRegistry` supports deterministic capability discovery, `TeamArchitectureTemplate` and `WorkflowArchitectureTemplate` provide portable presets, and `BlackboardSpec` provides bounded round-robin or selector-driven shared-transcript orchestration.

Restored active handoffs resume at their frozen target. Active delegates are never replayed automatically: without a provider-specific checkpoint or idempotency contract, replay could duplicate side effects.

Compilation validates execution-plane `AgentCapabilities`: delegate sources must consume runtime toolsets, handoff sources must support transfer, and approved edges must honor exact-call confirmation. Ordered async `TeamLifecycleHook`s cover team, member, and relationship boundaries while ordinary Runner plugins and existing agent callbacks continue to apply. Team execution emits `team.*` telemetry spans and stable structured errors.

`TeamBudget` caps model calls, tokens, cost, tool calls, and wall time through a `BudgetTracker` that every member shares and that counts against the run's `RunBudget`; members stop before the call that would start past a limit, and the error is `ResourceExhausted` with its `budget.*` code. Partial streaming chunks never count. The runtime keeps the receipts of the 64 most recently finished invocations.

Relationship contracts separately allow state reads and writes, reject concurrent state conflicts, and restrict artifact-name prefixes. Dynamic registry resolution can require health, version, digest, and trust labels. `CompiledTeam::resume_plan`, `validate_team_replay`, and `CompiledTeam::analyze_execution` support durable hosts and provider-free replay/coverage evaluation without replaying an unsafe delegate.

### Conditional Agents

```rust
use std::sync::Arc;

use adk_agent::{ConditionalAgent, LlmConditionalAgent};
use adk_core::{Agent, Llm, Result};

fn routers(
    model: Arc<dyn Llm>,
    urgent: Arc<dyn Agent>,
    normal: Arc<dyn Agent>,
    support: Arc<dyn Agent>,
    sales: Arc<dyn Agent>,
) -> Result<()> {
    // Function-based condition
    let _conditional = ConditionalAgent::new(
        "router",
        |ctx| {
            ctx.user_content()
                .parts
                .iter()
                .find_map(|part| part.text())
                .is_some_and(|text| text.contains("urgent"))
        },
        urgent,
    )
    .with_else(normal.clone());

    // Model-driven routing
    let _llm_router = LlmConditionalAgent::builder("smart_router", model)
        .instruction("Route to the appropriate specialist based on the query.")
        .route("support", support)
        .route("sales", sales)
        .default_route(normal)
        .build()?;
    Ok(())
}
```

`LlmConditionalAgent` lowercases route labels and the model's trimmed classification, and picks the first label the classification contains, so the model does not need to produce an exact match.

### Multi-Agent Systems

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Agent, Llm, Result};

fn coordinator(
    model: Arc<dyn Llm>,
    support: Arc<dyn Agent>,
    sales: Arc<dyn Agent>,
) -> Result<(LlmAgent, LlmAgent)> {
    // Agent with sub-agents for transfer
    let coordinator = LlmAgentBuilder::new("coordinator")
        .instruction("Route to the appropriate specialist. Transfer when needed.")
        .model(model.clone())
        .sub_agent(support)
        .sub_agent(sales)
        .build()?;

    // Restrict where a leaf agent can transfer
    let leaf = LlmAgentBuilder::new("leaf")
        .model(model)
        .disallow_transfer_to_parent(true)
        .disallow_transfer_to_peers(true)
        .build()?;
    Ok((coordinator, leaf))
}
```

The runner enforces these restrictions on every transfer, including one a tool or a custom agent requests.

### Toolsets

`.toolset()` adds context-dependent tools resolved at the start of each `run()` from the invocation's `ReadonlyContext` — for example, per-user browser sessions from a pool or an MCP server's tools. Static tools (`.tool()`) and toolsets can be mixed on the same agent; duplicate tool names across them fail the turn. `Runner::close` reaches `Toolset::close` for each toolset through `Agent::close`.

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Result, Toolset};

fn with_toolset(model: Arc<dyn Llm>, toolset: Arc<dyn Toolset>) -> Result<LlmAgent> {
    LlmAgentBuilder::new("browser_agent")
        .description("Multi-tenant browser agent")
        .instruction("Help users browse the web.")
        .model(model)
        .toolset(toolset)
        .build()
}
```

### Retry Budget

```rust
use std::sync::Arc;
use std::time::Duration;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Result, RetryBudget, Tool};

fn resilient(model: Arc<dyn Llm>, tool: Arc<dyn Tool>) -> Result<LlmAgent> {
    LlmAgentBuilder::new("resilient_agent")
        .model(model)
        .tool(tool)
        .default_retry_budget(RetryBudget::new(2, Duration::from_secs(1)))
        .tool_retry_budget("browser_navigate", RetryBudget::new(3, Duration::from_millis(500)))
        .build()
}
```

Per-tool budgets take precedence over the default. Without a budget, tools execute once. A budget retries only tools whose `effect()` is `ReadOnly` or `Idempotent`, only after a retryable error or a timeout, with exponential backoff up to `RetryBudget::max_delay` and jitter; each attempt gets a fresh tool context. A `NonIdempotent` tool runs once, and one that times out is answered with `{"status": "outcome_unknown"}`. With `RunConfig::action_ledger` set, a replayed `NonIdempotent` call is answered from the ledger instead of executing again.

### Circuit Breaker

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Result, Toolset};

fn guarded(model: Arc<dyn Llm>, toolset: Arc<dyn Toolset>) -> Result<LlmAgent> {
    LlmAgentBuilder::new("guarded_agent")
        .model(model)
        .toolset(toolset)
        .circuit_breaker_threshold(5)
        .build()
}
```

After five consecutive failures of a tool, the breaker opens and answers its calls with an error without executing the tool. The breaker resets at the start of each invocation.

### Tool Error Callbacks

`on_tool_error` callbacks supply fallback results when a tool fails (after retries are exhausted). They are tried in order; the first to return `Some(value)` wins.

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Result, Tool};
use serde_json::json;

fn with_fallback(model: Arc<dyn Llm>, tool: Arc<dyn Tool>) -> Result<LlmAgent> {
    LlmAgentBuilder::new("fallback_agent")
        .model(model)
        .tool(tool)
        .on_tool_error(Box::new(|_ctx, tool, _args, error| {
            Box::pin(async move {
                tracing::warn!(tool = tool.name(), error = %error, "tool failed");
                // Ok(Some(value)) substitutes a result; Ok(None) keeps the default error response.
                Ok(Some(json!({ "error": "the lookup service is unavailable; try again later" })))
            })
        }))
        .build()
}
```

### Rich After-Tool Callbacks

`after_tool_callback_full` receives the tool, arguments, and response value. Return `Ok(None)` to keep the response, or `Ok(Some(value))` to replace it. These run after the `after_tool_callback` chain.

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Result};

fn auditing(model: Arc<dyn Llm>) -> Result<LlmAgent> {
    LlmAgentBuilder::new("auditing_agent")
        .model(model)
        .after_tool_callback_full(Box::new(|_ctx, tool, _args, _response| {
            Box::pin(async move {
                tracing::info!(tool = tool.name(), "tool completed");
                Ok(None)
            })
        }))
        .build()
}
```

### Guardrails

Requires the `guardrails` feature.

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Result};
use adk_guardrail::{ContentFilter, GuardrailSet, PiiRedactor};

fn safe(model: Arc<dyn Llm>) -> Result<LlmAgent> {
    let input_guardrails = GuardrailSet::new()
        .with(ContentFilter::harmful_content())
        .with(PiiRedactor::new());

    LlmAgentBuilder::new("safe_assistant")
        .model(model)
        .input_guardrails(input_guardrails)
        .build()
}
```

Under a `Runner`, input guardrails screen each user message before it is saved: a blocked message is never saved, and a redacted one is saved redacted, so later runs see only that form. The guardrails of every agent in the tree apply, including sub-agents and workflow steps. A block ends the run with `ErrorCategory::InvalidInput` (HTTP 400) and code `guardrail.input_blocked` or `guardrail.output_blocked`; `details.metadata` names the guardrail, its severity, the phase, and the failures. A `Low`-severity finding passes the content and is recorded on the output event under `adk.guardrail.warnings`. `tool_guardrails` also screen each tool result before the model and the session see it. See [Guardrails](../docs/official_docs/security/guardrails.md#input-guardrails-and-the-session).

### Custom Agent

```rust
use adk_agent::{CustomAgent, CustomAgentBuilder};
use adk_core::{Content, Event, EventStream, Result};

fn processor() -> Result<CustomAgent> {
    CustomAgentBuilder::new("processor")
        .description("Custom data processor")
        .handler(|ctx| async move {
            let mut event = Event::new(ctx.invocation_id());
            event.author = "processor".to_string();
            event.llm_response.content = Some(Content::new("model").with_text("Processed!"));
            Ok(Box::pin(futures::stream::iter(vec![Ok(event)])) as EventStream)
        })
        .build()
}
```

## CodeAct Agent (feature `codeact`)

`CodeActAgent` is a peer to `LlmAgent` that **acts by writing and running code** instead of emitting one tool call at a time. Each turn the model produces a single script; tools are exposed as callable functions the script composes, and the script returns a tagged `ScriptOutput` (`observation` / `error` / `final_result` / `transfer_to_agent`).

It is **language-agnostic**: the `CodeRuntime` trait is the step-wise interpreter seam, so the language (Python via Monty, a shell, a DSL, ...) is the runtime's choice. A runtime that can snapshot a paused call enables HITL confirmation and long-running tool deferral, which **suspend** into session state and **resume** on the next `run()` — the same save-rebuild-continue model as `LlmAgent`.

```rust
use std::sync::Arc;

use adk_agent::codeact::{CodeActAgent, CodeRuntime};
use adk_core::{Llm, Result, Tool};

fn analyst(
    model: Arc<dyn Llm>,
    runtime: Arc<dyn CodeRuntime>,
    load_csv: Arc<dyn Tool>,
) -> Result<CodeActAgent> {
    CodeActAgent::builder()
        .name("analyst")
        .model(model)
        .runtime(runtime)
        .instruction("Prefer concise, composable steps.")
        .tool(load_csv)
        .output_key("report")
        .build()
}
```

The configuration surface mirrors `LlmAgentBuilder`: instructions (static and providers, with `{state.key}` injection), `include_contents`, static tools and per-invocation `toolset`s, `generate_content_config` (and `temperature` / `top_p` / `top_k` / `max_output_tokens`), `tool_timeout`, retry budgets, circuit breaker, `on_tool_error`, `output_schema` / `output_type` with a correction-retry loop, `output_key`, tool confirmation, sub-agent transfer with the `disallow_*` flags, and feature-gated guardrails (`guardrails`), skills (`skills`), and the `EnhancedPlugin` pipeline (`enhanced-plugins`).

The full agent/model/tool callback surface is supported too: `before_callback` / `after_callback`, `before_model_callback` / `after_model_callback` (rewrite or short-circuit the model call), and `before_tool_callback` / `after_tool_callback` / `after_tool_callback_full` (rewrite or short-circuit a tool call). Each tool call gets a fresh per-call `ToolContext` that carries the interpreter call id and delegates artifacts, memory, shared state, user scopes, and secrets to the live invocation.

With `RunConfig::action_ledger` set, `CodeActAgent` records each `NonIdempotent` call like `LlmAgent` does. The idempotency key is `{app}/{user}/{session}/codeact/{run_id}/{iteration}/{call_id}`, so two identical calls in one program both execute, and a call replayed from a checkpoint (crash recovery or a confirmed resume) is answered from the ledger instead of executing again. A ledger answer, and a non-idempotent call that times out or panics, is raised into the script as the same `already_succeeded` / `already_failed` / `outcome_unknown` payload `LlmAgent` returns.

A runnable, dependency-free end-to-end demo (a self-contained `CodeRuntime` and a deterministic model) lives in [`examples/codeact_agent`](../examples/codeact_agent).

## Tool Call Markup Normalization

The `tool_call_markup` module handles models that emit tool calls as text markup (for example `<tool_call>...</tool_call>`) instead of structured function calls. `normalize_content` parses these text blocks into `Part::FunctionCall` parts and gives calls without an ID a generated `adk-call-` ID:

```rust
use adk_agent::normalize_content;
use adk_core::Content;

fn normalize(content: &mut Content) {
    normalize_content(content);
}
```

`LlmAgent` applies it automatically; call it only when building custom agent logic.

## Context Compaction

`LlmEventSummarizer` uses a model to summarize older conversation events, reducing context size for long-running sessions:

```rust
use std::sync::Arc;

use adk_agent::LlmEventSummarizer;
use adk_core::{EventsCompactionConfig, Llm};

fn compaction(model: Arc<dyn Llm>) -> EventsCompactionConfig {
    let summarizer = LlmEventSummarizer::new(model);
    // Optionally customize the prompt template:
    // let summarizer = summarizer.with_prompt_template("Custom: {conversation_history}");

    EventsCompactionConfig {
        compaction_interval: 3, // when the user-message count is a multiple of 3
        overlap_size: 1,        // leave the latest invocation out of the summary
        summarizer: Arc::new(summarizer),
    }
}
```

Pass the config to `Runner::builder().compaction_config(...)` to enable automatic compaction. Each summary's input includes the previous summary and the window's function calls and responses, so older turns and tool results carry forward through repeated compaction. Compaction never covers a call still held for confirmation.

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| Instructions travel as `system` contents instead of `user` | Custom `Llm` implementations receive instructions as `system` contents |
| A failed tool's function response names its component, category, and code, not its message | `expose_tool_error_messages(true)`, or an `on_tool_error` fallback |
| Retries apply only to `ReadOnly` and `Idempotent` tools; a timed-out `NonIdempotent` call reports `outcome_unknown` | Declare `ToolEffect::Idempotent` on tools that are safe to repeat |
| Tool calls are authorized after plugins and before-tool callbacks rewrite their arguments | — |
| A held call is answered and its siblings run; workflow agents pause and resume at the holding sub-agent | — |
| A failing toolset is skipped instead of failing the turn | `strict_toolsets(true)` keeps the old behaviour |
| Two static tools with one name fail `build()` with `agent.duplicate_tool_name` | Rename one tool |
| A final model turn with no answer fails with `model.empty_response` | — |
| Guardrail blocks are `guardrail.input_blocked` / `guardrail.output_blocked` (`InvalidInput`) instead of `agent.legacy` | Match the new codes |
| `after_agent` callbacks run after a transfer, a turn-ending tool result, and a cancellation | — |
| Workflow sub-agents see other agents' turns as quoted context | — |
| Team budget errors are `ResourceExhausted` | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — umbrella crate
- [adk-core](https://crates.io/crates/adk-core) — the `Agent` trait
- [adk-model](https://crates.io/crates/adk-model) — model providers
- [adk-tool](https://crates.io/crates/adk-tool) — the tool system
- [adk-runner](https://crates.io/crates/adk-runner) — the execution runtime
- [adk-guardrail](https://crates.io/crates/adk-guardrail) — guardrails
- [adk-skill](https://crates.io/crates/adk-skill) — skill discovery and injection

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
