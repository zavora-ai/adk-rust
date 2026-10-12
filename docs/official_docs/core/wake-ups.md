# Wake-ups and Webhooks

An agent wakes up when something outside it happens: a signed webhook arrives, a message lands
on a queue, or a schedule fires. Every wake-up becomes a run in the
[durable run store](durable-runtime.md), keyed so that a redelivery never creates a second run,
and every run executes under the same budget, policy, and kill switch as a run started by hand.
Outbound, `WebhookDelivery` sends signed notifications with retries and a dead-letter sink.

| Direction | Component | Crate (feature) |
|-----------|-----------|-----------------|
| Signing and verification | `webhook::{WebhookSigner, WebhookVerifier}` | `adk-core` (`webhooks`) |
| Inbound webhooks | `POST /hooks/{name}` — `WebhookIngress`, `WebhookHook` | `adk-server` (`webhook-ingress`) |
| Inbound queues | `QueueSource`, `QueueIngress`, `InMemoryQueueSource` | `adk-core` |
| Outbound webhooks | `webhook_delivery::WebhookDelivery` | `adk-core` (`webhook-delivery`) |
| AWP event subscriptions | `InMemoryEventSubscriptionService` delivers through `WebhookDelivery` | `adk-awp` |
| Approval notifications | `WebhookApprovalNotifier` delivers through `WebhookDelivery` | `adk-server` (`approvals`) |
| Ambient triggers | `StandardWebhookVerifier` for `WebhookTrigger` | `adk-agent` (`ambient`) |

## Standard Webhooks

Every signed delivery in ADK-Rust, inbound and outbound, uses
[Standard Webhooks](https://www.standardwebhooks.com/):

| Header | Value |
|--------|-------|
| `webhook-id` | The message identifier; identical on every retry of one message |
| `webhook-timestamp` | Unix seconds when this attempt was signed |
| `webhook-signature` | Space-separated `v1,<base64>` signatures; any one may match |

Each signature is HMAC-SHA256, keyed with the secret's bytes, over
`{webhook-id}.{webhook-timestamp}.{raw body}`.

| Guarantee | Mechanism |
|-----------|-----------|
| Authenticity | The verifier recomputes the HMAC and compares in constant time |
| Freshness | The timestamp must be within the tolerance (default five minutes) of the local clock, in either direction |
| Key rotation | `WebhookVerifier::with_secret` accepts several secrets and `WebhookSigner::with_secret` sends one signature per secret, so either side can rotate first |
| Replay protection | A replay outside the tolerance fails the timestamp check. Inside it, the webhook ingress returns the existing run for the `webhook-id`, and `WebhookReplayGuard` refuses the id for consumers without a run store |

```rust
use adk_core::webhook::{WebhookSecret, WebhookSigner, WebhookVerifier};

fn main() -> Result<(), adk_core::webhook::WebhookError> {
    let current = WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?;
    let next = WebhookSecret::from_bytes(b"the-next-signing-key-32-bytes!!!".to_vec())?;

    let body = br#"{"event":"invoice.paid"}"#;
    let signed = WebhookSigner::new(current.clone()).sign_now("msg_1", body);

    // During a rotation the receiver accepts both keys.
    let verifier = WebhookVerifier::new(current).with_secret(next);
    let verified = verifier.verify(&signed.as_headers(), body)?;
    assert_eq!(verified.id, "msg_1");
    Ok(())
}
```

> **Note:** verify the raw request body. Re-serialized JSON changes the bytes and fails
> verification.

`adk-anthropic` keeps its own Managed Agents verifier (`adk_anthropic::managed_agents::WebhookVerifier`):
that crate depends on no other ADK crate, so it does not take `adk-core` for this. Both
implementations pass the Standard Webhooks reference test vector.

## Inbound webhooks

Enable `webhook-ingress` on `adk-server` and register hooks with
`ServerBuilder::with_webhook_ingress`:

```toml
[dependencies]
adk-server = { version = "3.0.0", features = ["webhook-ingress"] }
```

```rust
use std::sync::Arc;

use adk_core::run_store::{DurableRunStore, InMemoryRunStore, OnConflict};
use adk_core::webhook::{WebhookSecret, WebhookVerifier};
use adk_server::webhooks::{WebhookHook, WebhookIngress};
use adk_server::{ServerBuilder, ServerConfig};

fn app(config: ServerConfig, runs: Arc<dyn DurableRunStore>) -> anyhow::Result<axum::Router> {
    let secret = WebhookSecret::from_base64(&std::env::var("GITHUB_WEBHOOK_SECRET")?)?;
    let ingress = WebhookIngress::new(runs).with_hook(
        WebhookHook::new("github", WebhookVerifier::new(secret), "triage", "triage_agent")
            .with_input_template(serde_json::json!({
                "text": "Triage issue #{{/payload/issue/number}}: {{/payload/issue/title}}"
            }))
            .with_concurrency_key("repo:{{/payload/repository/id}}", OnConflict::Enqueue)
            .with_max_pending(100),
    );
    Ok(ServerBuilder::new(config).with_webhook_ingress(ingress).build())
}
```

The route is mounted at the root, outside `/api` and without the auth middleware: the signature
is the authentication.

Woken runs are executed by the same `DurableWorker` that runs every other durable run. Share
one `DurableRunStore` between the ingress and the background runs (feature `background`), and
the server's embedded worker claims them:

```rust
use std::sync::Arc;

use adk_core::run_store::{DurableRunStore, InMemoryRunStore};
use adk_runner::Runner;
use adk_server::background::BackgroundState;
use adk_server::webhooks::{WebhookHook, WebhookIngress};
use adk_server::{ServerBuilder, ServerConfig};

fn app(config: ServerConfig, runner: Arc<Runner>, hook: WebhookHook) -> axum::Router {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    ServerBuilder::new(config)
        .with_background_runs(
            BackgroundState::new().with_run_store(runs.clone()).with_agent("triage_agent", runner),
        )
        .with_webhook_ingress(WebhookIngress::new(runs).with_hook(hook))
        .build()
}
```

### What the endpoint does

| Step | Refusal | Status |
|------|---------|--------|
| Look up the hook by name | Unknown hook | `404` |
| Verify the signature and timestamp | Missing, malformed, expired, or mismatched | `401`; the reason is logged, never returned |
| Check the kill switch | Frozen, or a replicated control that is stale | `503` with `Retry-After` |
| Check the hook's pending-run cap | `max_pending` unfinished runs from this hook | `429` with `Retry-After` |
| Parse the body and render the mapping | Not JSON, or a key template with a missing field | `422` |
| Enqueue with idempotency key `hook:{name}:{webhook-id}` | Run store unreachable | `503` with `Retry-After` |
| | `OnConflict::Reject` and an unfinished run holds the concurrency key | `409` with `blockingRunId` |

A new run answers `202 Accepted` with `{"runId", "state", "duplicate": false}`. A redelivery
of a `webhook-id` that already has a run answers `200 OK` with that run and
`"duplicate": true`, whether the run is queued, running, or finished, until the run store
prunes it. The kill switch is `ServerConfig::governance` unless the ingress sets its own with
`WebhookIngress::with_governance`.

### Mapping a delivery to a run

| Field | Default | Builder |
|-------|---------|---------|
| Queue | `default` | `with_queue` |
| App and target | Required | `WebhookHook::new(name, verifier, app_name, target)` |
| User | `webhook:{name}` | `with_user_id(template)` |
| Session | A new session per run | `with_session_id(template)` |
| Input | The JSON body | `with_input_template(json)` |
| Concurrency key and `OnConflict` | None | `with_concurrency_key(template, on_conflict)` |
| Attempts | `NewRun::DEFAULT_MAX_ATTEMPTS` | `with_max_attempts` |
| Everything | — | `with_mapper(Fn(&HookDelivery) -> Result<NewRun>)`; the idempotency key is still the `webhook-id` |

Templates resolve JSON pointers against
`{"payload": <body>, "webhook": {"id", "timestamp"}, "hook": {"name"}}`. A string that is
exactly `{{/pointer}}` becomes the pointed value; placeholders inside a longer string become
text. In the user, session, and concurrency-key templates a missing field refuses the delivery
with `422`, so unrelated deliveries never share a key.

## Queue wake-ups

A `QueueSource` is a pull-based, at-least-once message source: `pull` leases a batch, and each
message is then acked (removed) or nacked (redelivered later). A message that is neither is
redelivered when its lease lapses.

`QueueIngress` turns each message into a run:

| Step | Effect |
|------|--------|
| Governance | Nothing is pulled while the kill switch refuses work; a freeze that lands mid-batch nacks the rest |
| Back-pressure | With `max_pending`, a batch never exceeds the room left under that many unfinished runs from the source |
| Map | The mapper builds a `NewRun`; a mapping error nacks the message |
| Enqueue | Idempotency key `queue:{source}:{message id}` — a redelivered or duplicated message returns the existing run |
| Ack | Only after the enqueue succeeded. A failed enqueue, or one `OnConflict::Reject` refused, nacks the message for redelivery after `retry_after` |

```rust
use std::sync::Arc;
use std::time::Duration;

use adk_core::queue_source::{InMemoryQueueSource, QueueIngress};
use adk_core::run_store::{InMemoryRunStore, NewRun};
use adk_core::GovernanceControl;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let source = Arc::new(InMemoryQueueSource::new("orders"));
    let runs = Arc::new(InMemoryRunStore::new());
    let ingress = QueueIngress::new(source.clone(), runs, |message| {
        Ok(NewRun::new("default", "shop", "system", "order_agent", message.body_json()?))
    })
    .with_governance(GovernanceControl::new())
    .with_max_pending(50);

    source.publish("msg-1", br#"{"text": "Ship order 7"}"#.to_vec());
    let report = ingress.poll_once().await?;
    assert_eq!(report.created.len(), 1);

    // Or poll until shutdown.
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        ingress.run(Duration::from_millis(500), async { let _ = stopped.await; }).await;
    });
    stop.send(()).ok();
    task.await.ok();
    Ok(())
}
```

### Brokers

`InMemoryQueueSource` ships with `adk-core`. A broker adapter implements `QueueSource` and
proves the contract with `adk_core::queue_source_conformance_tests!` (feature `conformance`):

| Broker | `pull` | `ack` | `nack` | Message id |
|--------|--------|-------|--------|------------|
| Amazon SQS | `ReceiveMessage` (long poll) | `DeleteMessage` with the receipt handle | `ChangeMessageVisibility` | `MessageId`, or the FIFO `MessageDeduplicationId` |
| Google Pub/Sub | `Pull` / streaming pull | `Acknowledge` with the `ackId` | `ModifyAckDeadline` | `messageId` |
| NATS JetStream | Pull consumer `fetch` | `Ack` | `Nak` with delay | `Nats-Msg-Id` header, or stream sequence |
| Kafka | Consumer `poll` | Commit the offset | Seek back, or a retry topic | `topic/partition/offset` or a producer key |
| Redis Streams | `XREADGROUP` | `XACK` | Leave pending; `XAUTOCLAIM` redelivers | The entry id |

No broker adapter ships in this release. Each broker needs its own client library and a live
broker in CI, so adapters are separate optional crates rather than features of `adk-core`.

## Governance and budgets

| Control | Webhook ingress | Queue ingress | The run itself |
|---------|-----------------|---------------|----------------|
| Kill switch frozen | `503`, nothing enqueued | Nothing pulled; mid-batch messages nacked | Refused at start like any run |
| Replicated control stale | `503` (`governance.stale`) | Nothing pulled | Refused at start |
| Freeze written by another replica | `503` once this replica's mirror reads it (within the poll interval) | Nothing pulled | The worker claims nothing |
| Too much pending work | `429` past `max_pending` | Pulls shrink, then stop, at `max_pending` | — |
| Run budget, spend ledger, tool policy | — | — | Applied by the runner that executes the run |

A wake-up adds no execution path of its own: the worker that claims the run executes it with
the runner's `RunConfig`, so budgets, spend caps, tool policy, and approvals apply exactly as
they do to a run started through `POST /api/runs`.

Every run a hook or queue ingress creates acts for its user as a service
(`NewRun::with_service_subject`): its policy requests, approvals, and audit records name
`service:<user>` rather than a person. See
[Agent Identity](../security/agent-identity.md#durable-runs).

## Outbound webhooks

`WebhookDelivery` (feature `webhook-delivery` on `adk-core`) sends one signed message to one
endpoint:

| Step | Behaviour |
|------|-----------|
| Egress check | HTTPS only, and the host must not be, or resolve to, a loopback, private, link-local, CGNAT, multicast, or IPv4-embedding IPv6 address. Resolution runs inside the HTTP client, so the address checked is the address connected to; redirects are never followed and system proxies are bypassed |
| Signing | Each attempt is signed under the message's stable `webhook-id` with a fresh timestamp |
| Success | Any `2xx` |
| Retry | Connection errors, timeouts, `408`, `425`, `429`, and `5xx`: exponential backoff with full jitter, raised to `Retry-After` up to the backoff cap |
| Dead letter | Any other status, an egress refusal, or the last failed attempt; the message and its last error go to the `DeadLetterSink` |

```rust
use std::sync::Arc;

use adk_core::webhook::WebhookSecret;
use adk_core::webhook_delivery::{
    InMemoryDeadLetterSink, RetryPolicy, WebhookDelivery, WebhookEndpoint, WebhookMessage,
};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dead_letters = Arc::new(InMemoryDeadLetterSink::new());
    let delivery = WebhookDelivery::new()
        .with_retry_policy(RetryPolicy::default().with_max_attempts(6))
        .with_dead_letter_sink(dead_letters.clone());

    // A run-completion callback; an approval notifier is the same call with its own payload.
    let endpoint = WebhookEndpoint::new(
        "https://hooks.example.com/runs",
        WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?,
    );
    let message = WebhookMessage::json(
        "run_42.completed",
        &json!({"runId": "run_42", "state": "completed"}),
    )?;
    let report = delivery.deliver(&endpoint, &message).await;
    println!("delivered: {}", report.is_delivered());
    Ok(())
}
```

`EgressPolicy::allow_private_networks()` lifts the address and scheme checks for receivers an
operator configures inside a private network. Do not use it when callers register their own
URLs.

### AWP event subscriptions

`adk_awp::InMemoryEventSubscriptionService` delivers each event to every matching subscription
through `WebhookDelivery`: the event id is the `webhook-id`, and the subscription's secret bytes
are the signing key (`WebhookSecret::from_bytes(secret).to_whsec()` is the receiver's
`whsec_` form). `deliver` starts the deliveries in the background and returns;
`deliver_and_wait` returns each subscription's `DeliveryReport`. Subscriptions whose callback URL
the delivery's egress policy refuses are rejected when they are created.

## Live validation

`examples/autonomy_validation` scenario `webhook_wakeup` runs against OpenAI and Anthropic: a
forged signature answers `401`; a freeze written by another replica through a shared
governance store answers `503` and enqueues nothing; after the unfreeze a signed delivery answers
`202`, and the server's embedded `DurableWorker` runs the woken agent, which calls its tool once
and completes; replaying the captured delivery, and a sender retry re-signed under the
same `webhook-id`, both answer `200` with the same run, and no second run, claim, or model call
follows.

```bash
cargo run --manifest-path examples/autonomy_validation/Cargo.toml -- --provider both --scenario webhook_wakeup
```
