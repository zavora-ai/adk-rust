# The Web Home

The web home is the system of record for your agents (spec 3.3 §5.1): the place to decide with
full detail, review what happened, change what agents may do, and stop everything. It is a
React app that `adk-surface`'s `web` feature embeds and serves at `/ui/`, and it reads and acts
only through the engine's HTTP API under `/api/surface` and the kernel and harness routes
beside it. Nothing lives in the page that a reload cannot rebuild.

![Threads, with a decision waiting](../images/surfaces/web-threads-light.png)

> **Note:** the web home replaces the runtime UI that `adk-server` bundled at `/ui/` (spec 3.3
> §10 S1). `adk-server` keeps its APIs and serves no page of its own; see
> [Moving from the bundled runtime UI](#moving-from-the-bundled-runtime-ui).

## Open it

| Command | What it serves |
|---|---|
| `adk-rust serve` | The harness with the approval inbox and the web home at `http://127.0.0.1:8080/ui/`; loopback callers are the operator |
| `adk-rust up` | The resident kernel: turns run as processes, and the home shows the process table, the agenda and the `/proc` ledger |
| `adk-rust serve --auth-token <token>` | The same behind a bearer token, required on any non-loopback `--host`; the page asks for the token once and sends it as `Authorization: Bearer` |
| `adk-rust serve --agent chat` | The plain chat agent's REST API under `/api`, without the home |

`adk-rust up --mode ask` on `gemini-3.5-flash-lite`, asked to create `note.txt`: the harness
holds the write, the home shows it as a decision, and **Allow once** runs it once.

| Held (light) | Allowed once (dark) |
|---|---|
| ![A held write in the home](../images/surfaces/web-live-held-light.png) | ![The write ran once, dark](../images/surfaces/web-live-ran-dark.png) |

The `adk-rust` binary turns the home on with its `surface` feature, which is on by default.
The home serves the harness: a message typed in it runs as the operator the server admitted,
with their mode and standing rules, and **Stop all** freezes the one kill switch the server's
runs, the background runs and the home's turns share. `adk_cli::Launcher` serving a chat agent
serves its REST API only; to put the home on your own server, mount it as below.

## Mount it on your own server

`adk_surface::web::router(engine)` serves the engine's API; `adk_surface::web::home()` serves
the page as root routes. Both go on `ServerBuilder`, behind the extractor that identifies the
caller: every route of the API checks its caller against the session owner (spec 3.3 §11 S-I8)
and answers `401` without one.

```rust
use std::sync::Arc;

use adk_core::{AgentLoader, ApprovalService, GovernanceControl, RequestContext, RunConfig};
use adk_runner::Runner;
use adk_server::{RequestContextError, RequestContextExtractor, ServerBuilder, ServerConfig};
use adk_session::SessionService;
use adk_surface::{LocalSource, SurfaceEngine};
use async_trait::async_trait;

/// Admits every request as `ana`; a deployment uses its own authentication.
struct Owner;

#[async_trait]
impl RequestContextExtractor for Owner {
    async fn extract(
        &self,
        _parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        Ok(RequestContext {
            user_id: "ana".into(),
            scopes: vec!["owner".into()],
            metadata: Default::default(),
        })
    }
}

fn app(
    agent: Arc<dyn adk_core::Agent>,
    loader: Arc<dyn AgentLoader>,
    sessions: Arc<dyn SessionService>,
    approvals: Arc<dyn ApprovalService>,
) -> adk_core::Result<axum::Router> {
    // One kill switch for the server's runs and the home's turns, so Stop all stops both.
    let control = GovernanceControl::new();
    let runner = Runner::builder()
        .app_name("home")
        .agent(agent)
        .session_service(sessions.clone())
        // Held calls reach the inbox the home draws its cards from.
        .run_config(RunConfig::builder().approval_service(approvals.clone()).build())
        .governance(control.clone())
        .build()?;
    let source = LocalSource::builder("home")
        .sessions(sessions.clone())
        .approvals(approvals)
        .runner(Arc::new(runner))
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    let config = ServerConfig::new(loader, sessions)
        .with_request_context(Arc::new(Owner))
        .with_governance(control);
    Ok(ServerBuilder::new(config)
        .add_api_routes(adk_surface::web::router(engine))
        .add_root_routes(adk_surface::web::home())
        .build())
}
```

Give the source `.harness(harness)` for modes and "Always allow", and `.kernel(kernel, app)`
for processes, quiet time, spend and the `/proc` ledger; mount
`adk_kernel::control::router(kernel)` beside the engine so the home can read the process table
and the agenda. [The Engine](index.md#sources) lists what each source adds.

The page is public like any static asset; its content security policy allows scripts, styles
and fonts from the server only and no framing. `/` and `/ui` redirect to `/ui/`, and each
section (`/ui/today`, `/ui/memory`, `/ui/developer`) serves the same page.

## The three sections

One header carries the brand, **Threads**, **Today** and **Memory & rules**, the status line
(mode, what was spent of the daily limit, quiet time) and **Stop all**. After a stop, **Resume
all** appears; resuming happens only here or on a paired local terminal (§11 S-I10).

| Section | What it shows | Where it reads it |
|---|---|---|
| **Threads** | The roster (avatar, name, status word), the selected agent's thread with its decisions inline on their own background, **Decisions** with a filled count badge, **Coming up**. A thread follows one conversation: the session the agent's row names when it is first shown, or a new one, kept for the tab until a decision of another session is opened from **Decisions** or **Today** | `GET /surface/agents`, the session feed, `GET /surface/decisions`, the kernel's processes and agenda |
| **Today** | "While you were away": spent, quiet time, how many things wait, one line per event since you left with a status pill and its actions, and today's routines with **Add a routine** | `GET /surface/briefing`, `GET /surface/status`, the kernel's agenda; **Do it** and **Not now** answer a suggestion through `POST /kernel/agenda/{id}/approve` or `reject` |
| **Memory & rules** | "Agent Memory and Rules": **What your agents remember**, **What your agents can do** and **What your agents did**, as bordered lists | `GET /rules` (with **Stop allowing**), `GET /surface/status`, `GET /surface/briefing`; **Where you can say yes from** from `GET /kernel/devices` and `GET /kernel/channels`, with **Remove** and **Add a phone or app** (a pairing code from `POST /kernel/pairing`) when the kernel pairs devices |

| Light | Dark |
|---|---|
| ![Threads](../images/surfaces/web-threads-light.png) | ![Threads, dark](../images/surfaces/web-threads-dark.png) |
| ![Today](../images/surfaces/web-today-light.png) | ![Today, dark](../images/surfaces/web-today-dark.png) |
| ![Memory and rules](../images/surfaces/web-memory-light.png) | ![Memory and rules, dark](../images/surfaces/web-memory-dark.png) |

### What is read-only until the kernel serves it

Some controls need kernel routes that are not built yet (spec 3.3 §10). The home shows what
exists, read-only, and renders each control through a typed seam (`adk-surface/web/src/seams.ts`)
that a client implementing the route fills, with no change to the screens.

| Control | Needs | Shown today |
|---|---|---|
| Quiet time, the daily spending limit and the mode: **Change**, **Just look · Suggest · Ask first · Go ahead** | K3 | The values from the status line |
| Today since you last looked, quiet time **Respected**, and a record link per line | K4 | The last 24 hours (the engine's default); the quiet hours, with no claim about whether they were kept |
| **What your agents remember**: **That's right**, **Fix it**, **Forget it**, **Undo** | K5 | "Nothing your agents remember is shown here yet." |
| **Every action is on record** and **Check again** | K5 | The latest lines of the activity ledger |
| **Confirm it's you and approve** | The passkey route of the phone package | The answer, refused until the server confirms presence |

## Decisions

Every card is drawn from its inbox entry (spec 3.3 §7.1, §11 S-I1) and answers through
`POST /surface/commands` with the version it showed (S-I2).

| Rule | Behaviour |
|---|---|
| One write path | **Allow once**, **Always allow for this person** (or this folder, this command), **Edit** and **Deny** send `decide` with the card's `version` |
| The version you saw | A card keeps the version it showed when a newer one arrives; an answer to it comes back as changed, and the card shows the new one with "This decision changed since you saw it." |
| Answered elsewhere | A conflict shows "You said yes in Telegram at 07:31" as the card's last step; the answer is never retried |
| Answered here | The card stays in the thread, decided, with who decided and where as its last step |
| **Edit** | Offered only where the card shows the call's own arguments: never over a redacted view or an MCP server's form, which would be submitted as written |
| **Take over**, then **Done** | A tool that asks for a password or a one-time code shows what asks and what for; what the person types goes through `POST /surface/decisions/{id}/take-over` to that one call, never the inbox, the model or a log line (§8.2, §10 H2) |
| The exact call | A truncated call cannot be approved until **Show the whole call** expands it |
| Enter and Esc | Neither ever decides; Esc closes **Edit** without deciding (§7.4) |
| Typing while a decision is open | Queued as the next message and shown as "Sent after this card is answered", never an answer (S-I4); the page reads the queue back from `GET /surface/sessions/{session}/queued`, so a reload or a switch of threads still shows it |
| High risk | Offered as **Confirm it's you and approve**; the server, not the page, says the person confirmed it is them |

| Take over (light) | Take over (dark) |
|---|---|
| ![Take over: the bank's one-time code](../images/surfaces/web-take-over-light.png) | ![Take over, dark](../images/surfaces/web-take-over-dark.png) |

Every command carries how it was given — keyboard, pointer or touch — and the engine writes it
to the audit record, so **What your agents did** can say "You let Finance send one email".

## Light, dark, keyboard and phone

- **Themes**: light and dark come from one set of tokens (spec 3.3 §9). The page follows the
  device's setting; the switch in the header cycles device, light and dark and remembers the
  choice in this browser only.
- **Keyboard**: every control is reachable, focus is always visible, and **Skip to the content**
  is the first stop.
- **Phone**: under 640 px the page is one column — Decisions first, then the roster as a row,
  then the thread.

| Light | Dark |
|---|---|
| ![The phone layout](../images/surfaces/web-phone-light.png) | ![The phone layout, dark](../images/surfaces/web-phone-dark.png) |

The screenshots on this page render the example of spec 3.3 §5.1 (`/ui/fixture.html`), with
the routes the kernel does not serve yet filled by the example's seams.

## The developer panel

`/ui/developer` keeps what was worth keeping of the bundled runtime UI: the agent's topology,
the runtime services and UI protocols the server advertises, a session's telemetry and its
state, read from `adk-server`'s own APIs (`/api/apps`, `/api/ui/agents/{name}`,
`/api/debug/trace/session/{id}`). Developer panels may use technical words (§8).

## Building the front end

The app lives in `adk-surface/web` (React, Vite and TypeScript, pinned dependencies, nothing run
at install time). Its build is committed under `adk-surface/assets/web`, and CI fails when the
build drifts from the source.

```bash
cd adk-surface/web
npm ci
npm run verify     # type check, component tests, build into ../assets/web
npm run dev        # against `adk-rust serve` on 127.0.0.1:8080
npm run e2e        # Playwright against the scripted kernel of examples/scripted_home.rs
```

`npm run dev` also serves `/ui/fixture.html`, the screens over the example of spec 3.3 §5.1.
The end-to-end test starts `cargo run -p adk-surface --features web --example scripted_home`
in a new, empty workspace each run (`HOME_E2E_BIN` names an example already built), opens the
home, sees the call the scripted agent's harness holds, allows it once and checks that it ran
once. Against a home already running, set `HOME_E2E_URL` and `HOME_E2E_WORKSPACE` to its
address and its workspace.

## Moving from the bundled runtime UI

| Before | Now |
|---|---|
| `adk-server` served a runtime UI at `/`, `/ui/` and `/ui/assets/config/runtime-config.json` | `adk-server` serves its APIs only; those paths answer `404` |
| The UI ran agents through `/api/run_sse` with `x-adk-ui-protocol: adk_ui` | The web home reads the engine's feed and sends `send` commands; it never uses the `adk_ui` profile (§10 S3) |
| `adk_cli::Launcher` serve mode always had a page | It serves the REST API; `adk-rust serve` and `adk-rust up` serve the web home, and your own server mounts it as shown below |

To keep a page on a server you build with `adk-server`, mount the web home as shown in
[Mount it on your own server](#mount-it-on-your-own-server).

## Related

- [The Engine](index.md) — the feed, projection, commands and the web API the home reads
- [The Control Plane](../kernel/control-plane.md) — the kernel routes beside the engine
- [Serving the Harness](../harness/serving.md) — `adk-rust serve`, its authentication and the inbox
