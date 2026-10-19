# Ambient Agents

Ambient agents react to events instead of to a user turn. An `EventSource` produces
`TriggerEvent`s and the agent runs for each one.

Requires the `ambient` feature:

```toml
[dependencies]
adk-agent = { version = "3.0.0", features = ["ambient"] }
```

## Running an ambient agent

A trigger handler is required. The supported path is `with_invoker`, which takes anything
implementing `adk_core::AgentInvoker` — `Runner` does:

```rust
use adk_agent::ambient::{AmbientAgent, RunnerTriggerConfig};
use std::sync::Arc;

// `agent` is the Arc<dyn Agent> to run, `source` an Arc<dyn EventSource>, and `runner` an
// Arc<Runner> over the same agent.
let mut ambient = AmbientAgent::new(agent, source)
    .with_invoker(runner, RunnerTriggerConfig::new("system"))
    .with_max_concurrent_triggers(4);

let mut outputs = ambient.take_output(64);
ambient.start().await?;
```

`RunnerTriggerConfig` controls three things:

| Method | Purpose | Default |
|--------|---------|---------|
| `new(user_id)` | Identity the runs are recorded under. A trigger has no interactive user, so use `"system"` or a service account name. | required |
| `with_session_policy` | `TriggerSessionPolicy::PerTrigger` gives each event its own session; `Shared(id)` reuses one. | `PerTrigger` |
| `with_prompt` | Turns the event into prompt text. | states the source and serializes the payload |

`PerTrigger` is the default because a schedule firing every minute into one shared session grows
that session's history — and the token cost of every later run — without bound.
`Runner` serializes externally invoked turns targeting the same shared session until each event
stream finishes, while different session IDs can still run concurrently.

`AgentInvoker::invoke` creates the session when it does not exist. `Runner::run` does not: it
resolves an *existing* session and yields `session.not_found` through the stream otherwise, which
an externally triggered run has no opportunity to pre-register.

When the invoker exposes its executable root, as `Runner` does, `with_invoker` uses that agent for
ambient logging and diagnostics. This prevents telemetry from naming one agent while another one
actually handles the trigger.

### Supplying a handler directly

`with_trigger_handler` remains available for callers driving something other than a `Runner`. It
receives the event and the agent and must return the event stream; creating the session is then
the handler's responsibility.

```rust
use adk_agent::ambient::{AmbientAgent, TriggerHandler};
use std::sync::Arc;

let handler: TriggerHandler = Arc::new(move |event, agent| {
    let backend = backend.clone();
    Box::pin(async move { backend.dispatch(event, agent).await })
});

let mut ambient = AmbientAgent::new(agent, source).with_trigger_handler(handler);
```

`start` fails without a handler:

```text
AmbientAgent has no trigger handler, so starting it would log trigger events without ever
invoking the agent. Call `with_trigger_handler` with a closure that drives the agent through a
Runner.
```

## Output and concurrency

| Behaviour | Control |
|-----------|---------|
| Events and errors the agent produces are delivered to a channel | `take_output(capacity)` |
| Triggers handled at once | `with_max_concurrent_triggers` (default 4, zero treated as one) |

> **Note:** the bound governs concurrency, not parallelism. Handlers share the ambient task, so a
> handler that blocks the thread still stalls the loop. Use `tokio::task::spawn_blocking` for
> those. Durable trigger offsets, dead-letter handling, and retry remain the caller's
> responsibility.

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

```rust
use std::sync::Arc;
use adk_agent::ambient::{CronTrigger, FileTickWatermark, MissedTickPolicy};

let trigger = CronTrigger::new("0 */5 * * * *")?
    .with_missed_tick_policy(MissedTickPolicy::CoalesceOne)
    .with_watermark(Arc::new(FileTickWatermark::new("/var/lib/my-agent/sweep.tick")));
```

`FileTickWatermark` stores one RFC 3339 cursor. It writes through a unique sibling temporary file,
synchronizes it, and atomically replaces the destination on Unix and Windows. Implement
`TickWatermark` for other backing stores.

### Bounding a replay

`All` on a frequent schedule can leave thousands of ticks outstanding after a long outage.
`with_max_catch_up` caps how many one pass replays (default 64); once the cap is reached the
remainder of the gap is discarded, the durable cursor advances past it, and the trigger resumes at
the next future tick, logging how many ticks were dropped. Restarting before the next ordinary tick
does not recover the discarded remainder.

### Delivery contract

The watermark advances when the trigger emits a tick, not when the consumer finishes acting on
it. A crash between emission and completion drops that run rather than repeating it —
at-most-once, not at-least-once. This is what stops a consumer that stops polling from replaying
the same gap on every restart. Consumers whose work must survive a mid-run crash should record
their own completion state. If a configured watermark cannot be persisted, the cron stream stops
before emitting the affected event rather than silently weakening this guarantee.

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

For senders that sign with [Standard Webhooks](../core/wake-ups.md), use
`StandardWebhookVerifier`: it checks the `webhook-timestamp` against the tolerance and the
`webhook-signature` against every configured secret, refuses a replayed `webhook-id`, and
attributes every accepted request to the principal it was built with:

```rust
use adk_agent::ambient::{StandardWebhookVerifier, WebhookTrigger};
use adk_core::webhook::{WebhookSecret, WebhookVerifier};
use std::sync::Arc;

let secret = WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?;
let trigger = WebhookTrigger::new(8080, "/hooks/ci")
    .with_verifier(Arc::new(StandardWebhookVerifier::new("ci", WebhookVerifier::new(secret))));
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

```rust
let stream = trigger.subscribe().await?;
// ... consume events ...
drop(stream); // the listener stops and the port is free
```
