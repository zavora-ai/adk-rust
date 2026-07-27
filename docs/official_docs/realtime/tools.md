# Tools in Realtime Sessions

The defining feature of a realtime *agent* (vs. a voice bot) is that it can take
**real actions** mid-conversation: look something up, process a refund, hand off
to a human — and then speak the result. Tools run **server-side**, so your
business logic and credentials never touch the client.

## How a tool turn flows

1. The model decides it needs a tool and emits `FunctionCallDone { name, arguments, call_id }`.
2. `RealtimeRunner` looks up the handler for `name` and runs it.
3. The handler's JSON result is sent back to the model as the tool output.
4. The runner triggers **one** follow-up response; the model speaks the answer,
   grounded in the result.

You never call `create_response()` for this — the runner handles the round-trip
when `auto_respond_tools` is on (the default).

## Native tools: `ToolDefinition` + `FnToolHandler`

The lightweight path. A `ToolDefinition` is the JSON schema the model sees; an
`FnToolHandler` is a synchronous closure that runs when it's called.

```rust
use adk_realtime::config::ToolDefinition;
use adk_realtime::events::ToolCall;
use adk_realtime::runner::FnToolHandler;
use serde_json::json;

fn process_refund_def() -> ToolDefinition {
    ToolDefinition {
        name: "process_refund".into(),
        description: Some("Issue a refund for an order. Only when clearly warranted.".into()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "order_id": { "type": "string", "description": "e.g. 'A-10293'" },
                "reason":   { "type": "string", "description": "Short reason" }
            },
            "required": ["order_id", "reason"]
        })),
    }
}

fn process_refund_tool()
-> FnToolHandler<impl Fn(&ToolCall) -> adk_realtime::error::Result<serde_json::Value> + Send + Sync> {
    FnToolHandler::new(|call: &ToolCall| {
        let order = call.arguments.get("order_id").and_then(|v| v.as_str()).unwrap_or("unknown");
        // …do the work…
        Ok(json!({ "status": "approved", "order_id": order,
                   "message": format!("Refund approved for {order}.") }))
    })
}
```

Register it on the builder with `.tool(definition, handler)`:

```rust
let runner = IntegratedRealtimeRunner::builder()
    .model(model)
    .config(config)
    .identity("support", "customer", &session_id)
    .session_service(sessions)
    .tool(process_refund_def(), process_refund_tool())
    .tool(connect_to_human_def(), connect_to_human_tool())
    .build()?;
```

The handler returns a `serde_json::Value`; whatever you return is what the model
sees, so include a human-readable `message` the agent can paraphrase.

> Handlers run **server-side and synchronously** within the event loop. Keep them
> quick; for slow work, return a "started" status and follow up out of band.

## Bridged tools: any `adk_core::Tool`

If you already have `adk-core` tools (your own `FunctionTool`s, or `adk-tool`
built-ins like the knowledge-graph `remember`/`relate`), attach them with
`.adk_tool(...)` — no rewrite. The integration layer wraps each one in a
`ToolHandler` and synthesizes a `ToolContext` scoped to the session's
`(app_name, user_id, session_id)`:

```rust
use adk_tool::{RememberTool, RelateTool};

let runner = IntegratedRealtimeRunner::builder()
    .model(model).config(config).identity("app", "user", &sid)
    .memory_service(kg.clone())
    .adk_tool(Arc::new(RememberTool::new(kg.clone())))   // adk_core::Tool
    .adk_tool(Arc::new(RelateTool::new(kg)))
    .tool(get_weather_def(), get_weather())              // native handler — mix freely
    .build()?;
```

This is how the agent curates its own [memory](memory.md). The bridge serves
**locally-executed, context-independent** tools well; tools that need rich agent
state are better written as native `FnToolHandler`s.

## Parallel tool calls

A model can request several tools in **one** response (e.g. "what's the weather
and the time in London?"). ADK-Rust handles this correctly: it sends each tool's
output as it completes, then issues **exactly one** `response.create` once the
dispatch response finishes.

This matters because the naïve approach — firing a response per tool — hits
OpenAI's *"conversation already has an active response in progress"* error and
stalls the session. The runner avoids it by separating "send tool output"
(`send_tool_output`) from "trigger the response" (`respond_after_tools`, called
once on the dispatch `ResponseDone`). You get this for free; just be aware when
**reading** events that a tool turn spans two responses
(see [Architecture](architecture.md#tool-turns-span-two-responses)).

## Reading tool events in a UI

To surface tool activity (e.g. a "Processing refund…" chip), watch for
`FunctionCallDone`:

```rust
ServerEvent::FunctionCallDone { name, arguments, .. } => {
    // `arguments` is a JSON string of the call args
    ui_show_tool_activity(&name, &arguments);
}
```

The spoken confirmation arrives afterward as `TranscriptDelta` once the tool
result is folded into the follow-up response.

## See it work

The [`customer_service`](examples.md#customer_service) example wires
`process_refund` and `connect_to_human`; the [`realtime_tools`](examples.md#realtime_tools)
example is a headless probe that exercises single-tool, parallel-tool, and
calculator turns on both providers.

Next: [Multimodal →](multimodal.md)

## Tool concurrency

`RunnerConfig::max_concurrent_tools` (default 4) bounds how many tool handlers run at
once. When a response dispatches several calls, the runner queues each on its event loop
and admits it to execution as a permit frees:

```rust
use adk_realtime::{RealtimeRunner, RunnerConfig};

let runner = RealtimeRunner::builder()
    .model(model)
    .runner_config(RunnerConfig {
        auto_execute_tools: true,
        auto_respond_tools: true,
        max_concurrent_tools: 3,
    })
    .build()?;
```

Two properties follow, and both are covered by tests:

- **Event intake continues during tool execution.** Audio deltas, transcripts, and
  interruptions are handled while tools run. A handler that waits on something arriving
  later in the session no longer deadlocks the session.
- **One follow-up response, after the last output.** When tool output is sent
  automatically, the model is owed a single `create_response`. It is issued once both the
  dispatching response has closed and every dispatched tool has reported — in either
  order, since a response can now close while tools are still running.

> **Important:** the bound governs concurrency, not parallelism. Handlers share the
> runner's task, so a handler that blocks the thread — synchronous file or network I/O,
> heavy computation — still stalls the loop. Use `tokio::task::spawn_blocking` for those.

## Disconnect policy

The runner does not reconnect automatically. On transport loss it lets dispatched tools
finish, calls `EventHandler::on_disconnect`, and returns from `run`:

```rust
use adk_realtime::{EventHandler, Result};

struct Reconnecting;

#[async_trait::async_trait]
impl EventHandler for Reconnecting {
    async fn on_disconnect(&self) -> Result<()> {
        tracing::warn!("realtime transport ended");
        Ok(())
    }
}
```

Reconnection stays with the caller because it requires deciding what context to replay
and, on Gemini, whether a stored resumption token is still valid. The `on_disconnect` hook
exists so transport loss can be told apart from a graceful `close` — `run` returns
`Ok(())` for both.
