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
   adk-rust pair --name "Ana's phone" --kind phone   # every scope, paired for 90 days
   adk-rust pair --name "Tray" --kind tray --scope read,stop   # a narrower device
   adk-rust pair --name "Laptop" --days 30           # a remote terminal, 1 to 365 days
   ```

   ```text
   Pairing code  7K2M-QX4D-9HWT
   Works until   2026-10-24 14:52
   It may        decide, read, stop
   Paired for    90 days once it pairs
   ```

   The code works for ten minutes and pairs one device. With a home address set
   (`Pairing::with_home_url`), a code of kind `phone` (`adk-rust pair --kind phone`) also
   carries `<home>/pair#code=7K2M-QX4D-9HWT`, the URL the phone opens; the code sits in the
   fragment, so no server log keeps it. A terminal's or a tray's code carries no link: it is
   typed on the device.

2. On the device, claim it. The claim is the one route that needs no caller, since the code
   is the credential:

   ```bash
   curl -H 'content-type: application/json' -d '{"code": "7K2M-QX4D-9HWT", "name": "Pixel"}' \
     http://127.0.0.1:8080/api/kernel/pairing/claim
   ```

   The answer carries the device and its token, `adkd_<id>_<secret>`, shown this once. The
   claim sends no token and the device guard lets it through, so a device whose old token
   was removed or expired pairs again with a new code.

3. The device sends the token as `Authorization: Bearer <token>` over HTTP from then on.

| Guard | What it stops |
|---|---|
| A code is twelve characters (60 bits); the store keeps a code's SHA-256 and a token's secret's SHA-256, never the code or the token | A leaked database pairing anyone: trying every code against a hash takes years of a current GPU, the code lives ten minutes |
| A claim takes the code in one statement, and puts it back when the device cannot be stored or the claim cannot be recorded | One code pairing two devices, and a failed claim spending the code |
| Every claim is counted in the device store before its code is looked up, on every replica over the store; after twenty claims without a success every waiting code is cancelled, and claims are refused until the next code is issued | Guessing a code, with claims in flight at once or spread over replicas |
| The claim route sits behind the server's host and origin checks | A web page that rebinds its name to the loopback address spending the waiting codes |
| A token is compared in constant time, and refused once revoked or past its expiry (90 days by default, 1 to 365) | A stolen or forgotten device keeping access |
| An event stream a device holds open ends when the device is revoked or its pairing ends | A removed device reading on through a stream it opened before |

### What a device may do

A device acts as the person who issued its code, with that person's approval groups, on the
routes its scopes open and on no other route:

| Scope | Routes (under `/api`) |
|---|---|
| `read` | `GET /kernel/health`, `GET /kernel/processes`, `GET /kernel/events`, `POST /kernel/attach/{session}`, `GET /kernel/agenda`, `GET /approvals`, `GET /approvals/{id}`; what Today and "Agent Memory and Rules" read ([settings, away, memory and record](today-and-memory.md)): `GET /kernel/settings[/{partition}]`, `GET /kernel/away`, `POST /kernel/away/seen` (the device's own cursor), `GET /kernel/away/lines/{seq}`, `GET /kernel/memory`, `GET /kernel/record`, `GET /kernel/record/check`. No device changes a setting or a memory |
| `decide` | `POST /approvals/{id}/decide` at low and medium risk; a high-risk decision, or one whose risk the kernel cannot read, answers `403` and waits for the person to confirm it is them in the home (spec 3.3 §7.2). A call of a tool a mounted driver declares `publish_public` or `financial`, the classes nothing may allow without a person (spec 3.2 K-I9), is high risk whatever its request says. A take-over, whose secret the person types in the home, may only be denied. The device guard checks the risk and records the decision; `DeviceTokenExtractor` refuses a device's decision the guard did not admit |
| `stop` | `POST /kernel/freeze` |

| Answer | When |
|---|---|
| `401` | An unknown, revoked or expired device token |
| `403 kernel.unauthorized` | A route the device's scopes do not open, `unfreeze` included: resuming happens only in the home or on the kernel's own terminal (S-I10) |

Over HTTP, a request that carries a device token is that device, on a loopback bind too: it
is never admitted as the loopback operator, and a forged token is refused rather than passed
on. The routes the server serves beside the kernel's — sessions, runs, rules — refuse device
tokens too. The kernel's socket and named pipe admit only its own OS user, as the operator,
and read no `Authorization` header, so a device token there changes nothing.

A server that serves the kernel's routes to devices composes three pieces, as `adk-rust up`
does: the routes behind a `DeviceTokenExtractor` that wraps its other extractor,
`control::claim_router` merged with `ServerBuilder::add_root_routes`, and
`control::guard_devices` around the built app.

### Device kinds and the server's routes

Every device has a kind, set by whoever issues its code (`--kind`, or `kind` in the issue
route's body) and never by the claim: the code carries it to the device it pairs.

| Kind | What it is | Its answers (spec 3.3 §7.2) |
|---|---|---|
| `phone` | The web home installed on a phone | The phone's row: high risk after confirming it is the person |
| `terminal` | A terminal on another machine; also every device paired without a kind | The terminal's row, never a paired local terminal: high risk waits for the home |
| `tray` | The desktop tray | The tray's row |

A device's requests carry its kind in the `adk.device.kind` request-context metadata
(`DEVICE_KIND_METADATA`), beside its id, name and scopes, and `GET /kernel/devices` lists it.

The kernel names only its own routes. A server that serves more — the surfaces' routes under
`/api/surface` — opens them to devices with `Pairing::with_device_routes`, a map from a method
and a path to the scope it needs, consulted after the kernel's own (`device_scope_for`).
`DeviceTokenExtractor` and `guard_devices` read it from the pairing:

```rust
use std::sync::Arc;

use adk_kernel::pairing::{DeviceScope, InMemoryDeviceStore, Pairing};
use axum::http::Method;

fn pairing() -> Pairing {
    Pairing::new(Arc::new(InMemoryDeviceStore::new())).with_device_routes(Arc::new(
        |method: &Method, path: &str| match (method.as_str(), path) {
            ("GET", path) if path.starts_with("/api/surface/") => Some(DeviceScope::Read),
            _ => None,
        },
    ))
}
```

The guard checks a decision's risk and records `approvals.decide` on the kernel's own decide
route, `POST /approvals/{id}/decide`, only. A decide route of the server's map is admitted for
the server's engine, which applies what each surface may decide and records the answer
itself (the surfaces' `surface.decide`).

### Attachments

Surfaces keep what belongs to a device or a person beside it in the device store: a phone's
push subscription, a passkey, the claim that sends one wake-up once across replicas
(spec 3.3 §5.4, §8.2). A `DeviceAttachment` is a holder — `AttachmentHolder::Device(id)` or
`AttachmentHolder::User(id)` — a kind, a key and a JSON record the store never reads.

| `DeviceStore` method | What it does |
|---|---|
| `put_attachment` | Keeps an attachment, replacing the one with its holder, kind and key |
| `put_attachment_if_absent` | Keeps it unless one exists; of concurrent puts from any replica, exactly one writes |
| `attachments(holder, kind)` | The holder's attachments of a kind, in key order |
| `remove_attachment(holder, kind, key)` | Removes one |
| `remove_attachments(holder)` | Removes every attachment of a holder |

Revoking a device removes its attachments. The in-memory, SQLite, PostgreSQL and Redis stores
keep attachments (SQL in `adk_kernel_device_attachments`, migration step 3); a third-party
store answers the methods `kernel.unsupported` until it implements them, and the conformance
suite covers them.

`Pairing::until_device_ends(id)` resolves when a device stops working: at once when it is
revoked on this kernel, when a re-read finds it revoked on another replica (every five seconds
by default), or when its pairing ends. A server ends a device's open streams with it.

### Listing and removing

```bash
adk-rust pair list                 # devices and chat accounts, and accounts waiting to pair
adk-rust pair revoke 0a1b2c3d      # a device's id, a unique prefix of it, or its name
adk-rust pair revoke ch-77f0       # a chat account, paired or waiting to be paired
```

```text
ID                 NAME                     KIND      MAY                  STATE      SINCE
0a1b2c3d4e5f6071   Ana's phone              phone     decide, read, stop   paired     2026-10-24 14:43
ch-5c1e09a2d4b7f3e Ana (telegram)           chat      decide, stop         paired     2026-10-24 14:50
```

A revoked device's token stops working at once: its next request is refused, and an event
stream it holds open ends, at once on the kernel that revoked it and within five seconds on
another replica over the same store (`Pairing::with_revocation_check`). A stream also ends
when the device's pairing does. `pair list` and `pair revoke` report a kernel that cannot
list its chat accounts as the error it is, not as having none.

| Route | Purpose | `operator` | Recorded |
|---|---|---|---|
| `POST /kernel/pairing` | Issues a code; body `{"name": "...", "scopes": ["read"], "expiresInDays": 30, "kind": "phone"}`, 1 to 365 days | Yes | `pairing.issue`, with the scopes the code grants, the name as stored and the kind; a request refused for its length records nothing |
| `POST /kernel/pairing/claim` | Claims a code; body `{"code": "...", "name": "..."}` | — (the code) | `pairing.claim`, as the device's owner |
| `GET /kernel/devices` | Every device, revoked and expired ones included | Yes | — |
| `POST /kernel/devices/{id}/revoke` | Removes a device | Yes | `devices.revoke` |

Every action a device takes is recorded with `device` in its `kernel.control` record, and a
decision a device answers writes `approvals.decide` before it reaches the inbox, with
`surface: device`, the device, its name (`deviceName`) and the answer it sent (`answer`:
`approve`, `edit`, `reject` or `respond`). The record's line in `GET /kernel/record` carries
the action (`kernelAction`), the answer (`decision`) and the device's name (`channel`), so
"What your agents did" can say where the person said yes from (spec 3.3 §11).

The routes are mounted on every kernel: one built without `ControlPlaneConfig::with_pairing`
answers `400` naming it, and one without chat channels lists no chat accounts and answers
`pair accept` with `400`.

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
| Cards carry no arguments (S-I5) | A decision card holds the agent, a summary of at most 160 characters with every argument value masked as `[…]`, and the risk. A value of three or more characters is masked wherever it appears, ignoring case; a shorter one, `true` or `false` where it stands as a word or number of its own; newlines and other control characters read as spaces |
| Chat says yes once or no, never money (§7.2, §7.4) | **Allow once** and **Deny** at low and medium risk only, through `ApprovalService::decide` at the version the card showed. A high-risk card, and the card of a call that moves money, offers no button and links to the home: a tool a mounted driver declares `financial`, a `payments_*` tool of `adk-payments`, or one `ChannelGatewayBuilder::money_tools` names. A tool a mounted driver declares `publish_public` or `financial` is high risk whatever its request says (spec 3.2 K-I9). A take-over, whose secret the person types in the home (§8.2), offers **Deny** only |
| Only who may answer is asked | A card goes to the decision's person when the policy lets that person answer it; an answer from anyone else is told the decision waits for someone else |
| A second answer is shown, never retried (S-I2) | "Decided already: you said yes at 07:31."; an answer that counts toward a quorum is told the decision also waits for someone else |
| One card per decision | Whether the doorbell or `ring_waiting` rings a decision first, it is carded once; a gateway stopped and started again keeps ringing the doorbell, and starting a running gateway changes nothing |
| Stop all from anywhere, resume only at home (S-I10) | "stop all" or `/stop` freezes the kernel; "resume" or `/resume` is refused |
| Nothing notifies during quiet time | A card for a decision asked in quiet time is sent when quiet time ends, if it still waits: the quiet time of the decision's area in force, a change made in the home included. The wait reads the wall clock at least once a minute, so a laptop that slept through the night sends the card when quiet time ends |
| Every waiting decision reaches the thread | Every minute (`ChannelGatewayBuilder::resweep`) the gateway rings the decisions that still wait: a card that could not be sent is sent again, and a decision another replica holds is carded too; after a restart, `ChannelGateway::ring_waiting` does the same at once |
| A turn's reply reaches the thread from any replica | The gateway asks the run store after the turns it started, so a turn another replica's worker ran is answered too, once |
| A removed account reaches no agent | Removing it stops its turns that have not finished, the one waiting on a decision and those queued behind it |

### Pairing a chat account

1. The account writes to the agent and is sent a code, valid for an hour:
   "This account isn't paired yet. To pair it, run `adk-rust pair accept 3HWT-9PQA-K7M2`
   where your agents run, before 15:50."
2. The person accepts it where the kernel runs: `adk-rust pair accept 3HWT-9PQA-K7M2`, or
   `POST /api/kernel/channels/pair` with `{"code": "3HWT-9PQA-K7M2"}` as the operator. The
   acceptance is recorded before the account is stored; when the record cannot be written,
   nothing is paired and the code still works.
3. The account's next message starts a turn. `adk-rust pair revoke <id>` removes it and
   stops its turns that have not finished.

At most five accounts of one channel wait to be paired at once; the account that asked is
told to try later and nothing is stored. The gateway takes unpaired accounts' messages one
at a time, so the limit holds for messages that arrive together and an account that writes
twice at once gets one code. `adk-rust pair list` shows the waiting accounts, and
`adk-rust pair revoke <id>` (`POST /api/kernel/channels/waiting/{id}/dismiss`) dismisses one:
its code stops working, its place frees, and for a day its messages get no new code and no
answer, so strangers cannot hold every place.

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
| `kernel.control` with `pairing.issue`, `pairing.claim`, `devices.revoke`, `channels.pair`, `channels.revoke`, `channels.dismiss` | Pairing and removal, before they take effect: a claim or an acceptance takes its code, writes the record, then stores the device or account, and puts the code back when the record cannot be written |
| `kernel.control` with `approvals.decide`, `surface: device`, `device`, `deviceName` and `answer` | A decision a device answers |
| `kernel.channel` with `surface: chat`, `origin: {"kind": "chat", "channel": …}`, `channel`, `account`, `action`, `target` | A chat thread's pairing request (`pairing.requested`, `pairing.busy`), message (`message`), decision card (`notify`), answer (`decide`, `decide.refused`), stop (`stop`) and refused resume (`resume.refused`) |

A chat answer is recorded before `decide` runs and is refused when its record cannot be
written; a stop takes effect regardless.
