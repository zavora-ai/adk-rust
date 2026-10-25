# adk-surface

Agent surfaces for ADK-Rust (spec 3.3,
[`docs/design/spec-3.3-harness-ui.md`](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/spec-3.3-harness-ui.md)):
one engine under every window a person opens onto their agents — the web home, the terminal,
chat, the phone, the desktop tray, voice. Nothing lives in a surface: a decision is made once,
from anywhere, and every other window shows who made it.

[![Crates.io](https://img.shields.io/crates/v/adk-surface.svg)](https://crates.io/crates/adk-surface)
[![Documentation](https://docs.rs/adk-surface/badge.svg)](https://docs.rs/adk-surface)
[![License](https://img.shields.io/crates/l/adk-surface.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

**Stability: Beta.** `default = []`. `adk-cli` depends on it, with `web`, through its `surface`
feature, which is on by default so the `adk-rust` binary serves the web home; build `adk-cli`
with `default-features = false` to leave it out.

## What it is

| Part | What it does | Where |
|---|---|---|
| Feed | A session's events numbered by their position in the session store, as a snapshot then a live tail; reconnecting after the last `seq` seen never skips or repeats an event | `SurfaceEngine::feed`, `SurfaceEngine::thread` |
| Projection | Decision cards (§7.1), thread items, agent rows, the status line and the "While you were away" briefing, in the plain words of §8 | `model`, `words` |
| Commands | Decide at the version seen, send, queue, stop all, resume all, change the mode — each with the caller's subject and how the input was given, written to the audit log | `SurfaceEngine::execute`, `Command` |
| Rules | What each surface may decide (§7.2) and the rules on every surface (§7.4), enforced by the engine | `rules` |
| `web` | The web home: its HTTP API under `/api/surface`, its OpenAPI document and generated TypeScript types, and the page itself at `/ui/` — Threads, Today and Memory & rules, in light and dark (spec 3.3 §5.1) | `web::router`, `web::home` |
| `channels` | Chat: the Telegram adapter over the kernel's channel contract — cards with Allow once and Deny, never the call's arguments, pairing, "stop all" (spec 3.3 §5.3) | `channels::TelegramAdapter` |
| `mobile` | The phone: the web home installed on a phone, woken by Web Push from the person's own kernel with a wake-up that says nothing, and "Confirm it's you" with a passkey on the phone or the computer before a high-risk approval (spec 3.3 §5.4, §8.2). Links OpenSSL through webauthn-rs | `mobile::Phone`, `mobile::router` |
| Desktop tray | Experimental and not a feature: an unpublished Tauri 2 app in `desktop/` whose windows load the web home and its tray panel at `/ui/tray`, with the decisions badge on a tray icon (spec 3.3 §5.5) | `desktop/README.md` |
| `voice` | Voice and video: spoken "allow once" and "deny" through the engine with a cancel window, a transcript digest on record and never the recording; the camera and the screen while their switches are on; with `web`, the voice view at `/ui/voice` and its socket; with `tui`, the terminal's voice line (spec 3.3 §5.6) | `voice::VoiceHub`, `voice::router`, `SurfaceEngine::decide_spoken` |

## Sources

| Source | For |
|---|---|
| `LocalSource` | Surfaces in the same process as the agents: the session store, the approval service, and an in-process `Kernel`, `Harness` or `Runner` |
| `ControlSource` | A kernel in another process or on another machine, through `adk_kernel::control::ControlClient`, with the approvals inbox read in full over the same HTTP or Unix-socket endpoint |

## Quick start

```toml
[dependencies]
adk-surface = { version = "3.0.0", features = ["web"] }
```

```rust,no_run
use std::sync::Arc;
use adk_surface::{Caller, LocalSource, Origin, Place, SurfaceEngine, SurfaceKind};

# async fn example(
#     kernel: adk_kernel::Kernel,
#     sessions: Arc<dyn adk_session::SessionService>,
#     approvals: Arc<dyn adk_core::ApprovalService>,
#     builder: adk_server::ServerBuilder,
# ) -> adk_surface::Result<()> {
let source = LocalSource::builder("adk-rust")
    .sessions(sessions)
    .approvals(approvals)
    .kernel(kernel.clone(), "adk-rust")
    .build()?;
let engine = SurfaceEngine::builder(Arc::new(source))
    .places([Place::phone(), Place::chat("Telegram")])
    .build();

// The web home's API beside the kernel's own routes, behind the server's auth middleware,
// and its page at `/ui/`.
let builder = builder
    .add_api_routes(adk_kernel::control::router(kernel))
    .add_api_routes(adk_surface::web::router(engine.clone()))
    .add_root_routes(adk_surface::web::home());

// A chat adapter answers in process, with its channel as the origin.
let me = Caller::human("ana", SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() });
for card in engine.decisions(&me).await? {
    println!("{} · {} · {}", card.heading, card.agent.display_name, card.risk_text);
}
# let _ = builder;
# Ok(())
# }
```

## What each surface may decide (spec 3.3 §7.2)

| Surface | Allow once | Always allow | Edit | Deny | High risk |
|---|---|---|---|---|---|
| Web home | Yes | Yes | Yes | Yes | Yes, after confirming it is you |
| Terminal | Yes | Yes | Yes | Yes | Yes, on a paired local terminal |
| Phone | Yes | Yes | Yes | Yes | Yes, after confirming it is you |
| Desktop tray | Yes | No (opens the home) | No | Yes | No (opens the home) |
| Chat | Yes, low and medium risk | No | No | Yes | No (links to the home) |
| Voice | Yes, low and medium risk, with a cancel window | No | No | Yes | No |

A decision answered on one surface is shown on every other as "Decided — You said yes in
Telegram at 07:31": a second answer gets `approval.version_conflict` from the approval
service and the engine never retries it (S-I2). Input typed while a decision is open, or a
turn runs, is queued as the next message, never an answer, and sent oldest first once the
session is free, whichever surface answered (S-I4). "Stop all" works from every surface; "Resume all"
only from the web home or a paired local terminal (S-I10). No agent answers a decision (§7.4).
A chat card carries the agent, the risk and a short summary, never the call's arguments (S-I5).

## The web API

| Route (under `/api`) | Answer |
|---|---|
| `GET /surface/decisions` | The caller's open decisions as cards; `?session=` for one session's |
| `GET /surface/decisions/{id}` | One decision, waiting or decided |
| `GET /surface/sessions/{session}/thread` | The session's entries |
| `GET /surface/sessions/{session}/feed` | Server-sent events, resumable with `Last-Event-ID` |
| `GET /surface/sessions/{session}/queued` | What the caller typed while a decision of the session was open or a turn ran, not yet sent |
| `GET /surface/agents` | The roster |
| `GET /surface/status` | The status line |
| `GET /surface/briefing` | "While you were away": the events the kernel's `GET /kernel/away` lists, from the `/proc` ledger |
| `POST /surface/commands` | A command with its surface and origin |
| `GET /surface/openapi.json` | The OpenAPI 3.1 document |
| `POST /surface/decisions/{id}/take-over` | The secret a person typed, for a take-over card (`web::take_over_router`, beside a harness that takes over) |
| `POST /surface/decisions/{id}/answer`, `POST /surface/stop` | A paired device's answer by touch and its Stop all (`web::devices_router`) |
| `/surface/phone/**`, `POST /surface/decisions/{id}/confirm[/options]` | The phone's push subscription, passkeys, pairing QR code and device list, and "Confirm it's you" (`mobile::router`, feature `mobile`) |

Every route checks the caller against the session owner (S-I8). The OpenAPI document and the
TypeScript types in `web/src/generated/` are generated from the Rust types;
`scripts/check-surface-api.sh` fails CI when they drift, and `--write` regenerates them.

## The web home

The page is a React, Vite and TypeScript app in `web/`, built into `assets/web` and embedded
by the `web` feature (§6.1, §10 S1). It reads and acts only through the routes above and the
kernel and harness routes beside them, and holds no state a reload cannot rebuild. `adk-rust serve` and `adk-rust up` mount it. Every answer carries the version the card
showed, a conflict renders as "already decided", Enter and Esc never decide, and typing while
a decision is open is queued (§7.4). **Today** and the **morning briefing** read the kernel's
"While you were away" (`GET /api/kernel/away`, §10 K4): one line per event with its record,
moved on only by **Mark as seen**. **What your agents can do** reads and changes each area's
quiet time, daily spending limit and mode (`/api/kernel/settings`, §10 K3), each area's agents
with what each spent today, and the rules allowed without asking with **Extend** and **Stop
allowing**; **Where you can say yes from** pairs a chat account from its code (§10 K1). **What
your agents remember** reads and corrects the memory, and **What your agents did** shows the
record's check and its latest actions (`/api/kernel/memory`, `/api/kernel/record`, §10 K5).
The page reads each kernel route through a typed seam (`web/src/seams.ts`) and shows what
exists where the server serves less.

```bash
cd adk-surface/web
npm ci && npm run verify   # type check, component tests, build into ../assets/web
npm run e2e                # Playwright against examples/scripted_home.rs
```

See [The Web Home](../docs/official_docs/surfaces/web-home.md).

## Features

| Feature | Surface | Stability |
|---|---|---|
| `web` | The web home: its page and HTTP API | Beta |
| `tui` | The terminal: full screen, inline and JSON lines (`adk_surface::tui`), decisions answered by key; the `adk-rust` binary's `tui` feature | Beta |
| `channels` | Chat: the Telegram adapter (`adk_surface::channels`), over the kernel's channel contract, by long polling | Beta |

The phone, the desktop tray, voice, agent-drawn UI and the workflow observer arrive as
further features (spec 3.3 §6.1, §12). See
[the terminal](../docs/official_docs/surfaces/terminal.md) for its keys, modes and exit codes.

## Chat

`TelegramAdapter` is a Telegram bot as the person's chat thread with their agents (spec 3.3
§5.3). It is the kernel's `ChannelSink` for the channel `telegram` and reads the bot's
updates by long polling, handing each to `ChannelGateway::receive`; the gateway pairs
accounts, starts turns, answers decisions and stops everything.

```rust,no_run
use std::sync::Arc;
use adk_kernel::channels::{ChannelDoorbell, ChannelGateway, InMemoryChannelBindingStore};
use adk_surface::channels::TelegramAdapter;

# async fn example(kernel: adk_kernel::Kernel, doorbell: Arc<ChannelDoorbell>) -> Result<(), Box<dyn std::error::Error>> {
let telegram = TelegramAdapter::builder(std::env::var("TELEGRAM_BOT_TOKEN")?).build()?;
let gateway = ChannelGateway::builder(Arc::new(kernel), Arc::new(InMemoryChannelBindingStore::new()))
    .sink(Arc::new(telegram.clone()))
    .home_url("http://127.0.0.1:8080/ui/")
    .doorbell(doorbell)
    .build();
gateway.start().await?;
telegram.start(gateway).join().await?;
# Ok(())
# }
```

A card carries "Waiting for you · since 18:00", the call in plain words ("Write file", as the
engine heads every chat card; with `approvals`), the kernel's summary with every argument
value masked when a route wrote one, the agent and the risk, **Allow once** and **Deny** at low and medium risk only,
and "Open in home" — never the call's arguments (S-I5). Only a pressed button answers; typed
text is a message (S-I4). Groups, bots, edits and media are ignored, and `/start` loses a deep
link's payload (S-I6). `answer_log` and `audit` record each answer for the engine, so every
other surface says "You said yes in Telegram". See
[chat](../docs/official_docs/surfaces/chat.md). `adk-rust up` starts it when
`TELEGRAM_BOT_TOKEN` is set.

## License

Apache-2.0
