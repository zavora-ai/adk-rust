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
| Google push triggers | `POST /triggers/{pubsub,eventarc}/{name}` — `GooglePushIngress`, `PushTrigger`, `GoogleOidcVerifier` | `adk-server` (`google-triggers`) |
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

### Forge signatures

A repository host that cannot sign Standard Webhooks authenticates a hook with its own push
signature: build the hook with `WebhookHook::forge(name, signature, app_name, target)`.

| `ForgeSignature` | Checks | The run's `webhook-id` |
|------------------|--------|------------------------|
| `GitHub(ForgeSecret)` | `X-Hub-Signature-256`: HMAC-SHA256 of the raw body, in constant time | `X-GitHub-Delivery` |
| `GitLab(ForgeSecret)` | `X-Gitlab-Token` equal to the secret token, in constant time | `X-Gitlab-Event-UUID`, else `Idempotency-Key` |

Neither forge signs a timestamp: a redelivery under the same id returns the existing run, and
a consumer deduplicates by what the delivery carries, as `adk-website`'s content repository
does by commit ([Publishing from Git](../deployment/website-repository.md)).

## Google Pub/Sub and Eventarc triggers

A Pub/Sub push subscription or an Eventarc trigger delivers to an HTTPS endpoint with a
Google-signed OIDC token. Enable `google-triggers` on `adk-server` and register triggers with
`ServerBuilder::with_google_push`:

```toml
[dependencies]
adk-server = { version = "3.0.0", features = ["google-triggers", "background"] }
```

```rust
use std::sync::Arc;

use adk_core::run_store::{DurableRunStore, InMemoryRunStore};
use adk_runner::Runner;
use adk_server::background::BackgroundState;
use adk_server::google_triggers::{GoogleOidcVerifier, GooglePushIngress, PushTrigger};
use adk_server::{ServerBuilder, ServerConfig};

fn app(config: ServerConfig, runner: Arc<Runner>) -> anyhow::Result<axum::Router> {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    // The audience is the push endpoint URL, unless the subscription sets a custom one.
    let orders = GoogleOidcVerifier::new(
        "https://agents.example.com/triggers/pubsub/orders",
        ["orders-push@shop-prod.iam.gserviceaccount.com"],
    )?;
    let uploads = GoogleOidcVerifier::new(
        "https://agents.example.com/triggers/eventarc/uploads",
        ["eventarc-trigger@shop-prod.iam.gserviceaccount.com"],
    )?;
    let ingress = GooglePushIngress::new(runs.clone())
        .with_trigger(PushTrigger::pubsub("orders", orders, "shop", "order_agent"))
        .with_trigger(PushTrigger::eventarc("uploads", uploads, "shop", "order_agent"));
    Ok(ServerBuilder::new(config)
        .with_background_runs(
            BackgroundState::new().with_run_store(runs).with_agent("order_agent", runner),
        )
        .with_google_push(ingress)
        .build())
}
```

On Google Cloud, create the push subscription with authentication:

```bash
gcloud pubsub subscriptions create orders-push \
  --topic=orders \
  --push-endpoint=https://agents.example.com/triggers/pubsub/orders \
  --push-auth-service-account=orders-push@shop-prod.iam.gserviceaccount.com
```

The routes are mounted at the root, outside `/api`, with no auth middleware: the token is the
authentication. They are host-checked like every route, so add the service's hostname to
`SecurityConfig::allowed_hosts`.

### Verifying the token

| Check | Refusal |
|-------|---------|
| `Authorization: Bearer <token>` present | `401` |
| RS256 signature by a key in Google's JWK set (`https://www.googleapis.com/oauth2/v3/certs`), fetched once and cached for an hour | `401` |
| Issuer `https://accounts.google.com` or `accounts.google.com`; `exp` and `nbf` with a minute of leeway | `401` |
| Audience equal to the trigger's | `401` |
| `email` in the trigger's allowlist and `email_verified` | `403` |

Any Google account can mint an ID token for any audience, so the audience alone admits
anyone: `GoogleOidcVerifier::new` refuses an empty allowlist. The reason for a refusal is
logged, never returned. `GoogleOidcVerifier::with_jwks_cache` reads the keys from another
`JwksCache`, such as a mirror.

### Acknowledgement

Pub/Sub and Eventarc take a `2xx` as an acknowledgement and redeliver anything else, so a
delivery is acknowledged only once its run is durably enqueued:

| Step | Refusal | Status |
|------|---------|--------|
| Look up the trigger by kind and name | Unknown, or a trigger of the other kind | `404` |
| Verify the token | See above | `401` or `403` |
| Check the kill switch | Frozen, or a replicated control that is stale | `503` with `Retry-After`; the source redelivers |
| Decode the delivery | Not a Pub/Sub push body; a CloudEvent without `id`, `source` or `type` | `400` |
| Map it to a run | The mapper refuses | `422` |
| Enqueue with the idempotency key below | Run store unreachable | `503` with `Retry-After` |
| | `OnConflict::Reject` and an unfinished run holds the concurrency key | `409` with `blockingRunId` |

| Source | Idempotency key | Unique because |
|--------|-----------------|----------------|
| Pub/Sub | `pubsub:{trigger}:{subscription}:{messageId}` | A message id is unique within its topic, and a subscription reads one topic |
| Eventarc | `eventarc:{trigger}:{source}:{id}` | A CloudEvent id is unique within its source |

A new run answers `202 Accepted` with `{"runId", "state", "duplicate": false, "key"}`. A
redelivery answers `200 OK` with the same run and `"duplicate": true`, so the agent runs once
however often Pub/Sub delivers, until the run store prunes the run.

### Deliveries

| Route | Body |
|-------|------|
| `POST /triggers/pubsub/{name}` | The wrapped push body: `{"message": {"data", "attributes", "messageId", "publishTime"}, "subscription"}`. A subscription with payload unwrapping is not supported |
| `POST /triggers/eventarc/{name}` | A CloudEvent in binary mode (`ce-id`, `ce-source`, `ce-type`, optional `ce-subject` and `ce-time`; the body is the data) or structured mode (`Content-Type: application/cloudevents+json`, `data` or `data_base64`). A `google.cloud.pubsub.topic.v1.messagePublished` event's Pub/Sub message is unwrapped |

The data becomes JSON when it parses as JSON, else UTF-8 text, else `{"base64": ...}`.

### The run

| Field | Default | Builder |
|-------|---------|---------|
| Queue | `default` | `with_queue` |
| App and target | Required | `PushTrigger::pubsub(name, verifier, app_name, target)`, `PushTrigger::eventarc(..)` |
| User | `pubsub:{name}` or `eventarc:{name}` | `with_user_id` |
| Input | `{"text": <notice>, "pushEvent": <event>}` | — |
| Attempts | `NewRun::DEFAULT_MAX_ATTEMPTS` | `with_max_attempts` |
| Everything | — | `with_mapper(Fn(&PushDelivery) -> Result<NewRun>)`; the idempotency key is still the delivery's |

The notice tells the agent where the event came from and carries the event — id, scope,
type, attributes, data and the verified sender — as one line of JSON between `<<<push-event`
and `push-event>>>`, marked as data the agent takes no instructions from. The run acts for its
user as a service, not as an authenticated user: the verified service account is the sender
of an event, not a person, so standing rules and Auto stay inert for it.

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

Google push triggers answer a frozen or stale kill switch with `503` as the webhook ingress
does; they have no pending-run cap, so bound a subscription's throughput on Pub/Sub's side
(flow control, or a `with_concurrency_key` mapper with `OnConflict::Enqueue`).

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
| Egress check | The [egress policy](../security/egress.md), by default HTTPS only, and the host must not be, or resolve to, a loopback, private, link-local, cloud metadata, CGNAT, multicast, reserved, or IPv4-embedding IPv6 address. Requests go through a `GuardedClient`, so the address checked is the address connected to; redirects are never followed and system proxies are bypassed. When that client cannot be built, for example because the TLS setup fails, every message is dead-lettered with the reason and no request is sent |
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

`with_egress_policy` takes an `adk_core::egress::EgressPolicy`.
`EgressPolicy::allow_private_networks()` lifts the address and scheme checks, except for cloud
metadata endpoints, for receivers an operator configures inside a private network. Do not use
it when callers register their own URLs.

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
