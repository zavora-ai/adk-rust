# Plugins

The `adk-plugin` crate hooks into agent runs without changing agent code — for logging,
guardrails, caching, cost tracking, and other middleware. It has two plugin kinds:

| Kind | Defined with | Registered on | Hooks |
|------|--------------|---------------|-------|
| `Plugin` | `PluginBuilder` or `PluginConfig` (closures) | A `PluginManager` on the `Runner` | Run, user message, event, agent, model, model error, tool, tool error, close |
| `EnhancedPlugin` | A trait | `LlmAgentBuilder::enhanced_plugin`, or an `EnhancedPluginManager` on `IntegratedRealtimeRunner` | Before and after each tool call and model call, with priority ordering and typed shared state |

## Installation

```toml
[dependencies]
adk-plugin = "3.0.0"
adk-runner = "3.0.0"
# `EnhancedPlugin` on `LlmAgent` needs adk-agent's `enhanced-plugins` feature;
# adk-rust's `plugin` feature (in the `standard` tier) enables it.
adk-agent = { version = "3.0.0", features = ["enhanced-plugins"] }
```

## Runner plugins

A `PluginManager` holds `Plugin`s. The runner runs the run, user-message, and event callbacks
itself, and adds the manager to `RunConfig::invocation_hooks`, so every `LlmAgent` and
`CodeActAgent` in the run — transfer targets and agents behind an `AgentTool` included — runs
the agent, model, and tool callbacks ahead of its own. See
[Plugin hooks](runner.md#plugin-hooks) for when each runs.

| `PluginBuilder` method | Callback receives | Returning a value |
|------------------------|-------------------|-------------------|
| `before_run` | The invocation context | `Some(content)` ends the run with that content |
| `after_run` | The invocation context | — (side effects only) |
| `on_user_message` | The context and the user content | `Some(content)` replaces the message |
| `on_event` | The context and each event | `Some(event)` replaces the event |
| `before_agent`, `after_agent` | The callback context | `Some(content)` answers in the agent's place, or replaces its output |
| `before_model` | The callback context and the `LlmRequest` | `BeforeModelResult::Continue(request)` with a possibly changed request, or `Skip(response)` to skip the call |
| `after_model` | The callback context and the `LlmResponse` | `Some(response)` replaces the response |
| `on_model_error` | The callback context, the request, and the error message | `Some(response)` answers the turn in the failed call's place |
| `before_tool` | The callback context (`tool_name()`, `tool_input()`) | `Some(content)` skips the tool and answers the call with the content |
| `after_tool` | The callback context | `Some(content)` replaces the result |
| `on_tool_error` | The callback context, the tool, its arguments, and the error message | `Some(value)` answers the failed call with that result |
| `close_fn` | — | Runs on `Runner::close` |

For a callback that can change a value, the first plugin that returns one wins, and a plugin
callback that returns a value short-circuits the agent's own callbacks of that kind.

```rust
use adk_core::{CallbackContext, Content};
use adk_plugin::{PluginBuilder, PluginManager};
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use std::sync::Arc;

fn build(agent: Arc<dyn adk_core::Agent>) -> adk_core::Result<Runner> {
    let deny_deletes = PluginBuilder::new("deny-deletes")
        .before_tool(Box::new(|ctx: Arc<dyn CallbackContext>| {
            Box::pin(async move {
                if ctx.tool_name() == Some("delete_file") {
                    // Text-only content answers the call as `{"error": "<text>"}`.
                    return Ok(Some(Content::new("tool").with_text("delete_file is disabled")));
                }
                Ok(None)
            })
        }))
        .on_event(Box::new(|_ctx, event| {
            Box::pin(async move {
                tracing::debug!(event.id = %event.id, event.author = %event.author, "event");
                Ok(None)
            })
        }))
        .build();

    Runner::builder()
        .app_name("my_app")
        .agent(agent)
        .session_service(Arc::new(InMemorySessionService::new()))
        .plugin_manager(Arc::new(PluginManager::new(vec![deny_deletes])))
        .build()
}
```

### Model fallback

`LlmAgent` calls `on_model_error` when the model call or its response stream fails, before it
fails the turn, with the request the model was sent and the error message. A response the
callback returns answers the turn in the failed call's place: its text is the answer and its
tool calls run. A refusal by the run's budget or spend ledger is not reported.

```rust
use adk_core::{Llm, LlmResponse};
use adk_plugin::{Plugin, PluginConfig, PluginManager};
use futures::StreamExt;
use std::sync::Arc;

fn model_fallback(fallback: Arc<dyn Llm>) -> PluginManager {
    PluginManager::new(vec![Plugin::new(PluginConfig {
        name: "model-fallback".to_string(),
        on_model_error: Some(Box::new(move |_ctx, mut request, _error| {
            let fallback = fallback.clone();
            Box::pin(async move {
                tracing::warn!("primary model failed; using the fallback");
                request.model = fallback.name().to_string();
                let mut responses = fallback.generate_content(request, false).await?;
                let mut answer: Option<LlmResponse> = None;
                while let Some(response) = responses.next().await {
                    answer = Some(response?);
                }
                Ok(answer)
            })
        })),
        ..Default::default()
    })])
}
```

## Enhanced plugins

`EnhancedPlugin` is a trait whose hooks default to passing their input through, so a plugin
implements only what it needs. Each hook's output feeds the next plugin's input.

| Method | Default | Returns |
|--------|---------|---------|
| `name()` | Required | The plugin's identifier |
| `priority()` | `100` | Order: lower values run first; equal priorities run in registration order |
| `before_tool_call(tool, args, ctx, plugin_ctx)` | Continue with `args` | `BeforeToolCallResult::Continue(args)` with possibly rewritten arguments, or `ShortCircuit(result)` to skip the tool |
| `after_tool_call(tool, args, result, ctx, plugin_ctx)` | Continue with `result` | `AfterToolCallResult::Continue(result)`; called after a successful execution |
| `before_model_call(request, ctx, plugin_ctx)` | Continue with `request` | `BeforeModelCallResult::Continue(request)` or `ShortCircuit(response)` |
| `after_model_call(response, ctx, plugin_ctx)` | Continue with `response` | `AfterModelCallResult::Continue(response)` |
| `close()` | Nothing | Runs when the plugin shuts down |

An `Err` from any hook stops the pipeline and fails the call. In `LlmAgent`,
`before_tool_call` runs before the run's tool policy, so the policy checks the rewritten
arguments; see [Tool Authorization](../security/tool-authorization.md).

```rust
use adk_core::{CallbackContext, Result, Tool};
use adk_plugin::{BeforeToolCallResult, EnhancedPlugin, PluginContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Default)]
struct SearchCalls(u32);

struct SearchLimits;

#[async_trait]
impl EnhancedPlugin for SearchLimits {
    fn name(&self) -> &str {
        "search-limits"
    }

    fn priority(&self) -> i32 {
        10
    }

    async fn before_tool_call(
        &self,
        tool: Arc<dyn Tool>,
        mut args: Value,
        _ctx: Arc<dyn CallbackContext>,
        plugin_ctx: &PluginContext,
    ) -> Result<BeforeToolCallResult> {
        if tool.name() != "search" {
            return Ok(BeforeToolCallResult::Continue(args));
        }
        let SearchCalls(count) = plugin_ctx.get::<SearchCalls>().await.unwrap_or_default();
        plugin_ctx.insert(SearchCalls(count + 1)).await;
        if count >= 5 {
            return Ok(BeforeToolCallResult::ShortCircuit(json!({ "error": "search limit reached" })));
        }
        // Default the page size.
        if args.get("limit").is_none() {
            args["limit"] = json!(10);
        }
        Ok(BeforeToolCallResult::Continue(args))
    }
}
```

`PluginContext` is a typed store shared by every enhanced plugin of an agent: each Rust type
is its own key (`insert`, `get`, `contains`, `remove`). Each invocation gets its own context,
so the limit above counts one run's searches, and two users' concurrent runs never read or
overwrite each other's plugin state. State meant for every invocation of the agent, such as an
agent-wide quota, goes in `plugin_ctx.shared()`, the context `EnhancedPluginManager::context()`
returns.

| Context | Holds | Lifetime |
|---------|-------|----------|
| `plugin_ctx` (the hook argument) | One invocation's state, keyed by app, user, session, and invocation id | The invocation; the manager keeps the 1024 most recently used |
| `plugin_ctx.shared()` | State every invocation of the agent shares | The agent |

Register enhanced plugins on the agent:

```rust
use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::Llm;
use adk_plugin::EnhancedPlugin;
use std::sync::Arc;

fn build(model: Arc<dyn Llm>, plugin: Arc<dyn EnhancedPlugin>) -> adk_core::Result<LlmAgent> {
    LlmAgentBuilder::new("my_agent")
        .model(model)
        .instruction("You are a helpful assistant.")
        .enhanced_plugin(plugin)
        .build()
}
```

`AdaptedPlugin::new(plugin, priority)` runs a closure-based `Plugin` in the enhanced pipeline.
Its `before_tool` content refuses the call, as an agent's own before-tool callback does; its
arguments and `after_tool` results pass through unchanged.

## Related

- [Runner](runner.md#plugin-hooks) — When each runner plugin callback runs
- [Retry & Reflect](../tools/retry-reflect.md) — An enhanced plugin for tool failure recovery
- [Guardrails](../security/guardrails.md) — Input, output, and tool-result validation
- [Telemetry](../observability/telemetry.md) — Observability integration
- [Callbacks](../callbacks/callbacks.md) — Per-agent callbacks

---

**Previous**: [← Action Nodes](../tools/action-nodes.md) | **Next**: [Sessions →](../sessions/sessions.md)
