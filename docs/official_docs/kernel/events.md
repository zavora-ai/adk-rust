# The Event Bus and Outbound Webhooks

Every state change in the [agent kernel](index.md) is a `KernelEvent` on its event bus
(spec 3.2 §7.6): a process changing state, a wake, a held call and its decision, a standing
rule's hit, spend, an agenda item, a driver, an observation, the kill switch and health. The
bus numbers each event with a cursor and delivers it to subscribers and sinks. Publishing never
waits on any of them, so a slow receiver never holds up a process.

## Event types

| `type` | Published when |
|---|---|
| `process` | A process changes state, with the reason, such as a park |
| `wake` | A wake event is routed: the process it woke or spawned, its key and outcome |
| `held` | A process holds a tool call for approval; carries the call, its arguments included |
| `decided` | A held call is decided, with the decision and who made it |
| `ruleHit` | A standing rule allows a call |
| `spend` | Spend is committed against a partition |
| `agenda` | An agenda item changes state, or a proposal is refused |
| `review` | The daily review reports what is older than the review period |
| `driver` | A driver's state changes, `needsReapproval` included |
| `observation` | The baseline pass records an observation |
| `governance` | The kill switch is frozen or lifted |
| `health` | A health report; the first event a slow sink drops |

`EventKind` names these types in code; `KernelEvent::kind` gives an event's.

## Receivers

| Receiver | Delivery | When it falls behind |
|---|---|---|
| `adk-rust events`, `GET /api/kernel/events` | Server-sent events from a cursor ([control plane](control-plane.md#routes)); a reconnecting surface catches up from its last cursor | Skips to the oldest event the bus still holds |
| An `EventSink`: `AuditSink`, the activity ledger's sink, `WebhookSink`, or your own added with `KernelBuilder::event_sink` | Its own bounded queue, drained in order | Drops a queued `health` first, then its oldest event |

## Outbound webhooks

`WebhookSink` (feature `webhooks`, on by default) posts the events it selects to an HTTP
endpoint, signed with [Standard Webhooks](https://www.standardwebhooks.com/) signatures —
the scheme the kernel's webhook ingress verifies:

```rust
use std::sync::Arc;

use adk_core::InMemoryRunStore;
use adk_core::webhook::WebhookSecret;
use adk_core::webhook_delivery::{InMemoryDeadLetterSink, RetryPolicy};
use adk_kernel::{EventFilter, EventKind, Kernel, KernelConfig, WebhookSink};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = WebhookSecret::from_base64(&std::env::var("KERNEL_WEBHOOK_SECRET")?)?;
    let dead_letters = Arc::new(InMemoryDeadLetterSink::new());
    let sink = WebhookSink::new("https://hooks.example.com/kernel", secret)?
        .with_filter(EventFilter::only([EventKind::Held, EventKind::Decided, EventKind::Governance]))
        .with_retry_policy(RetryPolicy::default().with_max_attempts(5))
        .with_dead_letter_sink(dead_letters.clone());

    let kernel = Kernel::builder()
        .config(KernelConfig::default())
        .run_store(Arc::new(InMemoryRunStore::new()))
        .event_sink(Arc::new(sink))
        .build()?;
    kernel.start().await?;
    kernel.shutdown_on_signal().await?;
    Ok(())
}
```

### What the receiver gets

Each delivery is a `POST` with a JSON body:

```json
{
  "type": "governance",
  "timestamp": "2026-10-25T09:14:03.201Z",
  "data": {
    "cursor": 42,
    "at": "2026-10-25T09:14:03.201Z",
    "event": { "type": "governance", "frozen": true, "reason": "incident 12" }
  }
}
```

`data` is the record `GET /api/kernel/events` streams for the same event.

| Header | Value |
|---|---|
| `webhook-id` | `evt_<uuid>`: one per event, the same on every retry, so the receiver deduplicates on it |
| `webhook-timestamp` | Unix seconds when this attempt was signed |
| `webhook-signature` | `v1,<base64>`: HMAC-SHA256 with the secret over `{webhook-id}.{webhook-timestamp}.{raw body}` |

A receiver verifies the raw body with `adk_core::webhook::WebhookVerifier`, or with any
Standard Webhooks library; the verifier rejects a timestamp more than five minutes from its
clock.

### Selection, retries and back-pressure

| Concern | Behaviour |
|---|---|
| Selection | `EventFilter`: `default()` is every type but `health`; `only([...])`, `all()`, `without(kind)`; `EventFilter::parse(["held", "decided"])` refuses an unknown type name |
| Retry | `RetryPolicy` (default 6 attempts): connection errors, timeouts, `408`, `425`, `429` and `5xx` are retried with exponential backoff and full jitter, honouring `Retry-After`; redirects are never followed |
| Undeliverable | Any other status, or the last failed attempt, puts the event in the `DeadLetterSink` with its last status and error; without one the event is dropped with a warning |
| Back-pressure | `with_max_in_flight(n)` deliveries run at once, each on its own task; the default is 1, so events arrive in bus order. Past the limit the bus queues the sink's events and, when the queue is full, drops `health` first and then the oldest |
| Order | With more than one in flight, a later event may arrive before an earlier one being retried; order by `data.cursor` |

### The destination and its secret

| Rule | Why |
|---|---|
| `https`, or plain `http` to `localhost`, `*.localhost` or a loopback address only | The events leave the machine signed but not encrypted otherwise |
| No credentials in the URL | The signature authenticates each delivery |
| Logs name the endpoint's origin only, never its path, query or the secret | A path or query may carry a token |

Keep the secret in the environment or a secret store, never in source. A `held` event carries
the held call's arguments: leave `EventKind::Held` out of the filter for a receiver that should
not see them.

## Read next

- [The control plane](control-plane.md): `GET /api/kernel/events` and its cursor
- [Kernel commands](cli.md#adk-rust-events): `adk-rust events [--follow]`
- [Self-observation: `/proc`](proc.md): the activity ledger the ledger's sink writes
