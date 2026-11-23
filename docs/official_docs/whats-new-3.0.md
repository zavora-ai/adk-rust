# What's New in 3.0

ADK-Rust 3.0.0 builds agents that work on their own: an agent pursues a goal over many steps,
wakes itself, and keeps working while nobody watches, inside limits you set. This page lists
what 3.0 adds, by area, with the feature that turns it on and the guide that covers it.

> **Note:** 3.0.0 is a major release. Upgrading from 2.x starts with
> [Migrating 2.x → 3.0](migration/2.x-to-3.0.md); the [changelog](../../CHANGELOG.md) has an
> entry for every change.

| Area | Crates | Tier | Start with |
|------|--------|------|------------|
| [Autonomous agents](#autonomous-agents) | `adk-harness`, `adk-kernel`, `adk-server` | `enterprise` (`autonomy`) | [Build an autonomous agent](autonomy/index.md) |
| [One governed tool path](#one-governed-tool-path) | `adk-core`, `adk-runner`, `adk-auth`, `adk-sandbox` | every tier; the OS sandbox and the signed audit log in `enterprise` (`autonomy`) | [Tool authorization](security/tool-authorization.md) |
| [Durable runtime](#durable-runtime) | `adk-core`, `adk-session`, `adk-server` | `adk-core` stores in every tier; background runs, cron and webhook wake-ups in `enterprise` (`autonomy`) | [Durable runtime](core/durable-runtime.md) |
| [The harness](#the-harness) | `adk-harness` (Beta) | `enterprise` | [The harness](harness/index.md) |
| [The agent kernel](#the-agent-kernel) | `adk-kernel` (Beta) | `enterprise` | [The agent kernel](kernel/index.md) |
| [Agent surfaces](#agent-surfaces) | `adk-surface` (Beta) | `surface`, in no tier | [Surfaces](surfaces/index.md) |
| [AWP 2.0 and websites](#awp-20-and-websites) | `awp-types`, `adk-awp` (Beta); `adk-site`, `adk-drivers`, `adk-website` (Experimental) | `enterprise` (`awp`); `full` (`website`) | [Spin up an AWP website](deployment/website-quickstart.md) |
| [Local-first vision](#local-first-vision) | `adk-vision` (Experimental) | `full` (`vision`) | [Vision: getting started](vision/getting-started.md) |
| [Models and protocols](#models-and-protocols) | `adk-model`, `adk-server`, `adk-tool`, `adk-acp` | per provider feature | [Model providers](models/providers.md) |
| [Secure by default](#secure-by-default) | `adk-server`, `adk-browser`, `adk-devtools`, `adk-core` | every tier | [Migrating 2.x → 3.0](migration/2.x-to-3.0.md) |

## Autonomous Agents

An autonomous agent works toward a goal, checks its own work, wakes on a schedule or an event,
waits for a person when a call needs approval, stays inside its budget, and resumes after a
crash. The `autonomy` feature turns on everything it needs, and the `autonomous` template
scaffolds one.

| What you get | Where |
|--------------|-------|
| Goals that iterate until a check command passes, a grader passes every criterion of a rubric, or both, and park on a decision | [Plans and goals](harness/plan-and-goals.md#goals) |
| Wake-ups on a schedule, a file change, a signed webhook, a queue message, an approval or a finished run | [Wake-ups](core/wake-ups.md), [Scheduling](kernel/scheduling.md) |
| The harness and its kernel, durable and background runs, cron and webhook wake-ups, the approvals service, SQLite stores and the signed audit log, in one feature | `autonomy` (in `enterprise` and `full`) |
| A project with a rubric-graded goal, a weekday schedule, an approval-gated side effect, a run budget and the OS sandbox | `cargo adk new ops-agent --template autonomous` |

## One Governed Tool Path

Every tool call from every agent type — `LlmAgent`, `CodeActAgent`, the realtime runners and
the harness — passes one function, `adk_core::authorize_tool_call`, before it runs.

| Mechanism | What it does | Guide |
|-----------|--------------|-------|
| Tool policies | Allow, deny or require approval per call; `DeclarativePolicy` denies any tool no rule permits | [Tool authorization](security/tool-authorization.md) |
| Held calls | A call that needs approval parks the run; the decision binds to the call's fingerprint (acting agent, tool origin, tool name, canonical arguments), and a later run resumes it | [Resuming a held call](security/tool-authorization.md#resuming-a-held-call) |
| Run budgets and spend | `RunBudget` caps model calls, tokens, cost, wall time and tool calls per run; `SpendLedger` meters spend across runs | [Run budgets](core/runner.md#run-budgets), [Spend ledger](security/spend-ledger.md) |
| Kill switch | `GovernanceControl` freezes model and tool calls, on every replica that shares its governance store | [Tool authorization](security/tool-authorization.md) |
| Agent identity | Every run carries an acting agent (`ActingContext`) | [Agent identity](security/agent-identity.md) |
| OS sandboxes | Seatbelt on macOS, bubblewrap on Linux, a restricted token on Windows | [Shell and sandbox](harness/shell-and-sandbox.md) |
| Signed audit log | A tamper-evident record of each decision and call, checked with `adk-rust audit verify` | [Audit log](security/audit-log.md) |

## Durable Runtime

A durable run is a unit of work a worker claims under a lease. When the process dies, the lease
expires and the next claim resumes the run from its checkpoint.

| What you get | Where |
|--------------|-------|
| Leases with fencing (`LeaseStore`), the run queue (`DurableRunStore`) and schedules (`ScheduleStore`) | [Durable runtime](core/durable-runtime.md) |
| Background runs, cron schedules and signed webhook wake-ups that enqueue durable runs | [Wake-ups](core/wake-ups.md) |
| An action ledger (`ActionLedger`) that keeps a non-idempotent call from running twice | [Durable runtime](core/durable-runtime.md) |
| Run, lease, schedule, governance, approval, spend, action and audit stores on SQLite, PostgreSQL, Redis, MongoDB, Firestore and Neo4j | [Durable stores](security/durable-stores.md) |

## The Harness

`adk-harness` is a governed agent over `LlmAgent`: modes, standing approvals, a host or
OS-sandboxed shell, todos, `AGENTS.md` and skills, team members, plan graphs and durable goals.
It replaces `adk_agent::coding::CodingAgent` for new work.

| What you get | Where |
|--------------|-------|
| The harness at a terminal, served over HTTP, or embedded | [The harness](harness/index.md), [In your terminal](harness/cli.md) |
| Packs under the harness policy: browser, computer use, payments, voice, web, MCP and hooks | [Packs](harness/packs.md), [Web and MCP](harness/web-and-mcp.md), [Hooks](harness/hooks.md) |
| A project with approvals, `AGENTS.md` and a skill | `cargo adk new my-agent --template harness` |
| Moving from `CodingAgent` | [Migrating from `CodingAgent` to the harness](migration/coding-agent-to-harness.md) |

## The Agent Kernel

`adk-kernel` runs the harness's work as processes that sleep until a wake source fires, with an
event bus, a control plane and a kill switch across replicas.

| What you get | Where |
|--------------|-------|
| `adk-rust up`, `ps`, `top`, `attach`, `freeze` and `events` | [Kernel CLI](kernel/cli.md) |
| Drivers, partitions, `/proc`, and running as a service | [Drivers](kernel/drivers.md), [Partitions](kernel/partitions.md), [`/proc`](kernel/proc.md), [Service](kernel/service.md) |
| Stores in memory and on SQLite, PostgreSQL, Redis, MongoDB, Firestore and Neo4j | [Backends](kernel/backends.md) |

## Agent Surfaces

`adk-surface` turns sessions, decisions and activity into plain words for every surface, so the
web home, the terminal, a phone and chat attach to the same running state.

| What you get | Where |
|--------------|-------|
| The web home at `/ui/`: Threads, Today, Memory & rules, and a developer panel | [Web home](surfaces/web-home.md) |
| The terminal, a phone, chat channels and voice | [Terminal](surfaces/terminal.md), [Phone](surfaces/phone.md), [Chat](surfaces/chat.md), [Voice](surfaces/voice.md) |
| The workflow observer, which replaces ADK Studio | [Workflow observer](surfaces/workflow.md) |
| Agent-drawn forms and tables (`genui`), which replace `adk-ui` 2 | [Agent-drawn UI](surfaces/genui.md) |
| A message sent to a running turn from code (`Runner::steer`), a surface or the control plane | [Steering a running turn](surfaces/index.md#steering-a-running-turn) |

## AWP 2.0 and Websites

AWP 2.0 gives a website a signed contract and signed releases, and an agent edge beside the
static site. `adk-website` runs the site with a team of agents inside its owner's charter.

| What you get | Where |
|--------------|-------|
| AWP 2.0 routes, signing, the verifier and the conformance kit | [AWP](deployment/awp.md) |
| A site from nothing: `adk-rust website new`, or `cargo adk new my-site --template website` | [Spin up an AWP website](deployment/website-quickstart.md) |
| `adk-rust site` and `adk-rust awp` for releases, domains and keys | [Site CLI](deployment/site-cli.md) |
| Moving a 1.x site | [Migrating from AWP 1.x to 2.0](migration/awp-1-to-2.md) |

## Local-First Vision

`adk-vision` detects, segments and reads on the device, and sends an image to a cloud model only
through an egress gate. 3.0.0 ships phases A to C of [spec 3.7](../design/spec-3.7-vision.md);
`examples/vision` runs their scenarios, S1–S19, with no API key
([The vision scenarios](vision/scenarios.md)).

### Phase A: the Document Desk

| What you get | Where |
|--------------|-------|
| Regions in source pixels, model manifests pinned by revision and SHA-256, and the egress gate | [Vision](vision/index.md) |
| A Document desk: on-device readers and a cloud describer behind the gate | `cargo adk new claims-desk --template vision`, [Getting started](vision/getting-started.md) |
| Governed vision tools and the image privacy guardrail | [Governed tools](vision/governed-tools.md), [Image privacy](security/image-privacy.md) |
| Vision fields in evaluation cases (`Turn::expected_vision`) | [Vision evals](vision/evals.md) |
| Approve with the image that would leave, and an annotated viewer of what an agent saw | [Approve with image](surfaces/approve-with-image.md), [Annotated viewer](surfaces/annotated-viewer.md) |

### Phase B: Video, Streams and the Inspector

| What you get | Where |
|--------------|-------|
| Frames from RTSP and local cameras, video files, screens, browser pages and the web home's camera, through one pipeline with the frame governor and a privacy step | [Streams](vision/streams.md) |
| Anonymous tracks across frames, questions about a clip — Gemini native video or sampled frames on any model — and `sam-3.1` video | [Video and tracking](vision/video.md) |
| Watch conditions that wake a sleeping agent once per tracked object | [Visual wakes](vision/wake.md) |
| Lengths and areas in millimetres from a fiducial's calibration, and the `vision_measure` tool | [Calibration and measurement](vision/measure.md) |
| The live camera surface on the web home and the phone: frames with what was found, tracks, zones, the frame governor's state and the source switch | [Live camera](surfaces/live-camera.md) |

### Phase C: Teams and the Screen Copilot

| What you get | Where |
|--------------|-------|
| Perceiver, reasoner and verifier teams; a disagreement reaches a person with both opinions and the image on one card | [Teams](vision/teams.md) |
| Visual memory in any `adk-rag` vector store, found by a text question or an example image, and erased by source | [Visual memory](vision/memory.md) |
| The video analyst: chunks captioned and indexed, and answers that cite chunks by time range | [The video analyst](vision/video.md#the-video-analyst) |
| Screen grounding: numbered marks in a browser or on a desktop, clicks in logical points through `ScreenSpace`, and Claude's computer-use toolset | [Screen grounding](vision/screen.md) |
| Faces blurred on streams before any write, and the on-device profile, which keeps every image on the machine | [Faces on streams](security/image-privacy.md#faces-on-streams), [The on-device profile](security/image-privacy.md#the-on-device-profile) |
| A vision model on Ollama as a second local describer | [On Ollama](vision/local-describer.md#on-ollama) |
| SAM 3 on the device under the SAM License: the backends, with weights from Meta's access request | [SAM 3](vision/onnx.md#sam-3-under-the-sam-license) |

`vision` (in `full`) is pure Rust and includes streams, tracking and Gemini video;
`vision-local` (in no tier) adds the on-device backends on ONNX Runtime and Candle.
Teams (`team`), visual memory (`memory`), the video analyst (`analyst`) and `sam3-local` are
features of `adk-vision` itself, and the live camera surface is `adk-surface`'s `vision-camera`.

## Models and Protocols

| What you get | Where |
|--------------|-------|
| Meta Muse: `muse` on `adk-rust`; `muse-realtime` and `muse-responses` on `adk-model` | [Meta Muse](models/providers.md#meta-muse) |
| `FallbackLlm`: an ordered chain of models across providers | [Model fallback](models/fallback.md) |
| Per-call reasoning effort (`GenerateContentConfig::effort`) | [Per-call reasoning effort](models/providers.md#per-call-reasoning-effort) |
| Tool declarations kept in declaration order (`LlmRequest.tools` is a `ToolDeclarations`) | [Migrating 2.x → 3.0](migration/2.x-to-3.0.md) |
| A2A v1.0.0 with `A2A-Version` negotiation | [A2A](deployment/a2a.md) |
| An agent served over MCP | [Serve an agent over MCP](mcp/agent-server.md) |
| `agent-client-protocol` 5 for ACP | [ACP](acp/index.md) |

## Secure by Default

| Default | Change it with |
|---------|----------------|
| The server binds `127.0.0.1` through the launchers and the `cargo adk` scaffolds | `serve --host` or `Launcher::with_bind_address`; `BIND_HOST` in a scaffold — after configuring authentication |
| Unlisted hosts and cross-origin requests are refused | `SecurityConfig::allowed_hosts`, `SecurityConfig::allowed_origins` |
| Browser navigation refuses private network addresses | `BrowserToolset::with_private_network_access(true)` |
| The devtools `bash` tool is off | `Workspace::allow_bash(true)` |
| `#[tool]` tools, and `FunctionTool` and `StatefulTool` with a schema, check their arguments; a mismatch is `tool.invalid_arguments` | `#[tool(lenient_args)]`, `with_lenient_args(true)` |

## Platform

| Change | Where |
|--------|-------|
| Rust 1.95 or later | [Quickstart](quickstart.md) |
| SQLx 0.9 for every SQL backend | [SQLx 0.9 in ADK-Rust 3.0](migration/sqlx-0.9.md) |
| reqwest 0.13 | [reqwest 0.13](migration/2.x-to-3.0.md#reqwest-013) |
| The 19 umbrella features that only aliased a sibling are removed | [Migrating 2.x → 3.0](migration/2.x-to-3.0.md) |

## Not in 3.0.0

| Design | Status |
|--------|--------|
| The self-improving harness's maintenance loop ([spec 3.4](../design/spec-3.4-self-improving-harness.md)) | Designed. 3.0.0 ships its protected-path set and the instruction-trust check only |
| Vision phase D: spatial perception — depth, 3D regions and pose, ROS 2 and VLA bridges ([spec 3.7](../design/spec-3.7-vision.md)) | Planned; the [phase D research spike](../design/spec-3.7-phase-d.md) holds its recommendations. 3.0.0 ships phases A to C |

---

**Next**: [Quickstart](quickstart.md), then [Build an autonomous agent](autonomy/index.md)
