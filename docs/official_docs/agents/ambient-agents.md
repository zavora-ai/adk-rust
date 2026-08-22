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
Ambient agents react to events instead of to a user turn. An `EventSource` produces
`TriggerEvent`s and the agent runs for each one.

Requires the `ambient` feature:

```toml
[dependencies]
adk-agent = { version = "2.0.0", features = ["ambient"] }
```

## Event sources

| Source | Fires on | Principal |
|--------|----------|-----------|
| `CronTrigger` | A cron schedule | `None` — a schedule has no caller |
| `FileWatchTrigger` | A matching filesystem change | `None` — a file change has no caller |
| `WebhookTrigger` | An authorized HTTP POST | The verifier's result |

`TriggerEvent::principal` lets a handler distinguish an authorized trigger from an anonymous
one rather than treating every event as equally trusted.

## Missed ticks

`CronTrigger::subscribe` computes the next tick from the moment it is called. A trigger that
restarts after downtime, or runs on a host that suspends, therefore resumes at the next future
tick and every tick that came due in between is discarded.

`MissedTickPolicy` decides what happens to that span:

| Policy | Behaviour | Use for |
|--------|-----------|---------|
| `Skip` | Discard elapsed ticks and wait for the next scheduled one. The default. | Schedules where a late run has no value |
| `CoalesceOne` | Emit one event covering the whole elapsed span. | Sweeps where only current state matters |
| `All` | Emit one event per elapsed tick, oldest first. | Schedules where each occurrence has its own work |

A policy alone only covers gaps inside one subscription. Detecting a gap that spans a process
restart needs a `TickWatermark` to record where the schedule left off:

```rust,ignore
use std::sync::Arc;
use adk_agent::ambient::{CronTrigger, FileTickWatermark, MissedTickPolicy};

let trigger = CronTrigger::new("0 */5 * * * *")?
    .with_missed_tick_policy(MissedTickPolicy::CoalesceOne)
    .with_watermark(Arc::new(FileTickWatermark::new("/var/lib/my-agent/sweep.tick")));
```

`FileTickWatermark` stores one RFC 3339 timestamp, writing through a temporary file and renaming
so a crash mid-write leaves the previous watermark intact. Implement `TickWatermark` for other
backing stores.

### Bounding a replay

`All` on a frequent schedule can leave thousands of ticks outstanding after a long outage.
`with_max_catch_up` caps how many one pass replays (default 64); once the cap is reached the
remainder of the gap is discarded and the trigger resumes at the next future tick, logging how
many ticks were dropped.

### Delivery contract

The watermark advances when the trigger emits a tick, not when the consumer finishes acting on
it. A crash between emission and completion drops that run rather than repeating it —
at-most-once, not at-least-once. This is what stops a consumer that stops polling from replaying
the same gap on every restart. Consumers whose work must survive a mid-run crash should record
their own completion state.

## Webhook triggers

A reachable webhook is a remote entry point into application logic, so `WebhookTrigger`
defaults to loopback and refuses to serve a wider address without a verifier.

### Local development

```rust
use adk_agent::ambient::WebhookTrigger;

// Binds 127.0.0.1 — reachable only from this host.
let trigger = WebhookTrigger::new(8080, "/webhook");
```

### Exposed listeners require a verifier

```rust
use adk_agent::ambient::{WebhookRequest, WebhookTrigger, WebhookVerifier};
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Debug)]
struct SharedToken(String);

impl WebhookVerifier for SharedToken {
    fn verify(&self, request: &WebhookRequest<'_>) -> Result<String, String> {
        match request.header("x-webhook-token") {
            Some(value) if value == self.0 => Ok("ci-system".to_string()),
            Some(_) => Err("token mismatch".to_string()),
            None => Err("missing x-webhook-token".to_string()),
        }
    }
}

let address: SocketAddr = "0.0.0.0:8080".parse().unwrap();
let trigger = WebhookTrigger::new(8080, "/webhook")
    .with_bind_address(address)
    .with_verifier(Arc::new(SharedToken(std::env::var("WEBHOOK_TOKEN").unwrap_or_default())));
```

Subscribing to a non-loopback address without a verifier fails with
`agent.ambient.webhook_unauthenticated`. The check happens at subscribe time, where the
mistake is still cheap.

> **Important:** a verifier receives the raw body via `WebhookRequest::body`, because
> signature schemes are computed over the exact bytes received. Verify before trusting any
> parsed form of the request.

### Request handling

| Condition | Response | Event |
|-----------|----------|-------|
| Body over the limit (1 MiB default, `with_max_body_bytes`) | `413` | none |
| Verifier rejects | `401` | none |
| Body is not valid JSON | `400` | none |
| Body is not valid JSON, `accept_non_json()` set | `200` | payload is a JSON string |
| Subscriber has gone away | `503` | none |
| Otherwise | `200` | payload is the parsed JSON |

A `401` carries no detail about which part of a credential failed; the reason is logged
instead, so the endpoint cannot be used to probe credentials.

`accept_non_json` is off by default because a malformed body wrapped as a string produces a
trigger event indistinguishable from a deliberate one.

### Lifetime

The HTTP listener belongs to the stream returned by `subscribe`. Dropping the stream shuts
the server down gracefully and releases the port, so the same port can be rebound:

```rust,ignore
let stream = trigger.subscribe().await?;
// ... consume events ...
drop(stream); // the listener stops and the port is free
```

> **Note:** the listener previously outlived its consumer. Dropping the stream left the
> server bound, still accepting requests it could not deliver, and a restart on the same port
> failed.
