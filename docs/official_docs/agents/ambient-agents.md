# Ambient Agents

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
