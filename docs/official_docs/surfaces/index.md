# Agent Surfaces: the Engine

`adk-surface` is the interface to an always-on agent (spec 3.3): the web home, the terminal,
chat, the phone, the desktop tray and voice are windows onto the same kernel. This page covers
the engine every surface runs on and the `web` feature's HTTP API; [The Web Home](web-home.md)
covers the page the `web` feature serves at `/ui/`. A decision is made once,
from any surface, and every other surface shows who made it and how.

> **Note:** `adk-surface` is Beta and opt-in (`default = []`). `adk-cli` depends on it, with
> `web`, through its `surface` feature, which is on by default so the `adk-rust` binary serves
> the web home; build `adk-cli` with `default-features = false` to leave it out. The umbrella
> crate reaches it through its own `surface` feature, which no tier includes.

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

`Source::keeps_ledger` says whether a source keeps the `/proc` ledger the briefing reads;
`LocalSource` keeps one only over a kernel with a ledger. Without one, `Briefing::recorded` is
`false`, and a surface says nothing is recorded rather than that nothing happened.

Both sources read decisions one session at a time where a surface shows one session
(`InboxQuery::with_session_id`, `GET /api/approvals?session=`; spec 3.3 §10 C1), and
`SurfaceEngine::session_decisions` returns that session's cards.

`ControlSource` reads the inbox over the same endpoint as its client: HTTP, or HTTP over the
kernel's Unix socket. It sends the client's bearer token (`ControlClient::bearer_token`);
`ControlSource::with_bearer_token` replaces it for the inbox alone. Every inbox call gives up
after 30 seconds with `surface.kernel_timeout`, as the client's own calls do. The inbox serves
requests without their fingerprints, so a thread links a held call to a request of the same
tool by the same agent: first each call to the request showing its arguments, then each call
still unlinked to the earliest request still unlinked, since a route may redact what approvers
see. A call with no request in the caller's inbox, because no route sent it there or the
caller is not its approver, has no link. A call held again before its request is resolved
keeps that request. Over the named pipe on Windows
the source keeps to `ControlClient`'s pending items, which carry no risk, agent or earlier
decisions: every decision counts as high risk, "Edit" is not offered, and a decision
answered elsewhere is no longer found.

## A decision on every surface

A card is drawn from the decision's inbox entry, never from agent output (S-I1):

| Part | Field | Example |
|---|---|---|
| Who and when | `agent`, `sinceText`, `statusText` | Finance · since 18:00 · Waiting for you |
| Heading | `heading` | Send email to [client email] |
| What the heading leaves out | `summary`, on a decision the kernel itself asks | Finance suggests this. Once you say yes, it runs every 5 minutes. |
| The exact call | `call.text`, `call.args`, `call.truncated` | `send_email(to: "…", subject: "…")` |
| How it got here | `timeline` | Asked at 18:00 — Waited through quiet time — Waiting for you |
| Actions | `allowed.options` | Allow once · Always allow for this person · Edit · Deny |
| Details | `riskText`, `reversibilityText`, `alsoShown` | Medium risk · Can't be unsent · Also on your phone and in Telegram |

The answers depend on the surface and on how the input was given: a spoken answer in the web
home follows the voice row. The high-risk column is about approving: the tray and voice keep
**Deny** on every decision, and only chat, where a high-risk decision never resolves (S-I6),
offers no answer there. A decision that needs more than one approver never offers "Always
allow", and the standing rule of "Always allow" is granted only once the request is approved.

A decision the kernel itself asks is drawn in the same plain words: an agent's proposed routine
shows its title as the heading, who suggests it and when it runs as the summary, and **Do it**
and **Not now** as its answers (§8.1 "Suggestion"); a question from `/proc` answers **That's
right** or **Forget it**. The kernel's tool name and the item's JSON stay in the exact call.

> **Note:** spec 3.3 §7.5 shows a change to a file as a diff against the current file before
> anything is written. The engine reads no workspace files, so a file change's card shows the
> call's new arguments (`write_file(path: …, content: …)`), not a diff; the diff needs the
> harness's change sets (spec 3.1 §7.26).

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
| `send` | Starts a turn; while a decision of the session is open (whoever may answer it), a turn runs, or earlier input waits, the message is queued instead and sent, oldest first, once the session is free, whichever surface answered the decision (S-I4) |
| `queue` | Queues the message as the session's next |
| `stopAll` | Freezes every agent, from any surface (S-I10) |
| `resumeAll` | Unfreezes, only from the web home or a paired local terminal (S-I10) |
| `setMode` | Changes the session's mode; making agents act more on their own is a standing permission, so only the web home, the phone and the terminal may loosen it |

> **Note:** §6 also names "steer", a message that reaches a turn while it runs. No runtime
> under the engine takes input in the middle of a turn, so there is no steer command: a message
> sent during a turn is queued as the session's next message, and **Stop all** stops a turn that
> has gone wrong.

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

A surface attached to a kernel over its control plane, such as the terminal, reads the same
answers from the kernel's record with `RecordAnswerLog`: the `surface.decide` lines among the
latest 500 actions of `GET /api/kernel/record`, and the answers it gave itself. A record line
says how an answer was given, not on which surface: `chat` names its channel, `touch` reads as
the phone, `pointer` as the home, `keyboard` as the terminal.

```rust,ignore
use std::sync::Arc;
use adk_surface::{ControlSource, RecordAnswerLog, SurfaceEngine};

let answers = Arc::new(RecordAnswerLog::new(client.clone()));
let engine = SurfaceEngine::builder(Arc::new(ControlSource::new(client, "adk-rust", "adk-rust")))
    .answers(answers)
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
| `GET /surface/sessions/{session}/queued` | `QueuedResponse`: what the caller typed while a decision of the session was open or a turn ran, sent oldest first once the session is free (§7.4 rule 2) |
| `GET /surface/agents` | The roster, and `primary`: the agent a message goes to when the person names none (`Source::primary_agent`; `LocalSource`'s kernel target, else its harness's agent), absent when the source has none |
| `GET /surface/status?session=` | The status line |
| `GET /surface/briefing?since=` | "While you were away" |
| `POST /surface/commands` | `{"surface", "origin", "command"}` → the outcome |
| `GET /surface/openapi.json` | The OpenAPI 3.1 document |

`adk_surface::web::take_over_router(engine, take_over)` adds `POST
/surface/decisions/{id}/take-over` (`{"version", "secret"}` → the outcome) beside a harness that
takes over (spec 3.3 §8.2, §10 H2): the secret goes to the harness's `TakeOver` for the one call
that asked, never to the inbox or the engine's records. A take-over card is headed by what to
type and offers only **Deny** as a decide; **Edit** is offered only where a card shows the
call's own arguments.

| Answer | When |
|---|---|
| `401` | No authenticated caller |
| `403` | A rule of §7.2 or §7.4 refuses the command; `surface.operator_only` for **Stop all** or **Resume all** from a caller outside the group `SurfaceEngineBuilder::stops_need` names (§11 S-I10); `surface.not_here` for every command from a paired device's token |
| `404` | Not the caller's session or decision |
| `400` | A malformed command; a chat or agent origin, which reach the engine in process from their adapters; a `decide` declaring the voice surface or origin, which reaches the engine through the voice surface (`cancelAnswer` stays open) |

What widens a caller's answers comes from the server, never the request: the extractor sets
`surface.presence = confirmed` in the `RequestContext` metadata when the person confirmed it
is them, and `surface.pairedLocal = true` for a paired local terminal.

A paired device's token (`adk.device.id` in the `RequestContext` metadata, spec 3.3 §10 K2)
is never the web home and never a paired local terminal, whatever the request declares: it
reads cards as the terminal row, its presence never comes from metadata, and every audit
record of its caller carries `device`. A record of an answer given after confirming it is
you carries `presence: "confirmed"`.

### Generated types

The OpenAPI document and the TypeScript types under `adk-surface/web/src/generated/` are
generated from the Rust types. `bash scripts/check-surface-api.sh` fails when they drift;
`bash scripts/check-surface-api.sh --write` regenerates them. CI runs the check.

## Related

- [The Web Home](web-home.md) — the page at `/ui/` that reads and acts through this API
- [The Terminal](terminal.md) — the `tui` feature: full screen, inline and JSON
- [Chat](chat.md) — the `channels` feature: the Telegram adapter over the kernel's channel contract
- [The Phone](phone.md) — the `mobile` feature: wake-ups from your own kernel, device answers and "Confirm it's you"
- [The Desktop Tray](desktop.md) — Experimental, a separate app: the home in a desktop window and the tray panel at `/ui/tray`
- [Voice and Video](voice.md) — the `voice` feature: spoken answers through the engine with a cancel window, the camera and the screen while their switches are on
- [Your Agents](agents.md) — agents a person adds by asking or by hand: the roster, New agent, settings, `/agent` and the decision that adds one
- [The Website's Views](website.md) — the `website` feature: a prebuilt website's onboarding, site view, weekly review and web editor at `/ui/website`
- [The Control Plane](../kernel/control-plane.md) — the kernel routes the web API sits beside
- [Harness Modes and Approvals](../harness/modes-and-approvals.md) — held calls, standing rules, modes
