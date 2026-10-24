# Spec 3.3: Agent surfaces — one kernel, every surface

| | |
|---|---|
| **Status** | Draft / RFC — rewritten 2026-10-24. Replaces the first draft ("one decision contract, five renderers", #936), which specified a contract-only crate and no surface a person could open |
| **Builds on** | [`spec-3.1-harness-agent.md`](spec-3.1-harness-agent.md) (the harness: modes, standing rules, approvals, memory, voice) and [`spec-3.2-agent-kernel.md`](spec-3.2-agent-kernel.md) (the always-on kernel: processes, wakes, agenda, quiet hours, event bus, control plane, `/proc`). Together they make an always-on personal agent; this spec is its interface |
| **Target** | Design labels only: work from specs 3.1–3.4 ships in the unreleased 3.0.0 (owner, 2026-10-21) |
| **Grounding commit** | `origin/main` @ `c5bdf4f1` for every file reference |
| **Evidence** | Three research reports (git-excluded, `reports/`): *Harness UI expectations 2026* (113 tracker issues, 2026-10-20), *Best agent surface design 2026* and *Personal agent interface 2026* (2026-10-24: Grok Bot, OpenAI dots, OpenClaw, the wider personal-agent field, terminal, web, mobile, generative UI, workflow tools, agent-OS interfaces) |
| **Concepts** | Twenty screens in light and dark, rendered under [`images/spec-3.3/`](images/spec-3.3/) |
| **Affected crates** | NEW `adk-surface`; `adk-kernel` (the `channels` contract and the read and write routes the surfaces need); `adk-server` (the bundled UI and UI-protocol code move out; AG-UI carries decisions); `adk-cli` (the terminal surface, `pair`); `adk-harness` (a personal preset); `adk-acp` ("always allow" as a rule); `adk-core` (park on timeout, inbox by session) |
| **Stability** | `adk-surface` enters at **Beta**; the `verb-ui` and `desktop` features at **Experimental** |

---

## 1. Summary

Spec 3.1 gave adk-rust a harness and spec 3.2 gave it a kernel. Together they are an always-on personal agent, so the interface is built for a person living with one: someone who wants to **decide, and later check, what their agents do while they are somewhere else**.

`adk-surface` is that interface. It is one opt-in crate with one cargo feature per surface: a web home, a terminal, chat channels, a phone app, a desktop tray, and voice and video. Every surface is a window onto the same kernel. Nothing lives in a surface: a decision is made once, from anywhere, and every other window shows who made it.

The 2026 personal agents (xAI's Grok Bot, OpenAI's dots, the open-source OpenClaw) converged on the same visible shape: a named agent in a thread, an activity view, and an approval card with a take-over. adk-surface keeps that shape and competes on what none of them document: decisions that wait instead of expiring, a decision bound to exactly one call and answerable once from any device, a record of every action that anyone can check, memory a person can read and correct, and spending and quiet-time limits on what agents do by themselves.

![One kernel, every surface](images/spec-3.3/overview-light.png)

## 2. What changed from the first draft

| First draft | This spec | Why |
|---|---|---|
| A contract-only `adk-surface` crate; screens spread across four crates | `adk-surface` ships the surfaces themselves; the shared contract is private plumbing under them | "Building a surface without a surface" (owner, 2026-10-23) |
| A developer harness in a terminal first | A person with an always-on agent first: the web home and a chat thread lead, the terminal drives the same kernel | Spec 3.2 made the agent resident; its decisions arrive while the person is away |
| No full-screen terminal UI (N3) | Full-screen by default, with inline and JSON modes | Claude Code made full-screen its default in 2026; Codex reportedly followed |
| Generative UI and workflow building deferred to the `adk-ui` and `adk-studio` repositories | `adk-ui` is absorbed into the `genui` feature; `adk-studio` is retired in favour of a workflow observer | Both repositories have one contributor; standards (AG-UI, MCP Apps, A2UI) replaced their protocols |
| The bundled web UI stays in `adk-server` | It moves into `adk-surface`'s `web` feature; `adk-server` keeps its APIs | `adk-server` compiles the UI for every user today ([`lib.rs:57`](../../adk-server/src/lib.rs), [`Cargo.toml:49`](../../adk-server/Cargo.toml)) |
| Engineering vocabulary on screen (held, parked, fingerprint, audit) | Plain words on every surface a person uses ([§8](#8-language)) | Owner, 2026-10-24 |

The owner's seven-point assessment (2026-10-23) resolves as follows:

| # | Point | Resolution |
|---|---|---|
| 1 | Move the native agent UI out of `adk-server` | Done in phase 1, with the UI-protocol code ([`ui_protocol.rs`](../../adk-server/src/ui_protocol.rs)) |
| 2 | The harness UI differs from the runtime UI | One web app whose panels follow what each session can do; developer panels are a separate track ([§12](#12-phased-delivery)) |
| 3 | Absorb `adk-ui`, retire the repository, do dynamic UI properly | `genui` feature; agent-drawn UI never contains approval controls ([§7.4](#74-rules-on-every-surface)) |
| 4 | The same for `adk-studio` | Retire the repository; build a workflow observer and debugger before any editor |
| 5 | The terminal is the most important surface | The terminal ships in phase 1 on the same engine as the web home |
| 6 | A native cross-platform mobile app | An installable web app with push first; a native wrapper when stated triggers fire ([§5.4](#54-phone)) |
| 7 | An experimental Agentrix OS verb UI | `verb-ui` feature at Experimental, without "OS" branding |

## 3. Goals and non-goals

**Goals**

- **G1.** A decision reaches the person wherever they are (web, phone, chat, terminal, tray, voice) and is answered once; every other window shows who answered.
- **G2.** Nothing is decided by silence. An unanswered decision waits through quiet time and restarts; it never expires into a yes or a no.
- **G3.** A person can see what happened while they were away, line by line, each line traceable to its record.
- **G4.** A person can read and correct what their agents remember, see what they may do without asking, and check that the record of what they did is complete.
- **G5.** Every surface uses the same plain words and the same visual language, in light and dark.
- **G6.** Nothing is forced on anyone: no crate depends on `adk-surface`, and each surface is a separate feature.

**Non-goals**

| Non-goal | Why |
|---|---|
| A cloud desktop the person watches the agent use | Rivals compete on watching; adk-rust's agents do not need watching, because every consequential act waits for a person |
| A mascot or animated character | The dots character drew criticism for inviting misplaced trust; presence is a name, an avatar and a status word |
| A gateway to many chat platforms | One reference adapter at a time; every channel is a source of untrusted input, as OpenClaw's security history shows |
| A native phone app first | The phone's jobs (decide, glance, steer) fit an installable web app; native waits for its triggers |
| A visual workflow editor first | Canvas builders retreated in 2026 (OpenAI's Agent Builder closes on 2026-11-30; Flowise archived itself); observers grew |

## 4. Who it is for

| Person | Main surfaces | What they do |
|---|---|---|
| Someone living with an always-on agent | Chat thread, phone, web home | Decide, glance at what happened, steer, stop |
| A command-line user | Terminal, plus the phone for decisions away from the desk | Everything the web home does, over SSH or locally |
| A developer building on adk-rust | Web home's developer panels, terminal | Watch runs, debug graphs, check traces and evaluations |

The jobs are narrow — decide, glance, steer, and occasionally review — so every screen has one main action.

## 5. Surfaces

| Order | Surface | Feature | The person's job there | Status on main |
|---|---|---|---|---|
| 1 | **Web home**, an installable web app served by the person's own kernel | `web` | Decide with full detail, review, change rules, stop | Kernel routes exist ([`control/mod.rs:96-140`](../../adk-kernel/src/control/mod.rs)); the bundled UI calls none of them |
| 1 | **Chat thread**, one adapter (Telegram first, Matrix second) | `channels` | Talk, get the doorbell, say yes once or no | `channels = []` is reserved and empty ([`Cargo.toml:56`](../../adk-kernel/Cargo.toml)) |
| 1 | **Terminal**, full-screen, inline or JSON | `tui` | Everything the web home does, from the keyboard | The kernel commands exist ([`cli.rs:386-665`](../../adk-cli/src/cli.rs)); no full-screen UI |
| 2 | **Phone**, the web home installed with push and pairing | `mobile` (implies `web`) | Decide from the lock screen, glance | Nothing |
| 3 | **Desktop tray**, the web home in a native shell plus a tray icon | `desktop` | Glance, say yes once, stop everything | Nothing |
| 3 | **Voice and video**, in the browser and the terminal | `voice` | Hands-free turns, show a window or the camera | `adk-rust --voice` answers waiting decisions ([`packs/voice.rs`](../../adk-harness/src/packs/voice.rs)) |

The web home and the chat thread ship together because each covers the other's weakness: a thread is a poor place to hold an inbox, review memory or change a rule, and it runs through someone else's servers; the web home is fully the person's own but needs a doorbell.

### 5.1 Web home

The system of record. Three sections share one header with the status line and **Stop all**.

**Threads.** The roster of agents on the left (avatar, name, status word), the selected agent's thread in the middle, and **Decisions** with a count badge and **Coming up** on the right. A decision appears inline in the thread, on its own background.

| Light | Dark |
|---|---|
| ![Web home, threads and decisions](images/spec-3.3/web-home-light.png) | ![Web home, threads and decisions, dark](images/spec-3.3/web-home-dark.png) |

**Today.** "While you were away": what was spent, whether quiet time was respected, how many things wait, then one line per event since the person left, each with a status pill and a link to its details. Questions from the kernel's `/proc` observations ([`control/mod.rs:136-140`](../../adk-kernel/src/control/mod.rs)) are answered here or on the phone, never assumed.

| Light | Dark |
|---|---|
| ![While you were away](images/spec-3.3/web-today-light.png) | ![While you were away, dark](images/spec-3.3/web-today-dark.png) |

**Memory & rules.** "Agent Memory and Rules — a record of what your agents know, remember and have done, and the settings for what they can do." Three bordered lists: **What your agents remember** (what you said, what you confirmed, and guesses you can confirm, fix or forget, with an undo for overnight tidy-ups); **What your agents can do** (how much they ask first per area, what is allowed without asking with an end date, quiet time and the daily spending limit, and a fixed line that spending money and publishing always ask first); **What your agents did** (a check that every action is on record and nothing was changed or deleted, the latest actions as sentences, and where the person can say yes from).

| Light | Dark |
|---|---|
| ![Agent memory and rules](images/spec-3.3/web-memory-rules-light.png) | ![Agent memory and rules, dark](images/spec-3.3/web-memory-rules-dark.png) |

### 5.2 Terminal

The same home for command-line users: a status line, the roster and what is coming up on the left, the thread on the right, and the decision as a box answered by key (`y` allow once, `a` always allow, `e` edit, `n` deny, `?` details). Esc never decides; typing while a decision is open is queued as the next message. `--ui inline` keeps output in the terminal's scrollback, and `--output json` emits one event per line with documented exit codes for CI. The terminal attaches to a kernel on another machine with the same device pairing the phone uses.

| Light | Dark |
|---|---|
| ![Terminal, light](images/spec-3.3/terminal-light.png) | ![Terminal, dark](images/spec-3.3/terminal-dark.png) |

### 5.3 Chat channels

The doorbell and the conversation. A decision card in chat carries a short summary, the agent and the risk; it can only **Allow once** or **Deny**, and only for low and medium risk. Anything else links to the home. Only paired accounts can message the agent; "stop all" from chat stops everything, and resuming happens only in the home.

| Light | Dark |
|---|---|
| ![Chat channel](images/spec-3.3/chat-light.png) | ![Chat channel, dark](images/spec-3.3/chat-dark.png) |

### 5.4 Phone

The web home installed on the phone, woken by push. The notification carries no details ("A decision is waiting — open the app to see it"). **Decisions** carries a notification badge on the heading and the tab; a decision answered elsewhere shows "You said yes in Telegram at 07:31 · done once". A high-risk decision shows the exact command and asks the person to confirm it is them before approving.

| Decisions, light | Decisions, dark | High risk, light | High risk, dark |
|---|---|---|---|
| ![Phone decisions](images/spec-3.3/phone-decisions-light.png) | ![Phone decisions, dark](images/spec-3.3/phone-decisions-dark.png) | ![High-risk decision](images/spec-3.3/phone-decide-light.png) | ![High-risk decision, dark](images/spec-3.3/phone-decide-dark.png) |

A native wrapper (Capacitor, or Tauri once it has first-party remote push) follows only when one of these triggers fires: iOS web push misses decisions in dogfooding; voice must keep working with the screen off; or the project funds a push relay and app-store accounts. Self-hosted projects with reliable iOS push (Bitwarden, Home Assistant, Matrix) rely on a relay that holds the Apple push key.

### 5.5 Desktop tray

The web home in a desktop shell (Tauri 2, after a WebKitGTK test on Linux) plus a tray panel: running state, spending against the limit, decisions with a badge, what is working now and next, a quick message box, **Open home** and **Stop all**.

| Light | Dark |
|---|---|
| ![Desktop tray](images/spec-3.3/tray-light.png) | ![Desktop tray, dark](images/spec-3.3/tray-dark.png) |

### 5.6 Voice and video

Voice through `adk-realtime` (Gemini Live or OpenAI Realtime). A spoken "allow once" shows on screen with a short cancel window; voice can say yes once or no to everyday actions only, and anything high risk needs a screen. The camera and screen sharing are input only while their switches are on. The record keeps a note of the answer, never the recording. The realtime runner already emits `tool.confirmation_requested` for a waiting call ([`events.rs:498`](../../adk-realtime/src/events.rs)).

| Light | Dark |
|---|---|
| ![Voice and video](images/spec-3.3/voice-light.png) | ![Voice and video, dark](images/spec-3.3/voice-dark.png) |

## 6. Architecture

| Layer | What it is | On main |
|---|---|---|
| **Kernel** | Processes running harness agents on any model; the approval inbox; the event bus; agenda, wakes and quiet time; memory; the activity ledger and the record | `adk-kernel`, `adk-harness`, `adk-core` |
| **Engine** (private module of `adk-surface`) | **Feed**: per-session events with a sequence number, a snapshot then a live tail, reconnect with `Last-Event-ID`. **Projection**: events become messages, decision cards, agent rows, status and briefing lines once, in Rust. **Commands**: decide, send, queue, steer, stop, change mode — each stamped with who and how (keyboard, touch, voice, chat, agent) | New |
| **Surfaces** | One feature each; they hold no state a reconnect cannot rebuild | New |

Process events come from the kernel bus, which keeps a recent window in memory ([`bus.rs:270`](../../adk-kernel/src/bus.rs)) and can stay that way, because every state it reports can be re-read from its store. The per-session feed derives its sequence from the session store, so a reconnect after a restart loses at most an unfinished stream of tokens, never a decision.

Every decision has one write path: `ApprovalService::decide` with the version the surface saw ([`approval.rs:1381-1400`](../../adk-core/src/approval.rs)). A second answer gets `approval.version_conflict` and is shown as "already decided by …", never retried silently.

The engine owns its stream (HTTP with server-sent events for reads, REST for commands, WebSocket only for a terminal pane and audio) and publishes OpenAPI generated from Rust, with TypeScript generated from it and a drift check in CI. AG-UI, ACP and MCP Apps are adapters over that stream, not its core.

### 6.1 Crate layout and features

`adk-surface` has `default = []`. No library crate depends on it; the `adk-rust` binary and the umbrella crate reference it only through opt-in features.

| Feature | Surface | Pulls in | Stability |
|---|---|---|---|
| `web` | Web home, served as an Axum router mounted on `ServerBuilder` or by `adk-rust ui` | `rust-embed` with assets built in CI | Beta |
| `mobile` | Web app manifest, service worker, Web Push, pairing, passkey route; implies `web` | a Web Push crate | Beta |
| `tui` | Terminal: full-screen, inline, JSON | `ratatui`, `crossterm`, `syntect`, `pulldown-cmark` | Beta |
| `channels` | Chat adapters over the kernel's `channels` contract | per adapter | Beta |
| `voice` | Voice and video in `web` and `tui` | `adk-realtime` | Beta |
| `genui` | Agent-drawn UI: the absorbed `adk-ui` core, A2UI renderers, AG-UI and MCP Apps adapters | — | Beta |
| `workflow` | Graph run observer and debugger | `adk-graph` with `time-travel` | Beta |
| `desktop` | Tauri shell and tray | `tauri` | Experimental |
| `verb-ui` | Surfaces that declare verbs an agent can call under approval | `adk-kernel` control client | Experimental |

Front ends: React, Vite and TypeScript for the web home, phone and desktop shell (the widest contributor pool; terminal, diff and graph components are JavaScript libraries anyway); `ratatui` for the terminal. npm dependencies are pinned and built in CI; nothing from npm runs at install time.

## 7. Decisions

### 7.1 Anatomy of a decision

Every surface draws a decision the same way, from the inbox entry — never from agent output.

| Part | Content | Example |
|---|---|---|
| Who and when | Agent avatar and name, how long it has waited, a status pill | F Finance · since 18:00 · *Waiting for you* |
| Heading | What will happen, in plain words | Send the October invoice to [CLIENT] |
| The exact call | The tool and its arguments, shown in full; a truncated call cannot be approved until expanded | `send_email(to: …, subject: …)` |
| How it got here | A short timeline | Asked at 18:00 — Waited through quiet time — Waiting for you |
| Actions | The verbs | Allow once · Always allow for this person · Edit · Deny |
| Details | Risk, reversibility, where else it is shown | Medium risk · Can't be unsent · Also on your phone and in Telegram |

Decisions sit on their own background (warm `#FFF8F3` in light, `#2A2432` in dark) so they are never mistaken for ordinary content. Counts of waiting decisions are a filled badge on every surface.

### 7.2 What each surface may decide

| Surface | Allow once | Always allow | Edit | Deny | High risk |
|---|---|---|---|---|---|
| Web home | Yes | Yes | Yes | Yes | Yes, after confirming it is you |
| Terminal | Yes | Yes | Yes | Yes | Yes, on a paired local terminal |
| Phone | Yes | Yes | Yes | Yes | Yes, after confirming it is you |
| Desktop tray | Yes | No (opens the home) | No | Yes | No (opens the home) |
| Chat | Yes, low and medium risk | No | No | Yes | No (links to the home) |
| Voice | Yes, low and medium risk, with a cancel window | No | No | Yes | No |

### 7.3 Waiting, not expiring

A decision nobody answers waits through quiet time and restarts. When the confirmation timeout passes and the run has an approval store, the call is recorded to the inbox and the run ends with it waiting (change C2 in [§10](#10-changes-outside-the-new-crate)); it is denied only when no store exists. Grok Bot's unattended approvals expire after about ten minutes; adk-rust's do not.

### 7.4 Rules on every surface

1. Esc never decides.
2. Typing while a decision is open is queued, never an answer.
3. An unanswered decision waits; it never becomes a yes or a no on its own.
4. Voice and chat say yes once or no, never "always", never money.
5. Agent-drawn UI never contains approval controls; a submit from it becomes a call that goes through approval like any other.
6. No agent approves its own call.

<a id="710-diff-before-write-and-honest-rewind"></a>

### 7.5 Edits shown before they happen, and honest undo

- **A change to a file is shown as a diff before anything is written.** The diff compares the pending change with the current file. **Edit** opens it for changes, and the edited call is checked again by policy before it runs. The edits of one turn are reviewed as one decision that binds each file's state before the change ([spec 3.1 §7.26](spec-3.1-harness-agent.md#726-change-sets-one-review-per-batch-apply_patch-and-the-before-hash)).
- **Undo says what it can and cannot undo.** Going back to an earlier step restores files and the conversation from snapshots taken before each turn ([spec 3.1 §7.27](spec-3.1-harness-agent.md#727-snapshots-and-rewind-amends-spec-33-710)). It keeps files the agent did not write, and lists from the action ledger what it cannot undo — an email sent, a payment, a push to a remote — before anything moves. This ships when spec 3.1 §7.26–§7.27 land ([§12](#12-phased-delivery), conditional).

## 8. Language

Surfaces a person uses speak plainly. The terminal and developer panels may use technical words.

| Principle | Rule |
|---|---|
| Plain words | Say what happens to the person: "Can't be unsent", not "non-idempotent"; "Waited through quiet time", not "parked" |
| Sentence case | No all-caps labels; headings, pills and buttons in sentence case |
| Mono only for data | Code, commands, times and model names; never labels |
| Name the outcome | A decision's heading says what will happen; the buttons carry the verbs |
| "Your agents" | The person owns them; headings say "What your agents remember", not "Memory" |
| No internal identifiers | Fingerprints, versions and record hashes stay out of person-facing screens |

### 8.1 Terms

| Concept | Surface word | On main |
|---|---|---|
| An agent doing work | **Working** | `RunState::Running` |
| An agent stopped on a decision | **Waiting for you** | `RunState::Waiting` with a held call |
| An agent idle until a wake | **Asleep until 07:00** | waiting with a wake set |
| An agent finished | **Done** / **Failed** | `Completed` / `Failed` |
| The list of pending decisions | **Decisions** (badge) | approval inbox |
| A decision answered elsewhere | **Decided** — "You said yes in Telegram at 07:31" | version conflict |
| A question from the kernel | **Question** — "Is that right?" | `/proc` observation |
| Something an agent offers to do | **Suggestion** — Do it / Not now | proposed agenda item |
| The overnight summary | **While you were away** | `/proc` ledger; no digest route yet |
| The record of actions | **What your agents did** — "Every action is on record" | hash-chained audit log |
| Memory | **What your agents remember**: *You said this* / *A guess* | Known / Assumed facts |
| How much agents act alone | **Just look · Suggest · Ask first · Go ahead** | `HarnessMode` `ReadOnly`, `Plan`, `Ask`, `Auto` ([`mode.rs:71`](../../adk-harness/src/mode.rs)) |
| Things allowed without asking | **Allowed without asking** — Extend / Stop allowing | standing rules |
| No proactive work, no notifications | **Quiet time** | `KernelConfig` quiet hours, set at start ([`kernel.rs:139`](../../adk-kernel/src/kernel.rs)) |
| The spending limit | **Daily spending limit** | run budget and spend ledger |
| Separate parts of a life | **Areas** (Work, Home) | partitions (types only) |
| Phones and apps allowed to decide | **Where you can say yes from** — Add a phone or app | not built |

### 8.2 Actions

| Action | Label | Where |
|---|---|---|
| Run this one call | **Allow once** | every surface |
| Allow from now on, scoped | **Always allow for this person** (or this folder, this command) | web, terminal, phone, IDE |
| Change the arguments, then allow | **Edit** | web, terminal, phone |
| Refuse | **Deny** | every surface |
| Approve a high-risk call | **Confirm it's you and approve** | phone, web |
| Type a secret yourself | **Take over**, then **Done** | phone, web |
| Stop every agent | **Stop all** | every surface |
| Restart after stopping | **Resume all** | web home only |
| Undo an overnight memory change | **Undo** | web home |
| Correct a guess | **That's right / Fix it / Forget it** | web home |

## 9. Visual language

The look follows [adk-rust.com](https://adk-rust.com): Inter and JetBrains Mono, rust orange as the one accent, two-tone display headings, a soft rust glow in one corner. Light and dark come from one set of tokens, and every dark screen is the light screen rendered with the dark set.

| Token | Light | Dark |
|---|---|---|
| Background | `#FFFFFF` | `#0A1628` |
| Card | `#FFFFFF` | `#15233B` |
| Raised (chips, switchers) | `#F6F7F9` | `#1C2B45` |
| Border | `#E6E8EC` | `#2A3D5C` |
| Text / secondary / labels | `#0F172A` / `#475569` / `#64748B` | `#F1F5F9` / `#D5DEE8` / `#A7B4C6` |
| Brand (display, dots) | `#F74C00` | `#F74C00` |
| Accent text and primary button | `#C03A00` with white text | `#FF9C78` text; `#F74C00` button with navy text |
| Decision background | `#FFF8F3` | `#2A2432` |
| Stop control | navy `#0A1628` | light `#F1F5F9` |

Text colours target WCAG AA contrast (4.5:1) against their background; the bright brand orange appears only on large type and dots. Agents have rounded monogram avatars in their own colour. Status uses a word and a colour, never colour alone.

## 10. Changes outside the new crate

| # | Crate | Change | Needed by |
|---|---|---|---|
| K1 | adk-kernel | The `channels` contract: a channel sink, a binding store for paired accounts, and a wake for messages from a channel | Chat, phase 1 |
| K2 | adk-kernel | Device pairing: short-lived pairing codes, scoped and revocable device tokens (`decide`, `read`, `stop`), stored hashed | Phone, chat, remote terminal |
| K3 | adk-kernel | Routes to read and change quiet time, the daily spending limit and the mode while running; today they are set at start | Memory & rules |
| K4 | adk-kernel | A "while you were away" route over the `/proc` ledger with a cursor per device | Today |
| K5 | adk-kernel | Memory and record routes: list, confirm, fix, forget, undo; read the record and check it | Memory & rules |
| K6 | adk-kernel | Partitions (areas) with their own mode, quiet time and budget | Areas |
| H1 | adk-harness | A `personal` preset whose workspace is not a code repository | Phase 1 |
| H2 | adk-harness | Take over: a secret typed by the person goes to the tool, never to the model | Phone, web |
| C1 | adk-core | `InboxQuery` gains a session filter | Per-session decisions |
| C2 | adk-core | A confirmation that times out waits in the inbox when a store exists ([§7.3](#73-waiting-not-expiring)) | Every surface |
| S1 | adk-server | The bundled UI and the UI-protocol code move to `adk-surface`; `adk-server` keeps its APIs | Phase 1 |
| S2 | adk-server | AG-UI carries decisions as interrupts; today held calls are dropped ([`runtime.rs`](../../adk-server/src/rest/controllers/runtime.rs)) | Third-party front ends |
| S3 | adk-server | The bundled client leaves the `adk_ui` profile before its sunset on 2026-12-31 ([`ui_protocol.rs:67`](../../adk-server/src/ui_protocol.rs)) | Phase 1 |
| A1 | adk-acp | "Always allow" creates a standing rule; today only allow once and reject once exist ([`permission.rs:61-62`](../../adk-acp/src/server/permission.rs)) | IDEs |

## 11. Security invariants and audit records

| # | Invariant |
|---|---|
| S-I1 | Every decision shown on any surface corresponds to one inbox entry; no surface invents one |
| S-I2 | Every answer goes through `decide` with the version the surface saw; a conflict is shown, never retried |
| S-I3 | An unanswered decision waits when a store exists and never proceeds on its own |
| S-I4 | Input typed, queued or tapped outside a decision's own controls never answers it |
| S-I5 | Chat cards and push notifications carry no arguments: chat a short summary, push nothing but a wake-up |
| S-I6 | Messages from chat are untrusted input; only paired accounts reach an agent, and high-risk decisions never resolve in chat |
| S-I7 | Device tokens are scoped, hashed, revocable and expiring; pairing never widens the server's bind (loopback by default; a non-loopback bind requires a token or SSO) |
| S-I8 | Every surface checks the caller against the session owner |
| S-I9 | Every string from a model, a tool or a file is escaped before display (`terminal_safe` in the terminal, HTML escaping on the web) |
| S-I10 | Stopping is one action from any surface; resuming happens only in the web home or on a paired local terminal |

Every answer, pairing, notification and stop is written to the signed audit log with the surface and how it was given (keyboard, touch, voice, chat), so "What your agents did" can say "You let Finance send one email, from Telegram".

## 12. Phased delivery

Each phase ships only when its scenario passes end to end on SQLite and on one shared store (PostgreSQL or Redis).

| Phase | Ships | Scenario and pass condition |
|---|---|---|
| **1. Decisions reach you** | Engine; `web` (threads, decisions, stop); `tui` (full-screen, inline, JSON); `mobile` push and pairing; one chat adapter; K1, K2, H1, C1, C2, S1–S3 | **Approve it tomorrow.** Quiet time 22:00–07:00. At 18:00 a routine prepares to email an invoice and waits for a decision. Nobody answers; the kernel is killed with `kill -9` and started again. At 07:30 the person taps Allow once in the chat thread. *Pass:* nothing runs before the tap and nothing notifies during quiet time; the email is sent exactly once; the phone's card shows "You said yes in Telegram"; the terminal shows the same; the record names the channel. **A stranger writes:** an unpaired account gets a pairing code and starts nothing |
| **2. While you were away** | Today; morning briefing when quiet time ends; routines; K3, K4 | **The morning after.** Over one night two routines finish, one fails, an agent suggests a payment, the person allows a call from the phone, and the kernel restarts. *Pass:* one briefing at 07:00; its counts and spending match the ledger exactly across the restart; every line links to its record |
| **3. Memory and rules** | Memory & rules; questions from `/proc`; permissions with end dates; the record check; K5, H2 | **Correct me, then check.** An overnight tidy-up merges two notes. *Pass:* Undo restores both; a confirmed guess becomes "You said this"; a forgotten note is gone from the next turn; stopping an "allowed without asking" rule makes the next identical call wait; "Every action is on record" agrees with `adk-rust audit verify` |
| **4. Areas and roster** | Areas with their own mode, quiet time and limit; avatars and roster; K6 | **Work stays at work.** Home asks first; Work goes ahead with a 09:00 weekday routine. *Pass:* a Work agent reading the Home calendar waits even in Go ahead; the routine skips Saturday; spending per agent sums to the day's total |
| **5. Presence and voice** | `desktop` tray; `voice` in the browser and terminal | **Speak, glance, stop.** *Pass:* a spoken "allow once" runs an everyday call once after the cancel window; a high-risk call is refused by voice and approved on screen; the tray changes within one event; Stop all from the tray stops everything within about a second |
| **Developer track** (after phase 1) | `workflow` observer and debugger; `genui`; ACP "always allow" (A1); AG-UI decisions | **Approve, rewind, fork.** A graph with a model step, a payment-like HTTP call and a decision. *Pass:* approved from the observer; a fork at an earlier step leaves the original intact and the HTTP call is not repeated. **Governed form:** an agent-drawn form's submit waits for approval like any call; a fake "Approved" card never renders |
| **Experimental** | `verb-ui` | **One verb, three origins:** the same request by text, click and voice produces records that differ only in how it was given; a second agent's request through verbs waits for a person |
| **Conditional** | Native phone wrapper and push relay; honest rewind (spec 3.1 §7.26–§7.27); a live view for take-over | Each starts only when its trigger fires ([§5.4](#54-phone)) |

## 13. Open decisions

| # | Decision | Recommendation |
|---|---|---|
| 1 | First chat platform | Telegram, then Matrix; never an unofficial WhatsApp transport |
| 2 | What a chat card may decide | Allow once and deny at low and medium risk, unattended work included |
| 3 | What leaves the machine through chat and push | Chat: summary, risk and agent. Push: a wake-up only |
| 4 | Where the channel code lives | The contract in `adk-kernel`'s `channels` feature; adapters in `adk-surface` |
| 5 | Who issues device tokens | The kernel, stored hashed, listed and revocable; central revocation through `adk-auth` later |
| 6 | Waiting on timeout (C2) as the default in `adk-core` | Yes, for every run with an approval store; `RunConfig` can opt back into deny |
| 7 | First-run defaults | Ask for quiet time and a daily spending limit at first run and always show both |
| 8 | Who writes the morning briefing | Counts and links from the stores; the model may add prose only about the person's own items |
| 9 | One agent or many | One primary agent, specialists in the roster, areas as partitions |
| 10 | Reaching the home from outside | Loopback by default; a documented private-network recipe; never an open tunnel without the app's own sign-in |
| 11 | `adk-ui` retirement | Drop the AWP profile unless a user exists; publish the React package as `@zavora-ai/adk-surface-react`; deprecate, do not yank |
| 12 | `adk-studio` and the enterprise line | Confirm both licences before reusing code; observer open, tenancy and release pipelines commercial |
| 13 | Debug eval tab | Build it on `adk-eval` so results match `adk-rust eval` |
| 14 | Mobile investment | Installable web app, with the triggers in [§5.4](#54-phone) written down |

**Decided (owner, 2026-10-23 and 2026-10-24):** `adk-surface` is the agent UI, opt-in, one feature per surface; every surface drives the same kernel; the terminal ships in light and dark, full-screen by default; plain words and sentence case on every person-facing surface; decisions have their own background and a count badge; every screen exists in light and dark.

## 14. Appendix: grounding on `origin/main` @ `c5bdf4f1`

| Area | Reference |
|---|---|
| Bundled UI compiled for every `adk-server` user | [`adk-server/src/lib.rs:57`](../../adk-server/src/lib.rs), [`adk-server/Cargo.toml:49`](../../adk-server/Cargo.toml) |
| `adk_ui` profile sunset | [`adk-server/src/ui_protocol.rs:67`](../../adk-server/src/ui_protocol.rs) |
| AG-UI translator without decisions | [`adk-server/src/rest/controllers/runtime.rs`](../../adk-server/src/rest/controllers/runtime.rs) (no held-call handling) |
| Kernel routes | [`adk-kernel/src/control/mod.rs:96-140`](../../adk-kernel/src/control/mod.rs): processes, events, attach, freeze, agenda, drivers, `/proc` ledger, baselines and observations |
| Event bus window | [`adk-kernel/src/bus.rs:270`](../../adk-kernel/src/bus.rs) |
| Reserved channels feature | [`adk-kernel/Cargo.toml:56`](../../adk-kernel/Cargo.toml) |
| Quiet hours set at start | [`adk-kernel/src/kernel.rs:139`](../../adk-kernel/src/kernel.rs) |
| Versioned decisions | [`adk-core/src/approval.rs:1381-1400`](../../adk-core/src/approval.rs) |
| Harness modes | [`adk-harness/src/mode.rs:71`](../../adk-harness/src/mode.rs) |
| Voice answers waiting decisions | [`adk-harness/src/packs/voice.rs`](../../adk-harness/src/packs/voice.rs), [`adk-realtime/src/events.rs:498`](../../adk-realtime/src/events.rs) |
| ACP options | [`adk-acp/src/server/permission.rs:61-62`](../../adk-acp/src/server/permission.rs) |
| Kernel commands | [`adk-cli/src/cli.rs:386-665`](../../adk-cli/src/cli.rs) |
