# Pairing Devices and Chat Channels

A phone, a desktop tray, a terminal on another machine and a chat account reach the
[agent kernel](index.md) without the operator's own credentials: each is paired once, can do
only what it was paired for, and can be removed at any time (spec 3.3 §10 K1, K2). Pairing
never widens the bind: the control plane stays on loopback unless `--auth-token` or SSO
opens it ([control plane](control-plane.md), S-I7).

| Who | Paired by | Reaches the kernel with | May |
|---|---|---|---|
| A phone, a tray, a remote terminal | A code the person issues at the kernel (`adk-rust pair`) and enters on the device | A device token, `Authorization: Bearer adkd_…` | What its scopes open: `read`, `decide`, `stop` |
| A chat account | A code the kernel sends the account, which the person accepts at the kernel (`adk-rust pair accept`) | The platform adapter, through the channel gateway | Talk to the agents, answer everyday decisions, stop everything |

## Devices

### Pairing a device

1. At the kernel, issue a code:

   ```bash
   adk-rust pair --name "Ana's phone"                # every scope, paired for 90 days
   adk-rust pair --name "Tray" --scope read,stop     # a narrower device
   adk-rust pair --name "Laptop" --days 30
   ```

   ```text
   Pairing code  7K2M-QX4D
   Works until   2026-10-24 14:52
   It may        decide, read, stop
   ```

   The code works for ten minutes and pairs one device. With a home address set
   (`Pairing::with_home_url`) the answer also carries `<home>/pair#code=7K2M-QX4D`, the URL a
   phone opens; the code sits in the fragment, so no server log keeps it.

2. On the device, claim it. The claim is the one route that needs no caller, since the code
   is the credential:

   ```bash
   curl -H 'content-type: application/json' -d '{"code": "7K2M-QX4D", "name": "Pixel"}' \
     http://127.0.0.1:8080/api/kernel/pairing/claim
   ```

   The answer carries the device and its token, `adkd_<id>_<secret>`, shown this once.

3. The device sends the token as `Authorization: Bearer <token>` from then on.

| Guard | What it stops |
|---|---|
| The store keeps a code's SHA-256 and a token's secret's SHA-256, never the code or the token | A leaked database pairing anyone |
| A claim takes the code in one statement | One code pairing two devices |
| Twenty wrong codes in a row cancel every waiting code | Guessing a code |
| A token is compared in constant time, and refused once revoked or past its expiry (90 days by default, at most 365) | A stolen or forgotten device keeping access |

### What a device may do

A device acts as the person who issued its code, with that person's approval groups, on the
routes its scopes open and on no other route:

| Scope | Routes (under `/api`) |
|---|---|
| `read` | `GET /kernel/health`, `GET /kernel/processes`, `GET /kernel/events`, `POST /kernel/attach/{session}`, `GET /kernel/agenda`, `GET /approvals`, `GET /approvals/{id}` |
| `decide` | `POST /approvals/{id}/decide` at low and medium risk; a high-risk decision, or one whose risk the kernel cannot read, answers `403` and waits for the person to confirm it is them in the home (spec 3.3 §7.2) |
| `stop` | `POST /kernel/freeze` |

| Answer | When |
|---|---|
| `401` | An unknown, revoked or expired device token |
| `403 kernel.unauthorized` | A route the device's scopes do not open, `unfreeze` included: resuming happens only in the home or on the kernel's own terminal (S-I10) |

A request that carries a device token is that device on every transport, loopback included:
it is never admitted as the loopback operator, and a forged token is refused rather than
passed on. The routes the server serves beside the kernel's — sessions, runs, rules — refuse
device tokens too.

### Listing and removing

```bash
adk-rust pair list                 # devices and chat accounts, and accounts waiting to pair
adk-rust pair revoke 0a1b2c3d      # a device's id, a unique prefix of it, or its name
```

```text
ID                 NAME                     MAY                  STATE      SINCE
0a1b2c3d4e5f6071   Ana's phone              decide, read, stop   paired     2026-10-24 14:43
ch-5c1e09a2d4b7f3e Ana (telegram)           chat, decide, stop   paired     2026-10-24 14:50
```

A revoked device's token stops working on its next request.

| Route | Purpose | `operator` | Recorded |
|---|---|---|---|
| `POST /kernel/pairing` | Issues a code; body `{"name": "...", "scopes": ["read"], "expiresInDays": 30}` | Yes | `pairing.issue` |
| `POST /kernel/pairing/claim` | Claims a code; body `{"code": "...", "name": "..."}` | — (the code) | `pairing.claim`, as the device's owner |
| `GET /kernel/devices` | Every device, revoked and expired ones included | Yes | — |
| `POST /kernel/devices/{id}/revoke` | Removes a device | Yes | `devices.revoke` |

Every action a device takes is recorded with `device` in its `kernel.control` record, and a
decision a device answers writes `approvals.decide` with the device before it reaches the
inbox, so "What your agents did" can say where the person said yes from (spec 3.3 §11).

## Chat channels

With the `channels` feature, the kernel's channel contract turns a chat thread into a
doorbell and a conversation; a platform adapter (Telegram first, in `adk-surface`) moves
the messages. The kernel applies every rule; the adapter renders and escapes what the kernel
sends (S-I9).

```toml
[dependencies]
adk-kernel = { version = "3.0.0", features = ["channels"] }
```

| Rule | What the gateway does |
|---|---|
| Only paired accounts reach an agent (S-I6) | An unpaired account's message, or a pressed button, gets a pairing code and starts nothing |
| A paired account's message is a turn | It starts a turn as the paired person in the thread's session, keyed by the platform's message id, so a redelivery starts nothing; the reply goes back to the same thread |
| Cards carry no arguments (S-I5) | A decision card holds the agent, a summary of at most 160 characters with every argument value masked as `[…]`, and the risk |
| Chat says yes once or no (§7.2, §7.4) | **Allow once** and **Deny** at low and medium risk only, through `ApprovalService::decide` at the version the card showed; a high-risk card offers no button and links to the home |
| A second answer is shown, never retried (S-I2) | "Decided already: you said yes at 07:31." |
| Stop all from anywhere, resume only at home (S-I10) | "stop all" or `/stop` freezes the kernel; "resume" or `/resume` is refused |
| Nothing notifies during quiet time | A card for a decision asked in quiet time is sent when quiet time ends, if it still waits; after a restart, `ChannelGateway::ring_waiting` rings every decision that still waits |

### Pairing a chat account

1. The account writes to the agent and is sent a code, valid for an hour:
   "This account isn't paired yet. To pair it, run `adk-rust pair accept 3HWT-9PQA` where
   your agents run, before 15:50."
2. The person accepts it where the kernel runs: `adk-rust pair accept 3HWT-9PQA`, or
   `POST /api/kernel/channels/pair` with `{"code": "3HWT-9PQA"}` as the operator.
3. The account's next message starts a turn. `adk-rust pair revoke <id>` removes it.

At most five accounts of one channel wait to be paired at once; the account that asked is
told to try later and nothing is stored.

### Building the gateway

```rust
use std::sync::Arc;

use adk_core::{ApprovalPolicy, ApproverSelector, InMemoryApprovalStore, StoreApprovalService};
use adk_kernel::channels::{ChannelDoorbell, ChannelGateway, InMemoryChannel, InMemoryChannelBindingStore};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
use adk_kernel::{Kernel, KernelConfig};

async fn serve(runs: Arc<dyn adk_core::DurableRunStore>, runner: Arc<adk_runner::Runner>) -> adk_kernel::Result<()> {
    // The doorbell rings every decision the inbox is asked.
    let doorbell = Arc::new(ChannelDoorbell::new());
    let approvals = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_router(ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval())
            .with_run_store(runs.clone())
            .with_notifier(doorbell.clone()),
    );
    // Chat accounts' pairings are accepted, listed and removed on the control plane.
    let bindings = Arc::new(InMemoryChannelBindingStore::new());
    let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new())).with_channel_bindings(bindings.clone());
    let kernel = Kernel::builder()
        .config(KernelConfig::default().with_control(ControlPlaneConfig::default().with_pairing(pairing)))
        .run_store(runs)
        .approval_service(approvals)
        .agent("adk-rust", runner)
        .build()?;
    kernel.start().await?;
    let chat = Arc::new(InMemoryChannel::new("chat"));
    let gateway = ChannelGateway::builder(Arc::new(kernel.clone()), bindings)
        .sink(chat)
        .doorbell(doorbell)
        .home_url("https://kernel.example")
        .build();
    gateway.start().await?;
    // The adapter hands every inbound message to `gateway.receive(..)`.
    kernel.shutdown_on_signal().await
}
```

| Type | Role |
|---|---|
| `ChannelSink` | What an adapter implements for what goes out: `send_message`, `send_decision` (a `DecisionCard`), `send_stopped` (a `StopNotice`), `send_notice` (a `ChannelNotice`) |
| `ChannelInbound` | What an adapter hands in: a typed message, a pressed answer carrying the card's decision and version, "Stop all" |
| `ChannelGateway` | Applies the rules; `receive` returns what it did (`InboundOutcome`) |
| `ChannelDoorbell` | The `ApprovalNotifier` that rings each submitted decision on every paired thread of its person |
| `ChannelHost` | What the gateway runs on: `Kernel`, or the contract suite's `ContractHost` |
| `InMemoryChannel` | The test channel: a sink that keeps what it is sent, and the inbound values a platform would make |

### Bindings and their backends

`ChannelBindingStore` keeps paired accounts and pairing requests, a code by its hash only:

| Backend | Type | Feature |
|---|---|---|
| In-memory | `InMemoryChannelBindingStore` | `channels` |
| SQLite | `SqliteChannelBindingStore` | `channels`, `sqlite` |
| PostgreSQL | `PostgresChannelBindingStore` | `channels`, `postgres` |
| Redis | `RedisChannelBindingStore` | `channels`, `redis` |

The device store, `DeviceStore`, runs on the same four backends without the feature:
`InMemoryDeviceStore`, `SqliteDeviceStore` (on `harness.db` under `adk-rust up`),
`PostgresDeviceStore` and `RedisDeviceStore` (with `--store`). Each store has a conformance
suite: `device_store_conformance_tests!` and `channel_binding_store_conformance_tests!`
(feature `conformance`).

### The adapter contract

Every adapter passes `channel_contract_tests!` (features `channels`, `conformance`): the
suite runs the adapter under the gateway over `ContractHost`, an in-memory kernel double,
and checks what the adapter put on the wire and what it made of the person's input.

```rust
use std::sync::Arc;

use adk_kernel::channels::InMemoryChannel;
use adk_kernel::conformance::{ChannelAdapter, ChannelAdapterHarness};

struct InMemory;

#[async_trait::async_trait]
impl ChannelAdapterHarness for InMemory {
    async fn adapter(&self) -> Arc<dyn ChannelAdapter> {
        Arc::new(InMemoryChannel::new("chat"))
    }
}

adk_kernel::channel_contract_tests! { harness = InMemory }
```

| Case | Contract |
|---|---|
| An unpaired account writes | It gets a code in its thread; no turn starts |
| A paired account writes | A turn starts as its person, authenticated; the reply comes back to its thread |
| A message is delivered twice | One turn |
| A decision is asked | The card shows the agent, the summary and the risk; no argument value leaves the machine |
| "Allow once", then a second press | Approved once; the second press is answered "decided already" |
| "Deny" | Rejected |
| A high-risk decision | No button; a forged answer decides nothing |
| "stop all", then "resume" | Everything stops; the resume is refused |
| A message while everything is stopped | Nothing starts, and the thread is told |
| An unpaired account forges an answer | Nothing is decided; it gets a code |
| A decision in quiet time | Its card arrives when quiet time ends |

## Audit records

| Record | Written for |
|---|---|
| `kernel.control` with `pairing.issue`, `pairing.claim`, `devices.revoke`, `channels.pair`, `channels.revoke` | Pairing and removal, before they take effect |
| `kernel.control` with `approvals.decide` and `device` | A decision a device answers |
| `kernel.channel` with `surface: chat`, `channel`, `account`, `action`, `target` | A chat thread's pairing request (`pairing.requested`, `pairing.busy`), message (`message`), decision card (`notify`), answer (`decide`, `decide.refused`), stop (`stop`) and refused resume (`resume.refused`) |

A chat answer is recorded before `decide` runs and is refused when its record cannot be
written; a stop takes effect regardless.
