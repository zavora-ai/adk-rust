# Tools in Realtime Sessions

A realtime agent can take actions mid-conversation — look something up, process a refund,
hand off to a human — and then speak the result. Tools run server-side, so business logic
and credentials stay off the client.

## How a tool turn flows

1. The model decides it needs a tool and emits `FunctionCallDone { name, arguments, call_id }`.
2. `RealtimeRunner` looks up the handler for `name`, checks the call against the
   [governed path](#the-governed-path), and runs it.
3. The handler's JSON result is sent back to the model as the tool output.
4. The runner triggers **one** follow-up response; the model speaks the answer,
   grounded in the result.

The runner handles the round-trip when `auto_respond_tools` is on (the default), so no
`create_response()` call is needed.

## Native tools: `ToolDefinition` + `FnToolHandler`

A `ToolDefinition` is the JSON schema the model sees; an `FnToolHandler` wraps a synchronous
closure that runs when it is called. A handler that awaits implements the async
`ToolHandler` trait instead.

```rust
use adk_realtime::config::{RealtimeConfig, ToolDefinition};
use adk_realtime::events::ToolCall;
use adk_realtime::integration::IntegratedRealtimeRunner;
use adk_realtime::model::BoxedModel;
use adk_realtime::runner::FnToolHandler;
use adk_session::SessionService;
use serde_json::json;
use std::sync::Arc;

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

// Register it on the builder with `.tool(definition, handler)`.
fn build(
    model: BoxedModel,
    config: RealtimeConfig,
    sessions: Arc<dyn SessionService>,
    session_id: &str,
) -> adk_realtime::Result<IntegratedRealtimeRunner> {
    IntegratedRealtimeRunner::builder()
        .model(model)
        .config(config)
        .identity("support", "customer", session_id)
        .session_service(sessions)
        .tool(process_refund_def(), process_refund_tool())
        .build()
}
```

The handler's `serde_json::Value` is what the model sees, so include a human-readable
`message` the agent can paraphrase. Two tools registered under one name fail `build` with a
configuration error.

> Handlers share the runner's task: a handler that blocks the thread stalls the session
> (see [Tool concurrency](#tool-concurrency)).

## Bridged tools: any `adk_core::Tool`

`adk-core` tools (your own `FunctionTool`s, or `adk-tool` built-ins such as the
knowledge-graph `remember` and `relate`) attach with `.adk_tool(...)`. The integration layer
registers each tool's name, description, and parameter schema with the provider and runs it
with a `RealtimeToolContext` that carries the session's `app_name`, `user_id`, and
`session_id`, the call ID, and `search_memory` over the runner's memory service. It has no
artifact service, secrets, or user scopes.

```rust
use adk_memory::GraphMemoryService;
use adk_realtime::config::RealtimeConfig;
use adk_realtime::integration::IntegratedRealtimeRunner;
use adk_realtime::model::BoxedModel;
use adk_tool::{RelateTool, RememberTool};
use std::sync::Arc;

async fn build(
    model: BoxedModel,
    config: RealtimeConfig,
    session_id: &str,
) -> anyhow::Result<IntegratedRealtimeRunner> {
    let kg = Arc::new(GraphMemoryService::new("sqlite:memory.db").await?);
    kg.migrate().await?;
    Ok(IntegratedRealtimeRunner::builder()
        .model(model)
        .config(config)
        .identity("app", "user", session_id)
        .memory_service(kg.clone())
        .adk_tool(Arc::new(RememberTool::new(kg.clone()))) // adk_core::Tool
        .adk_tool(Arc::new(RelateTool::new(kg)))
        .build()?)
}
```

This is how the agent curates its own [memory](memory.md). `RememberTool` and `RelateTool`
need `adk-tool`'s `graph-memory-tools` feature.

## Parallel tool calls

A model can request several tools in one response (for example, "what's the weather and
the time in London?"). The runner sends each tool's output as it completes, then issues one
`response.create` once the dispatch response has finished. A response per tool would fail
on OpenAI with "conversation already has an active response in progress", so sending tool
output (`send_tool_output`) and triggering the response (`respond_after_tools`) are separate
steps. When reading events, a tool turn spans two responses (see
[Architecture](architecture.md#tool-turns-span-two-responses)).

## Reading tool events in a UI

To surface tool activity (for example a "Processing refund…" chip), watch for
`FunctionCallDone`:

```rust
use adk_realtime::events::ServerEvent;

fn tool_activity(event: &ServerEvent) -> Option<String> {
    match event {
        // `arguments` is the call's arguments as a JSON string.
        ServerEvent::FunctionCallDone { name, arguments, .. } => Some(format!("{name}({arguments})")),
        _ => None,
    }
}
```

The spoken confirmation arrives afterward as `TranscriptDelta` once the tool
result is folded into the follow-up response.

## See it work

The [`customer_service`](examples.md#customer_service--aria) example wires
`process_refund` and `connect_to_human`; the [`realtime_tools`](examples.md#realtime_tools)
example is a headless OpenAI probe with weather, calculator, and time tools.

## Which tools are governed

`IntegratedRealtimeRunner` routes tool calls by how the tool was registered:

| Registered as | Dispatch | Policy applied |
|---------------|----------|----------------|
| `adk_tool(...)` — an ADK `Tool` | The integration policy pipeline | Configured plugins, then the governed path on the rewritten arguments, transcript recording, tool-event persistence |
| A native realtime handler | `RealtimeRunner` dispatch | The governed path, when `tool_governance` is set |

`ToolBridgeAdapter` remains available for wrapping an ADK `Tool` as a native handler; a tool
wrapped that way is dispatched like any native handler.

### Plugin failures fail closed

If the `before_tool_call` pipeline returns an error, the tool is **refused**:

```json
{ "error": "tool guarded was refused: its before-tool plugin pipeline failed with tool.internal (tool.legacy). Execution is refused rather than proceeding without policy." }
```

Authorization, redaction, and policy live in before-tool plugins, so a broken guard refuses
the call instead of letting it run unguarded.

If the `after_tool_call` pipeline returns an error, the tool has already run, but its result
is **withheld**, since result redaction and screening live in after-tool plugins. The model
receives:

```json
{
  "status": "result_withheld",
  "error": "tool 'guarded' ran, but its after-tool pipeline failed with tool.internal (tool.legacy), so its result is withheld; do not call it again for this request."
}
```

The message states that the tool ran, so the model does not repeat a side effect. Both
messages name the plugin error's component, category, and code; its message is sent only with
`expose_tool_error_messages(true)`.

### The governed path

Every realtime dispatch runs the same governed path as `LlmAgent`
(`adk_core::authorize_tool_call`): the kill switch, the `ToolPolicy`, and confirmation, on the
call's final arguments.

| Runtime | Where the governance comes from |
|---------|---------------------------------|
| `RealtimeAgent` under a `Runner` | The run's `RunConfig`: `tool_policy`, `governance`, confirmation handler, approvals, approval store |
| `RealtimeRunner` | `RealtimeRunnerBuilder::tool_governance(run_config, approval_scope)` |
| `IntegratedRealtimeRunner` | `IntegratedRealtimeRunnerBuilder::tool_governance(run_config)`, scoped to the runner's identity |

```rust
use adk_core::{ApprovalScope, DeclarativePolicy, RunConfig};
use adk_realtime::RealtimeRunner;
use adk_realtime::model::BoxedModel;
use std::sync::Arc;
use std::time::Duration;

fn build(model: BoxedModel) -> adk_realtime::Result<RealtimeRunner> {
    RealtimeRunner::builder()
        .model(model)
        .tool_governance(
            RunConfig::builder()
                .tool_policy(Arc::new(DeclarativePolicy::builder().allow("get_weather").build()))
                .build(),
            ApprovalScope::new("voice", "user-1", "call-42"),
        )
        .tool_timeout(Duration::from_secs(30))
        .build()
}
```

| Situation | What the provider receives |
|-----------|----------------------------|
| Arguments are not a JSON object | `{"error": "Tool '<name>' was not run: its arguments are not valid JSON (...)"}` — the tool does not run with `{}` |
| The policy denies the call | `{"error": "Tool '<name>' denied by policy: <reason>"}` |
| The call needs confirmation and no decision exists | `{"error": "Tool '<name>' requires confirmation"}`; `RealtimeAgent` also emits an event carrying the `ToolConfirmationRequest`, and the request is recorded in the approval store |
| The tool runs past its timeout (default five minutes) | `{"error": "Tool '<name>' timed out after ..."}` |
| An ADK tool fails | `{"error": "tool '<name>' failed: <component>.<category> (<code>)"}` — the message stays out unless `expose_tool_error_messages(true)` is set |
| A native handler fails | `{"error": "tool '<name>' failed"}` — the message stays out unless `expose_tool_error_messages(true)` is set |
| The result screen withholds the result | `{"status": "result_withheld", "error": "tool '<name>' ran, but its result was withheld: <reason>; ..."}` |
| The kill switch is frozen | Nothing — the session ends with a `governance.frozen` error |

An error message can quote credentials, internal hosts, or upstream bodies, and the provider
can speak it to the user and the session stores it, so by default it reaches neither.
`expose_tool_error_messages(true)` on `RealtimeAgentBuilder`, `RealtimeRunnerBuilder`, or
`IntegratedRealtimeRunnerBuilder` sends it. Exposing a message to the model and logging it are
separate: every failure is logged by its content-free part (an `AdkError`'s
`diagnostics()`, a native handler error's kind), and the message is added only under the payload
rule — the run's `RunConfig::record_payloads` for `RealtimeAgent`, and the `record_payloads` of the
`tool_governance` config for `RealtimeRunner` and `IntegratedRealtimeRunner`. A runner built
without `tool_governance` never logs the message.

## Screening tool results

Each runtime takes a `ToolResultScreen` (`adk_core`) that sees every tool result before the
model and the session do, and can rewrite it or withhold it:

| Runtime | Builder method | Where the screen runs |
|---------|----------------|-----------------------|
| `RealtimeAgent` | `tool_result_screen(screen)` | After the after-tool hooks and callbacks, before the tool event is yielded and the result is sent |
| `RealtimeRunner` | `tool_result_screen(screen)` | On each native handler's result or error response, before `function_call_output` is sent |
| `IntegratedRealtimeRunner` | `tool_result_screen(screen)` | After the plugin pipeline, before the transcript records the call, the tool event is persisted, and the result is sent; native handlers as `RealtimeRunner` |

A screen that withholds a result, or returns `Err` because it cannot decide — its redaction
service is down, for example — replaces the result with a `result_withheld` response, so the
runtime fails closed. `adk-guardrail`'s `ToolGuardrailSet` is a screen, so the tool-result
guardrails `LlmAgent` runs through `tool_guardrails` apply here unchanged:

```rust
use adk_core::Tool;
use adk_guardrail::{PiiRedactor, ResultTextGuardrail, ToolGuardrailSet};
use adk_realtime::RealtimeAgent;
use adk_realtime::model::BoxedModel;
use std::sync::Arc;

fn build(model: BoxedModel, lookup_order: Arc<dyn Tool>) -> adk_core::Result<RealtimeAgent> {
    RealtimeAgent::builder("voice")
        .model(model)
        .tool(lookup_order)
        .tool_result_screen(Arc::new(
            ToolGuardrailSet::new().with(ResultTextGuardrail::new(PiiRedactor::new())),
        ))
        .build()
}
```

A hand-written screen implements `ToolResultScreen::screen_result(tool_name, args, result)`,
returning `Ok(result)` to deliver, possibly rewritten, or `Err(reason)` to withhold.

The session keeps running while a call is held, so a decision recorded in the approval store
authorizes the call when the model issues it again.

## Tool callbacks on the direct agent

`RealtimeAgent` applies before- and after-tool callbacks with the same contract as the
standard agent loop. A runner's plugin `before_tool`, `after_tool`, `before_agent`, and
`after_agent` callbacks run ahead of the agent's own:

| Callback returns | Effect |
|------------------|--------|
| `Ok(None)` | The tool runs |
| `Ok(Some(content))` from a *before* callback | The content becomes the result; **the tool does not run** |
| `Err(e)` from a *before* callback | The tool does not run, after-callbacks are skipped, and the result is `{"error": "tool '<name>' was not run: a before-tool callback failed with <component>.<category> (<code>)"}` |
| `Ok(Some(content))` from an *after* callback | The content replaces the tool's result |
| `Err(e)` from an *after* callback | The result is withheld: a `result_withheld` response telling the model the tool ran |

A callback that refuses on purpose returns its reason as `Ok(Some(content))`; an `Err` is a
failure, and its message reaches the model only with `expose_tool_error_messages(true)`.

A callback's `Content` is converted to the JSON result the provider expects: a
`FunctionResponse` part contributes its payload, anything else contributes its text under a
`result` key.

## Realtime tool context

A tool invoked from `RealtimeAgent` sees the same capabilities it sees under a `Runner`:

| Capability | Source |
|------------|--------|
| `user_scopes()` | The parent invocation context |
| `get_secret(name)` | The parent invocation context |
| `shared_state()` | The parent invocation context |
| `search_memory(query)` | The parent's memory service |
| Identity (`app_name`, `user_id`, `session_id`, `branch`) | The parent invocation context |

A tool bridged through `IntegratedRealtimeRunner` runs with a `RealtimeToolContext` instead
(see [Bridged tools](#bridged-tools-any-adk_coretool)).

## Tool concurrency

`RunnerConfig::max_concurrent_tools` (default 4) bounds how many tool handlers run at
once. When a response dispatches several calls, the runner queues each on its event loop
and admits it to execution as a permit frees:

```rust
use adk_realtime::RealtimeRunner;
use adk_realtime::model::BoxedModel;
use adk_realtime::runner::RunnerConfig;

fn build(model: BoxedModel) -> adk_realtime::Result<RealtimeRunner> {
    RealtimeRunner::builder()
        .model(model)
        .runner_config(RunnerConfig {
            auto_execute_tools: true,
            auto_respond_tools: true,
            max_concurrent_tools: 3,
        })
        .build()
}
```

Two properties follow:

- **Event intake continues during tool execution.** Audio deltas, transcripts, and
  interruptions are handled while tools run, so a handler that waits on something arriving
  later in the session does not deadlock it.
- **One follow-up response, after the last output.** When tool output is sent
  automatically, the model is owed a single `create_response`. It is issued once both the
  dispatching response has closed and every dispatched tool has reported — in either
  order, since a response can close while tools are still running.

> **Important:** the bound governs concurrency, not parallelism. Handlers share the
> runner's task, so a handler that blocks the thread — synchronous file or network I/O,
> heavy computation — still stalls the loop. Use `tokio::task::spawn_blocking` for those.

## Disconnect policy

The runner does not reconnect automatically. On transport loss it lets dispatched tools
finish, calls `EventHandler::on_disconnect`, and returns from `run`:

```rust
use adk_realtime::Result;
use adk_realtime::runner::EventHandler;

struct Reconnecting;

#[async_trait::async_trait]
impl EventHandler for Reconnecting {
    async fn on_disconnect(&self) -> Result<()> {
        tracing::warn!("realtime transport ended");
        Ok(())
    }
}
```

Register it with `RealtimeRunner::builder().event_handler(Reconnecting)`. A caller that pulls
events with `next_event` does not get `on_disconnect`; once `next_event` returns `None`,
`RealtimeRunner::disconnect_reason()` reports why the provider ended the stream.

Reconnection stays with the caller because it requires deciding what context to replay
and, on Gemini, whether a stored resumption token is still valid. The `on_disconnect` hook
exists so transport loss can be told apart from a graceful `close` — `run` returns
`Ok(())` for both.

Next: [Multimodal →](multimodal.md)
