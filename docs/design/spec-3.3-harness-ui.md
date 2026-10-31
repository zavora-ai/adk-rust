# Spec 3.3: Agent surfaces — one kernel, every surface

| | |
|---|---|
| **Status** | Draft / RFC — rewritten 2026-10-24. Replaces the first draft ("one decision contract, five renderers", #936), which specified a contract-only crate and no surface a person could open. Amended 2026-10-25: agents you create ([§5.7](#57-your-agents), K7) |
| **Builds on** | [`spec-3.1-harness-agent.md`](spec-3.1-harness-agent.md) (the harness: modes, standing rules, approvals, memory, voice) and [`spec-3.2-agent-kernel.md`](spec-3.2-agent-kernel.md) (the always-on kernel: processes, wakes, agenda, quiet hours, event bus, control plane, `/proc`). Together they make an always-on personal agent; this spec is its interface |
| **Target** | Design labels only: work from specs 3.1–3.4 ships in the unreleased 3.0.0 (owner, 2026-10-21) |
| **Grounding commit** | `origin/main` @ `c5bdf4f1` for every file reference, except [§5.7](#57-your-agents) and its rows in §6–§14, which cite `origin/main` @ `c991dae3` |
| **Evidence** | Three research reports (git-excluded, `reports/`): *Harness UI expectations 2026* (113 tracker issues, 2026-10-20), *Best agent surface design 2026* and *Personal agent interface 2026* (2026-10-24: Grok Bot, OpenAI dots, OpenClaw, the wider personal-agent field, terminal, web, mobile, generative UI, workflow tools, agent-OS interfaces) |
| **Concepts** | Twenty screens in light and dark, rendered under [`images/spec-3.3/`](images/spec-3.3/) |
| **Affected crates** | NEW `adk-surface`; `adk-kernel` (the `channels` contract, the read and write routes the surfaces need, and agents a person adds); `adk-server` (the bundled UI and UI-protocol code move out; AG-UI carries decisions; a live target table); `adk-cli` (the terminal surface, `pair`, the agent factory and `/agent`); `adk-harness` (a personal preset; `propose_agent`); `adk-acp` ("always allow" as a rule); `adk-core` (park on timeout, inbox by session) |
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

**Threads.** The roster of agents on the left (avatar, name, status word), the selected agent's thread in the middle, and **Decisions** with a count badge and **Coming up** on the right. A decision appears inline in the thread, on its own background. The roster ends with **New agent**, which adds an agent of the person's own ([§5.7](#57-your-agents)).

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

The roster on the left lists the primary agent and the person's own agents ([§5.7](#57-your-agents)). Ctrl-G or `/agents` opens the Agents view, where agents are added, changed, paused and removed; Tab moves to the next agent unless a `/` command is being typed.

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

### 5.7 Your agents

A person adds agents of their own beside the primary agent, by asking or by hand. Each one is kept by the kernel, appears in the roster on the left of the web home and the terminal, keeps working across restarts, and is changed, paused or removed from either surface. An agent may suggest a new agent; only a person adds one.

**What `main` does today.** Every agent is fixed when the kernel is built:

| On `main` | Consequence |
|---|---|
| `adk-rust up` registers its targets once — `adk-rust`, `harness.goal`, and `adk-rust@<area>` per area ([`up.rs:314-328`](../../adk-cli/src/harness_cli/up.rs)) — through `KernelBuilder::agent` and `partition_agent` ([`kernel.rs:1562, 1757`](../../adk-kernel/src/kernel.rs)); `build` copies them into each queue's worker ([`kernel.rs:1955`](../../adk-kernel/src/kernel.rs)) | An agent added later cannot run until a restart; a worker fails a run whose target it lacks, not retryable ([`worker.rs:1079`](../../adk-server/src/background/worker.rs)) |
| Progress, usage and the ledger reach a kernel through sources attached at start and routed by area ([`process.rs:586`](../../adk-kernel/src/process.rs), [`supervise.rs:201`](../../adk-kernel/src/supervise.rs)) | An agent built later would spend without its spending reaching its processes |
| The roster is the latest process per target ([`local.rs:483`](../../adk-surface/src/local.rs)); display names are set once, when the engine is built ([`engine.rs:287`](../../adk-surface/src/engine.rs)) | An agent that has never run is not listed |
| A source starts every turn on its one target ([`local.rs:557`](../../adk-surface/src/local.rs)) | Every roster row messages the primary agent |
| Agent definitions exist only in the managed form (`HarnessDef`, `KernelDef`), which `adk-rust up` does not build | Nothing a person defines persists |

**Two ways in, one store.**

| | By asking | By hand |
|---|---|---|
| Where | Any message to ADK-Rust; **Describe it** in the web home or in the terminal's Agents view, which opens a thread with ADK-Rust holding the request | **Set it up yourself** in the web home or in the terminal's Agents view; `/agent new` in the terminal; `adk-rust agents new` |
| What happens | ADK-Rust calls `propose_agent`; the kernel checks the suggestion against ADK-Rust's own limits, keeps it as suggested, and puts one decision in **Decisions** | The kernel checks the definition and shows **Here is what Finance can do** with its risk; nothing is kept until the person confirms |
| Who decides | A person, in the web home, on the phone or in the terminal ([§7.2](#72-what-each-surface-may-decide)) | The person filling in the form, in the web home or the terminal |
| Limits | No more than ADK-Rust may do, in its own area (S-I12) | The area's limits |

**The definition.**

| On screen | Holds | Rules |
|---|---|---|
| Name | "Finance" | At most 40 characters, escaped everywhere (S-I9); unique among the kernel's active, paused and suggested agents, ignoring case; never the primary agent's name or "You"; can be changed |
| Colour | One of the eight avatar colours ([`project.rs:763`](../../adk-surface/src/project.rs)) | A named colour, never a free value and never a decision background ([§9](#9-visual-language)); kept across a rename |
| What it does | One line, shown in the roster and on its card | |
| Instructions | How it works | Secrets redacted and injection patterns labelled before they are shown or kept, as fetched text is (spec 3.1 I47); a match is labelled, never dropped. Instructions shape what the agent says and tries, never what it may do: its mode, tools and rules are code (spec 3.1 I32) |
| Area | Its partition (spec 3.2 §7.8) | Fixed: moving it is adding a new agent in the other area (K-I15) |
| How much it asks first | Just look, Suggest, Ask first or Go ahead | The area's mode or stricter; the stricter of the two governs every step |
| Runs on | A provider and a model | The primary agent's model by default; any provider whose key the kernel already has; keys stay in the kernel's environment, never in the definition |
| What it can use | Files: none, read, or read and change, in the area's workspace or one folder inside it; a shell: none, in the sandbox, or on this computer; the web: none, listed sites, or any site; packs; drivers mounted for its area | Adding an agent never mounts a driver: mounting stays its own approval (K-I13) |
| Daily spending limit | Optional | At most the area's; its own unattended work does not start once it is spent, as the area's limit works (K-I11) |
| Allowed without asking | Its own standing rules | Made only by a person (spec 3.1 I2). A new agent starts with nothing; "Always allow" on its cards makes rules for it alone, and its settings can copy one of the person's rules to it (S-I19) |

Its routines are its scheduled processes, added from its settings with the form Today uses; they are not part of the definition, and a suggestion carries none.

The kernel also keeps, never on a person-facing screen: the agent's id, its state (suggested, active, paused, removing, removed), how it was added (by hand, or suggested by which agent in which session), who added it and when, a version per change, and a digest of the definition that a decision binds to.

**Where it lives.** A kernel store, `AgentStore`, holds definitions beside the partition and driver stores, on every kernel backend — memory, SQLite in `harness.db`, PostgreSQL, Redis, MongoDB, Firestore and Neo4j — with one conformance suite. Every change goes through the kernel's audited agent table, recorded before it acts, with the fields that changed, as `Partitions::put` records an area ([`partition.rs:826`](../../adk-kernel/src/partition.rs)). No definition is read from a workspace file, a watched directory or `adk-server`'s agent registry (S-I17).

**Its identity.** The kernel gives each agent an id once, from its first name: `finance`, or `finance-2` when `finance` was used before; a name with no letter or digit gets `agent`, `agent-2`, and so on. The id is its kernel target, its root agent name and its identity, `urn:adk:agent:adk-rust/finance` ([`acting.rs:88`](../../adk-core/src/acting.rs)), so the worker accepts its runs with no extra configuration ([`worker.rs:1134`](../../adk-server/src/background/worker.rs)) and every decision, rule and record names it. Renaming changes only the name on screen. A removed agent's id is never given again (S-I14), because approvals and standing rules bind to it. Reserved: `adk-rust`; any id containing `@` or `.` (`harness.goal`, `adk-rust@work`, `kernel.wake`); `self-improver`; every member name and every target or workflow the host registered.

**How an added agent runs.**

1. At start, the kernel builds every active definition through the agent factory its host registers. `adk-rust up`'s factory builds a harness agent with the composition the primary agent uses — the same sessions, approval inbox, standing-rule store, ledgers, audit trail and kill switch — in the agent's area, on its own model.
2. The kernel hosts it in a live target table that every queue's worker reads, with the agent's progress source attached, so an agent added or changed while the kernel runs serves within one scheduler tick, with no restart, and its spending is counted to its own processes.
3. A change is built first and then swapped in. A build that fails keeps the agent as it was and shows **Can't start** with the reason. A turn in progress finishes on the agent it started on.
4. Every replica reconciles its table with the store on each tick. A replica that claims a run for an agent it has not built yet hands the run back for a later claim instead of failing it.
5. The kernel refuses to host an agent whose kill switch is not its own ([`governance.rs:510`](../../adk-core/src/governance.rs)), so Stop all reaches every added agent (S-I15).
6. An added agent's follow-ups run as itself. A follow-up without a proposing run goes to the area's primary agent, never to an added one.
7. Not in this phase: goals (`adk-rust goal` stays with the primary agent), wakes from webhooks, files or chat (chat reaches the primary agent only), team members, and `propose_agent` on an added agent.

**Threads.** A thread belongs to one agent. **New thread** on an agent's row sends the thread's first message to that agent; the agent's harness stamps the session on its first run, as areas stamp theirs ([`partition.rs:51`](../../adk-harness/src/partition.rs)), and every other agent, the primary included, refuses that session. No surface writes the stamp. A later message goes to the agent the stamp names; before the first run it goes to the agent of the session's latest run, otherwise to the primary agent.

**The roster.** The primary agent comes first, then the person's agents by name, under their area when the kernel has areas. Each row keeps the §5.1 shape — avatar, name, status word — with three new words ([§8.1](#81-terms)): **Ready** for an agent that has not run yet, **Paused**, and **Can't start**. The kernel publishes each change on its event bus, so the terminal redraws within one event and the web home on its next refresh. Selecting an agent opens its latest thread.

**In the web home.** Each screen exists in light and dark, from the §9 tokens.

| Screen | What it shows |
|---|---|
| Roster | **New agent** at the foot of the agents list. With no agents of the person's own: "No agents of your own yet. Add one, or ask ADK-Rust to suggest one." Each row shows **Settings** on hover and on focus |
| New agent (`/ui/agents/new`) | Heading "New agent" with two tabs. **Describe it**: one box, "What should it do?", and **Ask ADK-Rust to suggest one**, which starts a thread with ADK-Rust. **Set it up yourself**: the definition's fields in one column, in the order of the table above, with the control "How much it asks first" that "What your agents can do" uses; "Runs on" lists only providers whose key the kernel has. Beside the fields, **Here is what Finance can do**, kept current as the form changes, with the risk pill. **Add Finance** and **Cancel** at the foot |
| Agent settings, from **Settings** on a roster row or in the thread head | The per-agent settings every agent has (how much it asks first, "Allowed without asking"), and for an added agent: "Added by you on 25 Oct" or "Suggested by ADK-Rust, added by you on 25 Oct"; the definition's fields; **Copy one of your rules**; its routines with **Add a routine**; **Pause** and **Remove** at the foot. Every change shows "Was … Now …" before it is saved, as quiet time does. The primary agent's settings show its mode and rules only; its name, instructions and model stay in `adk-rust up`'s configuration |
| Memory & rules | "What your agents can do" lists every agent with its area, how much it asks first, and **Settings** |

**In the terminal.**

| Input | What it does |
|---|---|
| Ctrl-G or `/agents` | Opens or closes the Agents view on the right, as Ctrl-R does Permissions |
| In the Agents view: ↑ ↓, Enter, `n`, `e`, `p`, `x`, Esc | Select; open its latest thread; new agent (**Describe it** or **Set it up yourself**); settings; pause or resume; remove; back. Decision keys answer only in the thread view ([`app.rs:253`](../../adk-surface/src/tui/app.rs)), so none of these answers a decision |
| **Set it up yourself** | A form in the right pane: one field per line, Tab to the next, Enter shows **Here is what Finance can do**, a second Enter adds it, Esc leaves without saving |
| `/agent` with `list`, `show`, `new`, `edit`, `pause`, `resume` or `remove` | The kernel's agent command: the same code in the REPL, in the full screen, and on a kernel through the control plane's command routes. A change prints its preview and the line that confirms it; the Agents view and its form take both steps for the person |
| Tab, Shift-Tab | Complete a `/` command while one is typed; otherwise the next or previous agent in the roster, agents without a thread included |
| `adk-rust agents` with the same verbs | The same command for scripts: it prints the preview and asks on the terminal; `--yes` is refused at high risk, and with no terminal to ask on, a change is refused |

The footer lists "ctrl-g agents" and `/help` lists `/agents` and `/agent`. Names and instructions pass `terminal_safe` ([`safe.rs:38`](../../adk-surface/src/tui/safe.rs)). In `--ui inline` mode `/agents` lists the agents and changes use `/agent`; `--output json` emits one line per agent change.

**The decision that adds an agent.** A suggestion is one inbox entry with the tool name `kernel:agent.create`, written by the kernel as agenda proposals and driver mounts are ([`agenda.rs:1263`](../../adk-kernel/src/agenda.rs), [`tenant.rs:856`](../../adk-kernel/src/managed/tenant.rs)): the definition as its arguments; a summary and a risk computed by code; the suggesting agent as its requester; Approve and Reject as its only actions; no expiry (S-I3). It follows the §7.1 anatomy:

| Part | Content | Example |
|---|---|---|
| Who and when | The suggesting agent | A ADK-Rust · since 09:12 · *Waiting for you* |
| Heading | Kernel text, never the model's | Add an agent called Finance |
| The definition, in full | Every field above; the instructions as quoted text; instructions longer than a call may be shown ([`project.rs:29`](../../adk-surface/src/project.rs)) must be expanded before the agent can be added | Area Home · Asks first · Runs on `claude-sonnet-5-5` · Reads files in Invoices · No shell · Listed sites only |
| What it can do | Plain words, and a line comparing it with the suggesting agent | It can do less than ADK-Rust: no shell, no web search |
| Allowed without asking | What it would start with | Nothing yet |
| How it got here | A short timeline, naming the sites and files read in that turn | You asked at 09:10 — ADK-Rust read 2 files — ADK-Rust suggested this agent — Waiting for you |
| Actions | The verbs | Add this agent · Change it first · Don't add |
| Details | Risk, reversibility, where else it is shown | Medium risk · You can remove it later · Also on your phone |

Chat carries the summary and the risk with **Don't add** and "Open in home"; push carries a wake-up only (S-I5). **Change it first** opens **Set it up yourself** filled in with the suggestion. When the person saves it, the agent is added by hand and the suggestion ends as "You changed it and added it"; when the person leaves the form, the suggestion keeps waiting. The card never offers **Always allow**, and no standing rule, mode or routine answers it (S-I11). A suggestion is never applied with more than its card showed: the kernel adds the agent only when the definition's digest equals the one approved, and only after checking again that the suggesting agent may still do everything the new one may. If it may not, the suggestion ends refused, and the suggesting thread shows "Finance wasn't added: ADK-Rust can no longer use the web, so Finance can't either."

**Risk** comes from the definition and is computed by code:

| Risk | The agent would have any of |
|---|---|
| Low | Just look or Suggest; no shell; no web, or listed sites only; drivers that only read |
| Medium | Ask first; changing files; the sandboxed shell; any site; drivers that write locally or schedule with others |
| High | Go ahead; a shell on this computer; drivers that send or delete; or it was suggested during work the person did not start at a screen — anything but an interactive turn, or a turn that came from a chat message |
| High, spends money | The payments or computer-use pack; drivers that spend or publish (K-I9) |

**Changing an agent.** A change that makes an agent do less — a stricter mode, fewer tools, a lower limit — applies at its next step (spec 3.1 I14). A change that lets it do more is shown as "Was … Now …" with the risk of the result, needs what adding it at that risk needs, and first ends the agent's waiting decisions, as loosening a mode does ([`harness.rs:536`](../../adk-harness/src/harness.rs)). A change made against an older version is refused and shown as "Finance was changed in the terminal at 10:01", with the agent as it is now. In an added agent's own thread, the mode control and `/mode` can only make it ask more: "Finance asks first at most. Change it in Finance's settings."

**Pausing and removing.**

| Action | What happens | Where |
|---|---|---|
| **Pause** | Its work stops at the next step; its routines and follow-ups skip; a message to it gets "Finance is paused" | Web home, terminal |
| **Resume** | The work its pause stopped continues, and its routines fire again | Web home, paired local terminal (S-I10) |
| **Remove** | Asks first, naming what ends: "Removing Finance stops 1 piece of work, ends 2 decisions waiting for you and 1 routine. Its threads stay." Then its work and routines are cancelled, its waiting decisions show "Ended without an answer · You removed Finance at 10:02" (never a yes or a no), its follow-ups are withdrawn, its "allowed without asking" rules are removed, and its id is retired. The kernel records the removal before it starts; if it is interrupted, the next tick on any replica finishes it | Web home, terminal |

**Stop all** stops added agents like any other; **Resume all** never resumes an agent paused by hand (K-I21). Pausing and removing work while everything is stopped, because they only make agents do less. Adding and changing work too, and nothing the agent does starts until Resume all.

**What an agent may never do.**

1. Add, change, pause or remove an agent, or answer a decision about one ([§7.4](#74-rules-on-every-surface) rule 6).
2. Suggest an agent that can do more than itself: a looser mode than its own or its area's; tools, packs, sites, folders or drivers it lacks; another area; a shell on this computer; payments; computer use; members; or `propose_agent`.
3. Suggest a change to any agent: changes are the person's.
4. Give an agent anything allowed without asking (spec 3.1 I2).
5. Suggest an agent from the consolidation tick or the self-improver (K-I12, spec 3.4 SI-I11).

An added agent has no `propose_agent`: suggestions are one level deep, as delegation is for members (spec 3.1 I35).

**Stated limit.** The kernel's socket and pipe admit any process of the person's account as the person (spec 3.2 §7.11), as they do when a high-risk call is decided on the local terminal ([§7.2](#72-what-each-surface-may-decide)). A shell on this computer that a person let an agent run can therefore reach them, and so can a process that writes the kernel's store files directly; the sandboxed shell cannot reach the socket. Every change records the transport it came through and, where the operating system reports it, the calling process ([§13](#13-open-decisions) decision 22).

## 6. Architecture

| Layer | What it is | On main |
|---|---|---|
| **Kernel** | Processes running harness agents on any model; the approval inbox; the event bus; agenda, wakes and quiet time; memory; the activity ledger and the record | `adk-kernel`, `adk-harness`, `adk-core` |
| **Engine** (private module of `adk-surface`) | **Feed**: per-session events with a sequence number, a snapshot then a live tail, reconnect with `Last-Event-ID`. **Projection**: events become messages, decision cards, agent rows, status and briefing lines once, in Rust. **Commands**: decide, send, start a thread with an agent, queue, steer, stop, change mode, and add, change, pause and remove an agent — each stamped with who and how (keyboard, touch, voice, chat, agent) | New |
| **Surfaces** | One feature each; they hold no state a reconnect cannot rebuild | New |

The terminal changes agents through the kernel's `/agent` command, which `adk-rust up` serves with the REPL's other commands over the control plane; every change, from either surface, goes through the kernel's agent table, which holds the rules of [§5.7](#57-your-agents).

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
| `genui-apps` | Sub-feature of `genui`: the MCP Apps host, apps framed sandboxed on their own origin; implies `web` (wave 4 decision 16) | `adk-harness` `mcp`, `adk-tool` `mcp-apps` (rmcp) | Beta |
| `workflow` | Graph run observer and debugger | `adk-graph` with `time-travel`; `adk-harness` `web` (secret redaction) | Beta |
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

| Surface | Allow once | Always allow | Edit | Deny | High risk | Add an agent, or let one do more |
|---|---|---|---|---|---|---|
| Web home | Yes | Yes | Yes | Yes | Yes, after confirming it is you | Yes; high risk after confirming it is you |
| Terminal | Yes | Yes | Yes | Yes | Yes, on a paired local terminal | Yes; high risk on a paired local terminal |
| Phone | Yes | Yes | Yes | Yes | Yes, after confirming it is you | Adds a suggested agent; high risk after confirming it is you. Setting one up or changing it opens the home |
| Desktop tray | Yes | No (opens the home) | No | Yes | No (opens the home) | No (opens the home) |
| Chat | Yes, low and medium risk | No | No | Yes | No (links to the home) | No (links to the home) |
| Voice | Yes, low and medium risk, with a cancel window | No | No | Yes | No | No |

Adding an agent follows the "Always allow" column, because it is a standing change, as loosening a mode is ([§5.7](#57-your-agents)). **Don't add** is the Deny column: every surface can refuse a suggested agent. The chat gateway, which draws its own cards, offers **Don't add** only on a suggested agent.

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
| An agent the person added that has not run yet | **Ready** | an active definition with no process |
| An agent paused by hand | **Paused** | a paused definition; its processes suspended |
| An agent the kernel cannot build | **Can't start** — "No key for Anthropic" | the agent factory's error |
| An agent another agent suggested | **Suggested by ADK-Rust** | a suggested definition |
| A decision that ended because its agent was removed | **Ended without an answer** — "You removed Finance at 10:02" | a cancelled inbox request (`DecidedAnswer::Ended`) |
| The person's own agents | **Your agents** | definitions in `AgentStore` |

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
| Start adding an agent | **New agent** | web, terminal |
| Ask an agent to suggest one | **Describe it**, then **Ask ADK-Rust to suggest one** | web, terminal |
| Fill in the definition | **Set it up yourself**, then **Add Finance** | web, terminal |
| Approve a suggested agent | **Add this agent** (high risk: **Confirm it's you and add**) | web, terminal, phone |
| Open the form with the suggestion | **Change it first** | web, terminal |
| Refuse a suggested agent | **Don't add** | every surface |
| Change an agent | **Settings**, then **Save changes** | web, terminal |
| Give an agent one of your rules | **Copy one of your rules** | web home |
| Stop or restart one agent | **Pause** / **Resume** | Pause: web home, terminal; Resume: web home, paired local terminal |
| Remove an agent | **Remove**, then **Remove** or **Keep** | web, terminal |

**Always allow for this folder** makes one standing rule for every file write in the folder the call writes in, for the session: `write_file`, `edit_file` and an `apply_patch` that only adds or updates files, each pinned to its exact declaration (`OriginPin::ExactEach`, `RulePredicate::WritesWithin`, spec 3.1 §7.5). A write outside the folder, a `..` or symlink escape, a delete, a move, the shell and every other tool still ask; a call no folder rule covers is offered **Allow once** and **Deny** only.

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
| K7 | adk-kernel | Agents a person adds: `AgentRecord` and `AgentStore` on memory, SQLite, PostgreSQL, Redis, MongoDB, Firestore and Neo4j with a conformance suite; the audited agent table (`kernel.agent.*`); `KernelBuilder::agent_store`, `agent_factory` and `agents_handle`; an `AgentFactory` that assesses a definition, reports a suggester's limits and the models it can build, and builds a runner with its progress source; a live target table shared with every worker and reconciled with the store each tick, with progress routed by target before area; suggestions as `kernel:agent.create` inbox entries, applied on a tick only after a person approves the same digest; read routes `GET /kernel/agents` and `GET /kernel/agents/{id}` with `ControlClient` methods; `KernelEvent::Agent`; `ProcessFilter::target`; spawn refuses a paused or removed agent; a follow-up without a proposing run goes to the area's primary agent; the chat card of `kernel:agent.create` offers Deny only | Your agents ([§5.7](#57-your-agents)) |
| S4 | adk-server | `DurableWorker` reads a shared target table, so a target hosted after start runs on every queue; a run for a target the table expects but has not built yet is handed back for a later claim, not failed ([`worker.rs:1079`](../../adk-server/src/background/worker.rs)) | Your agents |
| H3 | adk-harness | `propose_agent` (class `Todo`, only on the primary agent) with the suggesting agent's own limits as its ceiling; a harness built from an `AgentRecord`; the session stamp `harness:agent` on every harness the kernel hosts; an added agent applies only standing rules pinned to its own id, and "Always allow" on its cards pins the rule to it; `assess_agent` (risk and plain-words abilities) | Your agents |
| L1 | adk-cli | `adk-rust up` opens the `AgentStore` beside the kernel's other stores and registers an agent factory over the composition the primary agent uses ([`harness_cli.rs:375`](../../adk-cli/src/harness_cli.rs)), each agent's model built from the providers whose keys it has; the `/agent` command in the REPL's command set, which the full-screen terminal runs and `up` serves over `GET /api/kernel/commands` and `POST /api/kernel/commands/run`; `adk-rust agents` | Your agents |

The control plane has no route that changes an agent: changes come through the engine in `up`'s process and through the `/agent` command. The read routes stay under `/api/kernel/agents`, because `adk-server`'s agent registry already serves `/api/agents` ([`registry/routes.rs:10`](../../adk-server/src/registry/routes.rs)).

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
| S-I11 | An agent is added, or made to do more, only by a person: by hand in the web home or the terminal, or by approving a suggestion in the web home, on the phone or in the terminal. The kernel applies a suggestion only after a person approves the definition digest it showed. No mode, standing rule, routine, chat, voice, tray or agent adds one, and none of these decisions is ever "Always allow" |
| S-I12 | A suggested agent never exceeds the agent that suggested it or its area: its mode, tools, packs, sites, folders, drivers, shell and spending are each at most the suggester's; it never has a shell on this computer, payments, computer use, members, another area or `propose_agent`. Code checks this when the suggestion is made and again, against the suggester's limits then, when it is applied |
| S-I13 | A card for an agent is drawn from the inbox entry the kernel wrote: the heading and abilities are kernel text, the definition is shown in full and escaped, and long instructions cannot be approved until expanded |
| S-I14 | An agent id is given once: a removed agent's id is never given again, so its approvals, rules, held calls and record never apply to another agent. Names are unique among the kernel's agents and never a reserved name |
| S-I15 | Every added agent runs under the kernel's kill switch: the kernel refuses to host a runner whose `GovernanceControl` is not its own; Stop all stops added agents; Resume all never resumes an agent paused by hand |
| S-I16 | Removing an agent ends its work, routines, follow-ups and rules, and ends its waiting decisions without an answer, never as a yes or a no; its threads and its record stay. An interrupted removal is finished by the next tick |
| S-I17 | Agent definitions come only from the kernel's store through its agent table and `propose_agent`, never from workspace files, a watched directory or `adk-server`'s agent registry |
| S-I18 | Every change to an agent is recorded before it acts (K-I16): who, from which surface and how, through which transport, the fields before and after, the risk, and for a suggestion the suggesting agent and session |
| S-I19 | An added agent applies only standing rules whose agent is exactly its own id; a rule made before it existed applies to it only when a person copies the rule to it |

Every answer, pairing, notification, stop and change to an agent is written to the signed audit log with the surface and how it was given (keyboard, touch, voice, chat), so "What your agents did" can say "You let Finance send one email, from Telegram".

Records of agents a person adds ([§5.7](#57-your-agents)):

| Record | Fields |
|---|---|
| `kernel.agent.proposed` | id, digest, risk, area, suggesting agent, session, run |
| `kernel.agent.created` | id, version, digest, risk, origin (by hand or suggested), approval id, by, surface, how, device, presence, transport |
| `kernel.agent.changed` | id, fields, from, to, risk, by, surface, how, transport |
| `kernel.agent.paused`, `kernel.agent.resumed` | id, by, surface, transport |
| `kernel.agent.removing`, `kernel.agent.removed` | id, by, surface, transport; what ended (processes, decisions, follow-ups, rules) |
| `kernel.agent.refused` | id or name, reason: over the ceiling, a reserved or taken name, an approver who is not a person, a digest that changed |

## 12. Phased delivery

Each phase ships only when its scenario passes end to end on SQLite and on one shared store (PostgreSQL or Redis).

| Phase | Ships | Scenario and pass condition |
|---|---|---|
| **1. Decisions reach you** | Engine; `web` (threads, decisions, stop); `tui` (full-screen, inline, JSON); `mobile` push and pairing; one chat adapter; K1, K2, H1, C1, C2, S1–S3 | **Approve it tomorrow.** Quiet time 22:00–07:00. At 18:00 a routine prepares to email an invoice and waits for a decision. Nobody answers; the kernel is killed with `kill -9` and started again. At 07:30 the person taps Allow once in the chat thread. *Pass:* nothing runs before the tap and nothing notifies during quiet time; the email is sent exactly once; the phone's card shows "You said yes in Telegram"; the terminal shows the same; the record names the channel. **A stranger writes:** an unpaired account gets a pairing code and starts nothing |
| **2. While you were away** | Today; morning briefing when quiet time ends; routines; K3, K4 | **The morning after.** Over one night two routines finish, one fails, an agent suggests a payment, the person allows a call from the phone, and the kernel restarts. *Pass:* one briefing at 07:00; its counts and spending match the ledger exactly across the restart; every line links to its record |
| **3. Memory and rules** | Memory & rules; questions from `/proc`; permissions with end dates; the record check; K5, H2 | **Correct me, then check.** An overnight tidy-up merges two notes. *Pass:* Undo restores both; a confirmed guess becomes "You said this"; a forgotten note is gone from the next turn; stopping an "allowed without asking" rule makes the next identical call wait; "Every action is on record" agrees with `adk-rust audit verify` |
| **4. Areas and roster** | Areas with their own mode, quiet time and limit; avatars and roster; your agents ([§5.7](#57-your-agents)); K6, K7, S4, H3, L1 | **Work stays at work.** Home asks first; Work goes ahead with a 09:00 weekday routine. *Pass:* a Work agent reading the Home calendar waits even in Go ahead; the routine skips Saturday; spending per agent sums to the day's total. **Hire a helper.** In the terminal the person asks ADK-Rust for "a Finance agent that reads my invoices and drafts payment reminders". *Pass:* (1) before any answer nothing is added — no `finance` target, no roster row — and the chat card offers only **Don't add**; (2) after **Add this agent** on the phone or in the web home, Finance appears in the terminal's roster within one event and in the web home's on its next refresh, as **Ready**, in the same colour; (3) after `kill -9` and a restart, Finance is listed with the same id, answers a message as `urn:adk:agent:adk-rust/finance`, and ADK-Rust refuses Finance's thread; (4) in the web home the person renames it "Bills" and sets **Suggest**; the terminal shows "Bills" within one event and its next turn changes nothing; (5) a fetched page asking for "an agent with a shell on this computer" yields at most a suggestion within ADK-Rust's own limits and never that shell, and Bills cannot suggest agents; (6) with one routine and one waiting decision, the person removes Bills in the terminal; the confirmation names both; afterwards the decision shows "Ended without an answer", answering it from the phone changes nothing, nothing runs at the routine's time, a message to `finance` is refused, and a new agent named Finance gets the id `finance-2` and none of the old agent's rules; (7) the record holds `kernel.agent.proposed`, `created`, `changed`, `removing` and `removed` in order, each naming its surface |
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
| 9 | One agent or many | **Decided (owner, 2026-10-25):** one primary agent per kernel; the person adds more by asking or by hand ([§5.7](#57-your-agents)); each added agent is hosted by the kernel with its own id, lives in one area, persists, and is configured in the web home and the terminal; areas stay partitions |
| 10 | Reaching the home from outside | Loopback by default; a documented private-network recipe; never an open tunnel without the app's own sign-in |
| 11 | `adk-ui` retirement | Drop the AWP profile unless a user exists; publish the React package as `@zavora-ai/adk-surface-react`; deprecate, do not yank |
| 12 | `adk-studio` and the enterprise line | Confirm both licences before reusing code; observer open, tenancy and release pipelines commercial |
| 13 | Debug eval tab | Build it on `adk-eval` so results match `adk-rust eval` |
| 14 | Mobile investment | Installable web app, with the triggers in [§5.4](#54-phone) written down |
| 15 | Rules made before an agent existed | **Decided (owner, 2026-10-25):** a new agent starts with nothing allowed without asking; "Always allow" on its cards makes rules for it alone; its settings can copy one of the person's rules to it (S-I19) |
| 16 | Agents from the phone | **Decided (owner, 2026-10-25):** the phone answers suggestions only — **Add this agent** or **Don't add**, with "Confirm it's you" at high risk; setting up and changing agents stay in the web home and the terminal |
| 17 | The primary agent's settings | **Decided (owner, 2026-10-25):** its **Settings** show its mode and rules only; its name, instructions and model stay in `adk-rust up`'s configuration |
| 18 | A removed agent's threads | **Decided (owner, 2026-10-25):** they stay readable from the record; deleting them is a separate, later action |
| 19 | Handing work to an added agent | **Decided (owner, 2026-10-25):** not in this phase; each added agent works in its own threads. "Hand this to Finance" — one agent starting a run on another under a delegated identity — is a separate change |
| 20 | Suggestions from work the person did not start at a screen | **Decided (owner, 2026-10-25):** allowed, always at high risk, so adding needs "Confirm it's you" or a paired local terminal |
| 21 | Another provider or model for an added agent | **Decided (owner, 2026-10-25):** any provider whose key the kernel already has; keys are never stored in the definition; the default is the primary agent's model |
| 22 | Agent changes on the kernel's own terminal | The kernel's socket admits any process of the person's account as the person ([§5.7](#57-your-agents), stated limit). Keep the §7.2 terminal rule — the paired local terminal is the person — as for high-risk decisions; record the transport and calling process of every change; test that the sandboxed shell cannot reach the socket. The alternative is "Confirm it's you" for every change that adds an agent or lets one do more, the local terminal included |
| 23 | A suggestion made after reading untrusted text | Keep its risk as the definition gives it: "How it got here" names the sites, messages and files read in that turn, and the suggestion stays within the suggester's limits. Suggestions from chat-started or unattended work stay high (decision 20) |

**Decided (owner, 2026-10-23 to 2026-10-25):** `adk-surface` is the agent UI, opt-in, one feature per surface; every surface drives the same kernel; the terminal ships in light and dark, full-screen by default; plain words and sentence case on every person-facing surface; decisions have their own background and a count badge; every screen exists in light and dark. Agents are added by asking or by hand, persist, appear in the roster on the left, and are configured in the web home and the terminal (owner, 2026-10-25).

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

Grounding of [§5.7](#57-your-agents) and its rows, on `origin/main` @ `c991dae3`:

| Area | Reference |
|---|---|
| Targets fixed at build | [`adk-kernel/src/kernel.rs:426, 985, 996, 1562, 1757, 1955`](../../adk-kernel/src/kernel.rs) |
| Worker target lookup and refusal | [`adk-server/src/background/worker.rs:367, 553, 1051-1085, 1134`](../../adk-server/src/background/worker.rs) |
| `adk-rust up` registrations and stores | [`adk-cli/src/harness_cli/up.rs:39, 314-328, 639, 698`](../../adk-cli/src/harness_cli/up.rs), [`adk-harness/src/kernel.rs:245`](../../adk-harness/src/kernel.rs) |
| Progress and ledger attached at start, routed by area | [`adk-kernel/src/process.rs:553, 586`](../../adk-kernel/src/process.rs), [`adk-kernel/src/supervise.rs:201`](../../adk-kernel/src/supervise.rs) |
| Store and audited-table templates | [`adk-kernel/src/partition.rs:612, 826`](../../adk-kernel/src/partition.rs), [`adk-kernel/src/drivers/store.rs:279`](../../adk-kernel/src/drivers/store.rs), [`adk-kernel/src/audit.rs:95`](../../adk-kernel/src/audit.rs) |
| Proposals in the inbox, applied after a person | [`adk-kernel/src/agenda.rs:1263, 1342, 1704`](../../adk-kernel/src/agenda.rs), [`adk-kernel/src/managed/tenant.rs:238, 856`](../../adk-kernel/src/managed/tenant.rs) |
| Follow-up target without a proposing run | [`adk-kernel/src/agenda.rs:1439`](../../adk-kernel/src/agenda.rs) |
| Chat cards draw their own answers | [`adk-kernel/src/channels/mod.rs:273`](../../adk-kernel/src/channels/mod.rs) |
| The person, no device | [`adk-kernel/src/control/settings.rs:205`](../../adk-kernel/src/control/settings.rs) (`require_owner`), [`adk-kernel/src/control/routes.rs:289`](../../adk-kernel/src/control/routes.rs) (caller and transport) |
| Roster, names and avatars | [`adk-surface/src/local.rs:483, 557`](../../adk-surface/src/local.rs), [`adk-surface/src/engine.rs:287`](../../adk-surface/src/engine.rs), [`adk-surface/src/model.rs:44, 60`](../../adk-surface/src/model.rs), [`adk-surface/src/project.rs:763`](../../adk-surface/src/project.rs) |
| Routines are scheduled processes | [`adk-surface/web/src/client.ts:298`](../../adk-surface/web/src/client.ts) |
| Who may loosen and resume | [`adk-surface/src/rules.rs:139, 366, 387`](../../adk-surface/src/rules.rs) |
| Decision keys only in the thread | [`adk-surface/src/tui/app.rs:253`](../../adk-surface/src/tui/app.rs) |
| Ended without an answer | [`adk-surface/src/model.rs:203`](../../adk-surface/src/model.rs), [`adk-surface/src/words.rs:289`](../../adk-surface/src/words.rs) |
| Identity and rules bound to an agent id | [`adk-core/src/acting.rs:88`](../../adk-core/src/acting.rs), [`adk-harness/src/approvals/rule.rs:368`](../../adk-harness/src/approvals/rule.rs), [`adk-harness/src/approvals/matcher.rs:223`](../../adk-harness/src/approvals/matcher.rs) |
| Session stamp precedent | [`adk-harness/src/partition.rs:51, 267`](../../adk-harness/src/partition.rs) |
| One kill switch | [`adk-core/src/governance.rs:510`](../../adk-core/src/governance.rs) |
| A host shell in Go ahead | [`adk-harness/src/policy.rs:707`](../../adk-harness/src/policy.rs) |
| `/api/agents` already taken | [`adk-server/src/registry/routes.rs:10`](../../adk-server/src/registry/routes.rs) |
