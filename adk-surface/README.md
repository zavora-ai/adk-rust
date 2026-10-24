# adk-surface

Agent surfaces for ADK-Rust (spec 3.3,
[`docs/design/spec-3.3-harness-ui.md`](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/spec-3.3-harness-ui.md)):
one engine under every window a person opens onto their agents — the web home, the terminal,
chat, the phone, the desktop tray, voice. Nothing lives in a surface: a decision is made once,
from anywhere, and every other window shows who made it.

[![Crates.io](https://img.shields.io/crates/v/adk-surface.svg)](https://crates.io/crates/adk-surface)
[![Documentation](https://docs.rs/adk-surface/badge.svg)](https://docs.rs/adk-surface)
[![License](https://img.shields.io/crates/l/adk-surface.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

**Stability: Beta.** `default = []`; no library crate depends on `adk-surface`.

## What it is

| Part | What it does | Where |
|---|---|---|
| Feed | A session's events numbered by their position in the session store, as a snapshot then a live tail; reconnecting after the last `seq` seen never skips or repeats an event | `SurfaceEngine::feed`, `SurfaceEngine::thread` |
| Projection | Decision cards (§7.1), thread items, agent rows, the status line and the "While you were away" briefing, in the plain words of §8 | `model`, `words` |
| Commands | Decide at the version seen, send, queue, stop all, resume all, change the mode — each with the caller's subject and how the input was given, written to the audit log | `SurfaceEngine::execute`, `Command` |
| Rules | What each surface may decide (§7.2) and the rules on every surface (§7.4), enforced by the engine | `rules` |
| `web` | The web home: its HTTP API under `/api/surface`, its OpenAPI document and generated TypeScript types, and the page itself at `/ui/` — Threads, Today and Memory & rules, in light and dark (spec 3.3 §5.1) | `web::router`, `web::home` |

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
service and the engine never retries it (S-I2). Input typed while a decision is open is queued
as the next message, never an answer (S-I4). "Stop all" works from every surface; "Resume all"
only from the web home or a paired local terminal (S-I10). No agent answers a decision (§7.4).
A chat card carries the agent, the risk and a short summary, never the call's arguments (S-I5).

## The web API

| Route (under `/api`) | Answer |
|---|---|
| `GET /surface/decisions` | The caller's open decisions as cards; `?session=` for one session's |
| `GET /surface/decisions/{id}` | One decision, waiting or decided |
| `GET /surface/sessions/{session}/thread` | The session's entries |
| `GET /surface/sessions/{session}/feed` | Server-sent events, resumable with `Last-Event-ID` |
| `GET /surface/sessions/{session}/queued` | What the caller typed while a decision of the session was open |
| `GET /surface/agents` | The roster |
| `GET /surface/status` | The status line |
| `GET /surface/briefing` | "While you were away" |
| `POST /surface/commands` | A command with its surface and origin |
| `GET /surface/openapi.json` | The OpenAPI 3.1 document |
| `POST /surface/decisions/{id}/take-over` | The secret a person typed, for a take-over card (`web::take_over_router`, beside a harness that takes over) |

Every route checks the caller against the session owner (S-I8). The OpenAPI document and the
TypeScript types in `web/src/generated/` are generated from the Rust types;
`scripts/check-surface-api.sh` fails CI when they drift, and `--write` regenerates them.

## The web home

The page is a React, Vite and TypeScript app in `web/`, built into `assets/web` and embedded
by the `web` feature (§6.1, §10 S1). It reads and acts only through the routes above and the
kernel and harness routes beside them, and holds no state a reload cannot rebuild. `adk-rust serve` and `adk-rust up` mount it. Every answer carries the version the card
showed, a conflict renders as "already decided", Enter and Esc never decide, and typing while
a decision is open is queued (§7.4). Controls whose kernel routes do not exist yet (quiet time,
the spending limit and the mode; memory; the record check) render read-only through typed
seams in `web/src/seams.ts`.

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

The terminal, chat channels, the phone, the desktop tray, voice, agent-drawn UI and the
workflow observer arrive as further features (spec 3.3 §6.1, §12).

## License

Apache-2.0
