# Agent Surfaces: the Engine

`adk-surface` is the interface to an always-on agent (spec 3.3): the web home, the terminal,
chat, the phone, the desktop tray and voice are windows onto the same kernel. This page covers
the engine every surface runs on and the `web` feature's HTTP API. A decision is made once,
from any surface, and every other surface shows who made it and how.

> **Note:** `adk-surface` is Beta and opt-in (`default = []`). No library crate depends on it;
> the umbrella crate reaches it through the `surface` feature, which no tier includes.

## The engine

| Part | What it does |
|---|---|
| **Feed** | Numbers a session's events by their position in the session store and serves them as a snapshot, then a live tail. A surface that reconnects with the last `seq` it received gets every later event exactly once |
| **Projection** | Turns inbox entries, session events, processes and `/proc` ledger rows into decision cards, thread items, agent rows, the status line and the briefing, in the words of spec 3.3 §8 |
| **Commands** | Carries out decide, send, queue, stop all, resume all and change mode, each with the caller's subject, the surface and how the input was given, and writes an audit record of each |
| **Rules** | Computes the answers each surface may give (§7.2) and enforces the rules of §7.4 |

```toml
[dependencies]
adk-surface = { version = "3.0.0", features = ["web"] }
```

## Sources

| Source | Reads | Acts |
|---|---|---|
| `LocalSource` | The session store, the approval service, an in-process kernel's process table and `/proc` ledger, a harness's mode | Decide, start a turn (a kernel process, a harness turn or a runner turn), freeze and unfreeze, set the mode, create the standing rule of "Always allow" |
| `ControlSource` | A kernel's control plane through `ControlClient` (attach, processes, health, the ledger) and the approvals inbox beside it, read in full | Decide (edits included), spawn, freeze, unfreeze |

Both sources read decisions one session at a time where a surface shows one session
(`InboxQuery::with_session_id`, `GET /api/approvals?session=`; spec 3.3 §10 C1), and
`SurfaceEngine::session_decisions` returns that session's cards.

`ControlSource` reads the inbox over the same endpoint as its client: HTTP, or HTTP over the
kernel's Unix socket. Pass the HTTP control plane's token to `ControlSource::with_bearer_token`
as well as to the client. The inbox serves requests without their fingerprints, so a thread
links a held call to its card by the tool and its arguments. Over the named pipe on Windows
the source keeps to `ControlClient`'s pending items, which carry no risk, agent or earlier
decisions: every decision counts as high risk, "Edit" is not offered, and a decision
answered elsewhere is no longer found.

## A decision on every surface

A card is drawn from the decision's inbox entry, never from agent output (S-I1):

| Part | Field | Example |
|---|---|---|
| Who and when | `agent`, `sinceText`, `statusText` | Finance · since 18:00 · Waiting for you |
| Heading | `heading` | Send email to [client email] |
| The exact call | `call.text`, `call.args`, `call.truncated` | `send_email(to: "…", subject: "…")` |
| How it got here | `timeline` | Asked at 18:00 — Waited through quiet time — Waiting for you |
| Actions | `allowed.options` | Allow once · Always allow for this person · Edit · Deny |
| Details | `riskText`, `reversibilityText`, `alsoShown` | Medium risk · Can't be unsent · Also on your phone and in Telegram |

The answers depend on the surface and on how the input was given: a spoken answer in the web
home follows the voice row.

| Surface | Allow once | Always allow | Edit | Deny | High risk |
|---|---|---|---|---|---|
| Web home | Yes | Yes | Yes | Yes | Yes, after confirming it is you |
| Terminal | Yes | Yes | Yes | Yes | Yes, on a paired local terminal |
| Phone | Yes | Yes | Yes | Yes | Yes, after confirming it is you |
| Desktop tray | Yes | No (opens the home) | No | Yes | No (opens the home) |
| Chat | Yes, low and medium risk | No | No | Yes | No (links to the home) |
| Voice | Yes, low and medium risk, with a cancel window | No | No | Yes | No |

A call that spends money follows the high-risk column and is never allowed "always". A call
shown truncated cannot be approved until the surface expanded it. A chat card leaves the
machine through someone else's servers, so it carries the agent, the risk and the tool's name
as its heading, never the call's arguments (S-I5).

## Commands

```rust,ignore
use adk_surface::{Answer, Caller, Command, CommandOutcome, Origin, SurfaceKind};

let chat = Caller::human("ana", SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() });
let outcome = engine
    .execute(&chat, Command::Decide {
        decision_id: card.id.clone(),
        version: card.version,
        answer: Answer::AllowOnce,
        edited_args: None,
        reason: None,
        expanded: false,
    })
    .await?;
match outcome {
    CommandOutcome::Answered { card } => println!("{}", card.decided.unwrap().text),
    CommandOutcome::AlreadyDecided { decided, .. } => println!("{}", decided.text),
    other => println!("{other:?}"),
}
```

| Command | Effect |
|---|---|
| `decide` | `ApprovalService::decide` at the version the surface saw. A version conflict becomes `alreadyDecided` with who, where and when ("You said yes in Telegram at 07:31") and is never retried (S-I2); a decision still open at a newer version comes back as `changed` |
| `cancelAnswer` | Cancels a spoken "allow once" inside its cancel window |
| `send` | Starts a turn; while a decision of the session is open, or a turn runs, the message is queued instead (S-I4) |
| `queue` | Queues the message as the session's next |
| `stopAll` | Freezes every agent, from any surface (S-I10) |
| `resumeAll` | Unfreezes, only from the web home or a paired local terminal (S-I10) |
| `setMode` | Changes the session's mode; making agents act more on their own is a standing permission, so only the web home, the phone and the terminal may loosen it |

### Error codes

| Code | When |
|---|---|
| `surface.not_found` | No session or decision of the caller has that id; also the answer for someone else's (S-I8) |
| `surface.answer_not_allowed` | The surface may not give this answer (§7.2) |
| `surface.confirm_presence` | A high-risk approval without confirming it is you |
| `surface.call_not_expanded` | A truncated call approved before it was expanded |
| `surface.not_here` | An agent answering a decision, resuming outside the home, loosening the mode from chat or voice |
| `surface.unsupported` | The source cannot do it |

## Audit records

Every answer is written to the audit trail as `surface.decide` (kind `approval`) with
`approvalId`, `sessionId`, `tool`, `answer`, `outcome` (`decided`, `already_decided`,
`changed`, `refused`, `cancelled`), `surface` and `origin`; the actor is the person.
`surface.send`, `surface.stop_all`, `surface.resume_all` and `surface.set_mode` record the
other commands the same way.

How an answer was given — "in Telegram", "on your phone" — is not part of the approval
service's decision, so the engine keeps it in an `AnswerLog`. The default,
`InMemoryAnswerLog`, lasts as long as the process. `AuditAnswerLog` reads the
`surface.decide` records back from the `AuditLogStore` the engine's audit trail writes to, so
every surface over the same signed audit log, and a restarted kernel, shows how a decision was
answered:

```rust,ignore
use std::sync::Arc;
use adk_surface::{AuditAnswerLog, SurfaceEngine};

let engine = SurfaceEngine::builder(source)
    .audit(trail)                                     // writes to `store` (signed audit log)
    .answers(Arc::new(AuditAnswerLog::new(store)))   // reads how each answer was given
    .build();
```

## The web API

`adk_surface::web::router(engine)` serves the engine under `/surface`, relative to `/api`.
Mount it with `ServerBuilder::add_api_routes` beside `adk_kernel::control::router(kernel)`, so
the server's auth middleware authenticates every request.

| Route (under `/api`) | Answer |
|---|---|
| `GET /surface/decisions?surface=&session=` | The caller's open decisions as cards, of one session with `session` |
| `GET /surface/decisions/{id}` | One decision, waiting or decided |
| `GET /surface/sessions/{session}/thread` | The session's entries |
| `GET /surface/sessions/{session}/feed` | Server-sent events: `entry` (with `id` = `seq`), `decisions`, `reset`; resumes after `Last-Event-ID` or `?after=` |
| `GET /surface/agents` | The roster |
| `GET /surface/status?session=` | The status line |
| `GET /surface/briefing?since=` | "While you were away" |
| `POST /surface/commands` | `{"surface", "origin", "command"}` → the outcome |
| `GET /surface/openapi.json` | The OpenAPI 3.1 document |

| Answer | When |
|---|---|
| `401` | No authenticated caller |
| `403` | A rule of §7.2 or §7.4 refuses the command |
| `404` | Not the caller's session or decision |
| `400` | A malformed command, or a chat or agent origin, which reach the engine in process from their adapters |

What widens a caller's answers comes from the server, never the request: the extractor sets
`surface.presence = confirmed` in the `RequestContext` metadata when the person confirmed it
is them, and `surface.pairedLocal = true` for a paired local terminal.

### Generated types

The OpenAPI document and the TypeScript types under `adk-surface/web/src/generated/` are
generated from the Rust types. `bash scripts/check-surface-api.sh` fails when they drift;
`bash scripts/check-surface-api.sh --write` regenerates them. CI runs the check.

## Related

- [The Control Plane](../kernel/control-plane.md) — the kernel routes the web API sits beside
- [Harness Modes and Approvals](../harness/modes-and-approvals.md) — held calls, standing rules, modes
