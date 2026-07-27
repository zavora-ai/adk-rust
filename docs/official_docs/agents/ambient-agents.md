# Ambient Agents

## Running an ambient agent

A trigger handler is required. It receives the event and the agent, and drives the agent —
typically through a `Runner`:

```rust,ignore
use adk_agent::ambient::{AmbientAgent, TriggerHandler};
use std::sync::Arc;

let handler: TriggerHandler = Arc::new(move |event, agent| {
    let runner = runner.clone();
    Box::pin(async move {
        runner.run_str("user-1", "ambient-session", event.payload.to_string().into()).await
    })
});

let mut ambient = AmbientAgent::new(agent, source)
    .with_trigger_handler(handler)
    .with_max_concurrent_triggers(4);

let mut outputs = ambient.take_output(64);
ambient.start().await?;
```

`start` fails without a handler:

```text
AmbientAgent has no trigger handler, so starting it would log trigger events without ever
invoking the agent. Call `with_trigger_handler` with a closure that drives the agent through a
Runner.
```

> **Important:** starting without a handler previously succeeded and then logged each trigger,
> so `AmbientAgent::new(..).start()` looked like it was running an agent that never ran.

## Output and concurrency

| Behaviour | Control |
|-----------|---------|
| Events and errors the agent produces are delivered to a channel | `take_output(capacity)` |
| Triggers handled at once | `with_max_concurrent_triggers` (default 4, zero treated as one) |

Produced events were previously logged at debug level and dropped, so a caller could not observe
what a run did or whether it failed. Triggers were also handled strictly one at a time — the loop
drained a handler's entire event stream before polling the source again — so one slow trigger
blocked every later one.

> **Note:** the bound governs concurrency, not parallelism. Handlers share the ambient task, so a
> handler that blocks the thread still stalls the loop. Use `tokio::task::spawn_blocking` for
> those. Durable trigger offsets, dead-letter handling, and retry remain the caller's
> responsibility.
