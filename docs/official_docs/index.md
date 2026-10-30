# ADK-Rust Official Documentation

Welcome to the official documentation for ADK-Rust (Agent Development Kit for Rust). This documentation provides comprehensive guides and references for building AI agents using the Rust implementation of the ADK framework.

## Getting Started

- [Introduction](introduction.md) - Overview of ADK-Rust, its architecture, and key concepts
- [Quickstart](quickstart.md) - Scaffold, run, and test your first agent in the embedded UI in under 5 minutes
- [A2UI Quickstart](quickstart-a2ui.md) - Give an agent the `genui` render tools, see its drawings in the web home, and read them as A2UI v0.9.1 JSON lines
- [Migrating 2.x → 3.0](migration/2.x-to-3.0.md) - Every breaking change since 2.3.0, with before/after code
- [Migrating 1.0 → 2.0](migration/1.0-to-2.0.md) - Complete breaking-change list with before/after code
- [SQLx 0.9 in ADK-Rust 3.0](migration/sqlx-0.9.md) - Sharing SQLx pools, TLS flags, and migration-runner inputs
- [Migrating from `CodingAgent` to the harness](migration/coding-agent-to-harness.md) - `adk_agent::coding` to `adk_harness::coding`, approvals in unattended runs, todos, nesting
- [Migrating from AWP 1.x to 2.0](migration/awp-1-to-2.md) - What changes, `adk-rust awp migrate-config business.toml`, and the AWP 1.x reference

## Core

- [Core Types](core/core.md) - Fundamental types: Content, Part, Agent trait, Tool trait, contexts
- [Runner](core/runner.md) - Agent execution runtime and configuration
- [Plugins](core/plugins.md) - Lifecycle hooks for tool/model interception and middleware
- [Durable Runtime](core/durable-runtime.md) - Leases with fencing, the durable run queue, schedules, governance state, and agent identity
- [Wake-ups and Webhooks](core/wake-ups.md) - Signed inbound webhooks and queue messages that enqueue durable runs, and signed outbound delivery

## Models

- [Model Providers](models/providers.md) - LLM integrations: Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama
- [Anthropic Client](models/anthropic.md) - `adk-anthropic`: the Messages API with streaming, thinking, prompt caching, and citations
- [OpenAI Responses API](models/openai-responses.md) - The dedicated `/v1/responses` client
- [Gemini Interactions API](models/gemini-interactions.md) - The Beta Interactions client and its `GeminiModel` transport
- [OpenCode Go and Zen](models/opencode.md) - Per-model routing across four wire APIs with conversation headers
- [Ollama](models/ollama.md) - Local inference with Ollama
- [mistral.rs Integration](models/mistralrs.md) - Native mistral.rs for high-performance local inference

## Agents

- [LlmAgent](agents/llm-agent.md) - The core agent type using Large Language Models
- [CodeAgent (CodeAct)](agents/code-agent.md) - Acts by writing and running code via a `CodeRuntime`
- [Workflow Agents](agents/workflow-agents.md) - Deterministic agents: Sequential, Parallel, Loop
- [Multi-Agent Systems](agents/multi-agent.md) - Building agent hierarchies with sub-agents
- [Graph Agents](agents/graph-agents.md) - LangGraph-style workflow orchestration
- [Graph Checkpointing](agents/graph-checkpointing.md) - Checkpointer backends, the version rule, durability modes, pending writes, and the conformance suite
- [Functional API](agents/functional-api.md) - Workflows as async functions with `#[entrypoint]`/`#[task]`, background runs, and cron scheduling
- [Ambient Agents](agents/ambient-agents.md) - Agents driven by cron, file-watch, and webhook triggers
- [Realtime Agents](agents/realtime-agents.md) - Voice-enabled agents with OpenAI/Gemini
- [Desktop Audio](agents/desktop-audio.md) - Microphone capture, speaker playback, and VAD turn-taking with `adk-audio`

## Coding Agent

A native coding agent: read/edit/run code in a sandboxed workspace, autonomous goal mode, and parallel ultra-review workflows.

- [Overview](coding-agent/index.md) - Start here: the pieces, install, and a 60-second quick start
- [Dev Tools](coding-agent/devtools.md) - `adk-devtools`: read/write/edit/glob/grep/bash + the sandboxed `Workspace`
- [Harness](coding-agent/harness.md) - `CodingAgent` (the `coding` feature), the plan loop, `write_todos`; superseded by the [harness](harness/index.md)
- [CLI](coding-agent/cli.md) - `adk-rust code` / `goal` (durable, resumable) / `ultracode`
- [Workflows](coding-agent/workflows.md) - Ultra-review on `adk-graph`: parallel reviewers + fan-in
- [Examples](coding-agent/examples.md) - The three runnable example crates

## Harness

The governed, durable harness agent (`adk-harness`, spec 3.1): modes, approvals, a sandboxed shell, team members, plan graphs and durable goals on one governed path.

- [Overview](harness/index.md) - Start here: what the harness is, how to use it, and a quick start
- [Modes and Approvals](harness/modes-and-approvals.md) - The mode table, prompts, held calls and decisions, standing rules
- [What the Harness Knows About Itself](harness/self-knowledge.md) - The `<harness>` block, the standing-rules digest and `harness_help`, generated from the running harness
- [Configuration](harness/configuration.md) - Builder options and defaults, `.adk/harness.toml`, features, the `harness` template
- [The Harness in Your Terminal](harness/cli.md) - `adk-rust`, `code`, `goal`, `ultracode`, slash commands, `harness.db`
- [Serving the Harness](harness/serving.md) - `adk-rust serve`, request identity, the approval inbox, background runs
- [Harness Shell and Sandbox](harness/shell-and-sandbox.md) - `bash` under Seatbelt or bubblewrap: isolation status, unsandboxed retry, `auto_allow_sandboxed`, audit records
- [Harness Team](harness/team.md) - Members lowered to a `TeamSpec`: local, ACP and A2A members, isolation, budgets, discovery, review rounds
- [Harness Plans and Goals](harness/plan-and-goals.md) - The plan graph with Approve and Review gates and rewind; loop-until goals, durable and ambient
- [Harness Durability](harness/durability.md) - The durability invariants and the tests that prove them
- [Harness Identity](harness/identity.md) - RBAC per tool class, per-call shell credentials, authenticated identity, audit
- [Harness Observability](harness/observability.md) - Spans, spend per agent and subject, the loop guard, evaluation baselines
- [Harness Memory](harness/memory.md) - Working notes behind approval, and long-term facts with provenance recalled into each turn
- [Harness Web Access and MCP Interaction](harness/web-and-mcp.md) - `web_fetch` and `web_search` under the domain allowlist, screened and delimited; MCP elicitation through the prompter or inbox, sampling under the budget, resources as untrusted blocks
- [Harness Hooks](harness/hooks.md) - Hooks on tool events: deny, fail or annotate through the shell's launcher, never allow; workspace hooks under trust; `harness.hook.ran`
- [Harness Payments](harness/payments.md) - The payments pack: `adk-payments` tools as class `Payment`, one non-waivable approval per call, mandate guardrails, spend ledger, `harness.payment` with the journal receipt
- [Harness Voice](harness/voice.md) - The voice surface: the harness as the agent of an `IntegratedRealtimeRunner`, held calls surfaced as realtime events, spoken or inbox approvals, `harness.voice.*` in one audit trail
- [The Personal Preset and Take Over](harness/personal.md) - `--preset personal`: a workspace of notes and files under the home, no host shell, quiet time and a daily spending limit asked at first run; take over: a typed secret reaches one tool call only
- [The Website Runtime](harness/website.md) - feature `website`: `Preset::Website`, the back-office and public toolsets of an AWP 2.0 website, its two partitions and signals, the autonomy ladder compiled to policy, authoring, outbound Web Bot Auth and onboarding

## Agent Kernel

The always-on form of the harness (`adk-kernel`, spec 3.2): durable processes that sleep for free and wake on events, a kill switch across replicas, and a control plane every surface attaches to.

- [Overview](kernel/index.md) - What `adk-rust up` runs: workers per queue, the process table, the event bus, freezing, using the kernel from code
- [Kernel Commands](kernel/cli.md) - `up`, `attach`, `ps`, `top`, `kill`, `suspend`, `resume`, `freeze`, `unfreeze`, `events`, `goal` on a running kernel, `[kernel]` in `harness.toml`
- [The Control Plane](kernel/control-plane.md) - The socket, named-pipe and HTTP transports, the auth matrix, routes, `kernel.control` audit records, `ControlClient`
- [The Event Bus and Outbound Webhooks](kernel/events.md) - Event types, subscribers and sinks, `WebhookSink`: Standard Webhooks signatures, event filters, bounded retry, dead letters, back-pressure that never blocks a process
- [Pairing Devices and Chat Channels](kernel/pairing.md) - `adk-rust pair`, scoped and revocable device tokens stored hashed, the chat channel gateway and its adapter contract suite
- [Drivers](kernel/drivers.md) - MCP servers the kernel supervises under approved manifests: effect classes, the OS sandbox, brokered secrets, re-approval of changed tools, parked calls across a restart
- [Driver classes](kernel/driver-classes.md) - DNS and registrar, email, analytics and search console, social and advertising drivers: their tools, manifests for any MCP server, autonomy caps, outbound rules and reference adapters
- [Running the Kernel as a Service](kernel/service.md) - `adk-rust up --install`, `--uninstall` and `--print`: a launchd agent, a systemd user unit or a Windows service that runs as you
- [The Managed Form](kernel/managed.md) - One kernel per tenant on the managed runtime: `KernelDef` beside `adk.harness`, wake sets, the tenant's proactivity policy, the managed approval path, voice decisions on parked processes
- [Self-observation: `/proc`](kernel/proc.md) - The content-free activity ledger, daily baselines and observations offered as questions, `adk-rust proc`, exporting for `cargo adk eval --ledger`
- [Charter, KPIs, Signals and Earned Autonomy](kernel/charter.md) - A website's charter store, KPI tracker, guardrail pauses, the autonomy ladder, typed signals from the public partition and the site process kinds (feature `charter`, spec 3.5)
- [Kernel Store Backends](kernel/backends.md) - The process, agenda, driver, ledger, partition and cron watermark stores on in-memory, SQLite, PostgreSQL, Redis, MongoDB, Firestore and Neo4j
- [Partitions](kernel/partitions.md) - One harness per partition, class and tenant identity, memory scoping, crossings by the coordinator under a non-waivable approval, `kernel.partition.*` audit records
- [Agents You Create](kernel/agents.md) - The audited agent table, suggestions only a person adds, live hosting through the agent factory, pausing and removing, the agent store on every backend, `kernel.agent.*` audit records

## Agent Surfaces

The interface to an always-on agent (`adk-surface`, spec 3.3): every surface is a window onto the same kernel, and a decision is made once, from anywhere.

- [The Engine](surfaces/index.md) - Feed, projection and commands every surface shares: decision cards in plain words, what each surface may decide, one write path with the version seen, the `web` feature's HTTP API and its generated types
- [The Web Home](surfaces/web-home.md) - Threads, Today and Memory & rules at `/ui/`, in light and dark: decisions with the version seen, what stays read-only until the kernel serves it, mounting it on your own server, the developer panel, and moving from `adk-server`'s bundled runtime UI
- [The Terminal](surfaces/terminal.md) - Full screen, inline and JSON lines from the keyboard: decisions answered by key, Esc never decides, typing queued, device pairing for a kernel on another machine, exit codes for CI
- [Chat](surfaces/chat.md) - The Telegram adapter over the kernel's channel contract: cards with Allow once and Deny and never the call's arguments, pairing by code, "stop all", long polling
- [The Phone](surfaces/phone.md) - The web home installed on a phone: wake-ups from your own kernel that say nothing, answers by touch, "Confirm it's you" with a passkey on the phone or the computer, pairing by QR code, `adk-rust up --home-url` behind Tailscale Serve
- [The Desktop Tray](surfaces/desktop.md) - Experimental: the web home in a desktop window and a tray icon with the decisions badge; its panel says yes once or no to everyday calls, opens the home for the rest, and stops everything; loopback only
- [Voice and Video](surfaces/voice.md) - Spoken "allow once" and "deny" through the engine with a cancel window, bound to the decision on screen, a digest on record and never the recording; the camera and the screen while their switches are on; the voice view and the terminal's voice line
- [The Website's Views](surfaces/website.md) - Experimental, the `website` feature: a prebuilt website's setup code, onboarding, charter, site view, weekly review and web editor at `/ui/website`, over `adk-website`'s desk; who may decide what, and mounting it on your own server
- [The Workflow Observer](surfaces/workflow.md) - Beta, the `workflow` feature: a graph agent's runs, steps and what each changed on the developer panel's Runs tab; forks from an earlier step with edits, where a call that already happened is not sent again; a graph node's decision on every surface; `adk-rust graph steps` and `graph fork`
- [Agent-Drawn UI](surfaces/genui.md) - The `genui` feature: forms, cards, tables and screens agents draw in threads, never with approval controls; a form's Send asks the drawing agent for the call, held for the person on the home's own card; the terminal's fill-in pane; moving from `adk-ui`
- [Scenarios](surfaces/scenarios.md) - The phase scenarios as programs you can run: "Approve it tomorrow" (a decision that waits through quiet time and a `kill -9`, answered in Telegram) and "The morning after" (one exact briefing across a restart), on SQLite and PostgreSQL; "Governed form" (a drawn form's call waits for the person whatever answered before)

## Realtime & Multimodal

A dedicated, newcomer-friendly guide to building voice + multimodal agents — architecture, concepts, and runnable examples.

- [Overview](realtime/index.md) - Start here: the mental model, install, and a 60-second quick start
- [Architecture](realtime/architecture.md) - The four layers, event loop, audio pipeline, and turn lifecycle
- [Providers](realtime/providers.md) - OpenAI Realtime & Gemini Live models, voices, and how to choose
- [Tools](realtime/tools.md) - Server-side actions: native handlers, bridged `adk_core::Tool`s, parallel calls
- [Multimodal](realtime/multimodal.md) - Video input via `send_video_frame` and browser camera capture
- [Affective Dialogue](realtime/affective-dialogue.md) - Emotion-aware responses on Gemini native-audio
- [Memory](realtime/memory.md) - Context injection, turn storage, and knowledge-graph self-curation
- [Building Web Apps](realtime/building-web-apps.md) - The server-side bridge, WS protocol, and Web Audio
- [Examples](realtime/examples.md) - The four runnable examples and a suggested learning path

## Tools

- [Function Tools](tools/function-tools.md) - Create custom tools with async Rust functions
- [Tool Authorization](security/tool-authorization.md) - Tool policy, confirmation, callbacks, RBAC, and graph interrupts
- [Schema Normalization](tools/schema-normalization.md) - Provider-specific tool-schema normalization
- [Tool Effects and the Action Ledger](tools/tool-effects.md) - Retry safety, idempotency keys, delegation timeouts, and calls that must not repeat
- [Built-in Tools](tools/built-in-tools.md) - Pre-built tools like GoogleSearchTool
- [Memory Tools](tools/memory-tools.md) - Let an agent search its own long-term memory
- [Code Execution](tools/code-execution.md) - Code tools over `adk-code` and `adk-sandbox`
- [Python Code Execution](tools/python-code-execution.md) - In-process Python through the Monty interpreter
- [Sandboxed Code Execution](sandbox/index.md) - `adk-sandbox` process and WASM backends with OS-level profiles
- [MCP Tools](tools/mcp-tools.md) - Connecting MCP servers as agent tools
- [MCP overview and architecture](mcp/index.md) - Where MCP fits and which ADK-Rust surface to use
- [MCP client](mcp/client.md) - Tools, resources, prompts, completion, subscriptions, elicitation, tasks, and HTTP
- [Dynamic MCP server manager](mcp/manager.md) - Runtime local-server registry, persistence, monitoring, and restart
- [MCP server authoring](mcp/server.md) - Publish tools and other capabilities with the aligned official Rust SDK
- [MCP security](mcp/security.md) - Authentication, visibility, authorization, approval, and secret handling
- [MCP testing](mcp/testing.md) - Deterministic gates and deployment verification
- [Browser Tools](tools/browser-tools.md) - 46 WebDriver tools for web automation
- [UI Tools](tools/ui-tools.md) - The `genui` render tools and the server's UI protocol profiles (`adk_ui`, `a2ui`, `ag_ui`, `mcp_apps`)
- [RAG](tools/rag.md) - Retrieval-Augmented Generation for knowledge base search
- [Vertex AI RAG Engine](rag/vertex-rag-engine.md) - Retrieval from managed RAG Engine corpora
- [Agent Retrieval](rag/agent-retrieval.md) - The Agent Retrieval (Vector Search 2.0) vector store
- [Governed Desktop Automation](computer-use/index.md) - `adk-computer-use` over the `computer-use-mcp` server
- [ACP Tools](tools/acp-tools.md) - Use an ACP coding agent as a typed ADK-Rust tool
- [Retry & Reflect](tools/retry-reflect.md) - Tool failure recovery with reflection prompts
- [Action Nodes](tools/action-nodes.md) - 14 deterministic node types for workflow graphs
- [Benchmarking](tools/benchmarking.md) - Performance measurement with `cargo adk bench`

## Agent Client Protocol

Use an external coding agent from ADK-Rust, or expose an ADK-Rust agent to an
editor through stable ACP v1.

- [Architecture and concepts](acp/index.md) - Client/host and coding-agent roles, sessions, capabilities, and protocol flow
- [ACP client and host](acp/client.md) - Delegation, persistent sessions, streaming, cancellation, permissions, files, terminals, and MCP
- [ACP agent server](acp/server.md) - Expose the Runner, tools, sessions, and live events through stdio
- [Testing and support matrix](acp/testing.md) - Verified interoperability and deliberately unadvertised features

## Sessions & State

- [Sessions](sessions/sessions.md) - Session management and lifecycle
- [State Management](sessions/state.md) - Managing conversation state with prefixes
- [Context Compaction](sessions/context-compaction.md) - Summarizing older events to bound request size

## Memory

Long-term memory that outlives a session — the persistent counterpart to session state. A dedicated, newcomer-friendly guide to both the semantic store and the bi-temporal knowledge graph.

- [Overview](memory/index.md) - Start here: the two kinds of memory, the mental model, install, and a quick start
- [Concepts](memory/concepts.md) - `MemoryEntry`, the `MemoryService` trait, search, project scoping, GDPR erasure
- [Backends](memory/backends.md) - The six stores (InMemory, SQLite, Postgres+pgvector, Redis, MongoDB, Neo4j) and how to choose
- [Knowledge Graph](memory/knowledge-graph.md) - `GraphMemoryService`: entities, observations, relations, and why bi-temporal matters
- [Tools & Agents](memory/tools-and-agents.md) - Wiring memory into agents; `remember`/`relate` and the memory tools

## Skills

- [Vertex AI Skill Registry](skills/skill-registry.md) - Reading skills from the Gemini Enterprise Agent Platform Skill Registry

## Callbacks & Events

- [Callbacks](callbacks/callbacks.md) - Intercept and customize agent behavior
- [Events](events/events.md) - Understanding the event system and conversation history

## Artifacts

- [Artifacts](artifacts/artifacts.md) - Binary data storage and retrieval

## Observability

- [Telemetry](observability/telemetry.md) - Logging, tracing, and monitoring
- [Telemetry to Google Cloud](observability/gcp.md) - Cloud Trace export and Cloud Logging JSON logs

## Deployment

- [Launcher](deployment/launcher.md) - Running agents in console or server mode
- [Server](deployment/server.md) - REST API, streaming and A2A integration
- [A2A Getting Started](a2a/getting-started.md) - Create and run an A2A agent
- [A2A Protocol](deployment/a2a.md) - Agent-to-Agent communication
- [Agentic Web Protocol 2.0](deployment/awp.md) - The signed site contract: profiles, keys and releases, the edge, verification, and what each crate provides
- [Spin Up an AWP Website](deployment/website-quickstart.md) - `adk-rust website new`, the `adk-website` binary, or `cargo adk new my-site --template website`: the setup code, keys, onboarding and deployment shapes
- [Move a WordPress Site to AWP](deployment/wordpress-to-awp.md) - Prove the domain, freeze, migrate, decide the quarantine, publish signed releases and verify
- [The Charter](deployment/charter.md) - A site's goals, KPIs with baselines, guardrails, authority, budget and public terms, and how its owners sign it
- [Writing Capability Packs](deployment/capability-packs.md) - One declaration and one handler per capability, served over A2A, MCP and HTTP; the default packs
- [The AWP Agent Edge](deployment/awp-edge.md) - A site's capabilities over A2A, MCP and HTTP: trust tiers, held and exactly-once calls, the public partition
- [Site Migration](deployment/site-migration.md) - Moving an existing site onto a signed static AWP site
- [Constantly Updated Sites](deployment/constant-updates.md) - Lanes, batching, feed bounds and delta releases for sites that change every minute
- [Sites and AWP from the Command Line](deployment/site-cli.md) - `adk-rust site` and `adk-rust awp`: releases, content, migration, keys and verification
- [Site Hosts](deployment/site-hosts.md) - Publishing signed releases to S3, GCS, Azure, Cloudflare, Netlify, GitHub Pages or any server over SSH
- [Site Generators](deployment/site-generators.md) - Building a site with Zola, Hugo, Astro or Build Awesome in a sandbox with no network
- [The AWP Website Product](deployment/adk-website.md) - `adk-website`: site kinds, the team of eight agents, onboarding from nothing, a presence or an existing site, incidents, alarms and the weekly review
- [Playbook Learning](deployment/website-learning.md) - Experiment decisions and KPIs short of the owner's targets propose playbook revisions; the owner accepts, edits or declines each, and accepted revisions are versioned
- [Publishing from Git](deployment/website-repository.md) - A signed push from an authorised committer proposes a release on the owner's decision card: GitHub, GitLab or the plain signed form, a local repository or a forge's raw endpoint
- [Website Experiments and Measurement](deployment/website-experiments.md) - Experiments on the charter's KPIs with guardrails that stop them, variants as static files, and the KPI sources that are not drivers
- [Agent Engine](deployment/agent-engine.md) - Deploying on the Gemini Enterprise Agent Platform
- [Agent Registry](deployment/agent-registry.md) - Discovery, registration, and remote invocation on the Agent Platform

## Evaluation

- [Agent Evaluation](evaluation/evaluation.md) - Testing and validating agent behavior
- [Benchmark Results](evaluation/benchmarks.md) - Published performance comparison data
- [Vertex AI Evaluation](evaluation/vertex-eval.md) - The Gen AI Evaluation Service bridge
- [Web Experiments](evaluation/experiments.md) - Sequential A/B analysis with guardrails and a Bayesian option

## Managed Agents

- [Managed Agent Runtime](managed-agents/runtime.md) - Provider-neutral, durable, resumable agent execution engine (Experimental)

## Security

- [Access Control](security/access-control.md) - Role-based permissions and audit logging
- [Guardrails](security/guardrails.md) - PII redaction, content filtering, schema validation
- [Memory](security/memory.md) - Long-term semantic memory for agents
- [Payments and Commerce](security/payments.md) - Agentic commerce journeys, protocol support, and validation paths
- [Spend Ledger](security/spend-ledger.md) - One budget for model calls and payments, with daily, monthly, and lifetime caps
- [Wallet](security/wallet.md) - An owner-funded wallet: category mandates, caps checked before execution, holds, rails, anomaly pauses, reconciliation
- [Approval Service](security/approval-service.md) - Approval inbox with quorum, delegation, escalation, expiry, webhooks, and server endpoints
- [Agent Identity](security/agent-identity.md) - Stable agent ids on every policy decision, approval, spend record, and audit event; resource-scoped credential brokers
- [Durable Stores](security/durable-stores.md) - Spend ledger, action ledger, and approval store backends, their conformance suite, and the no-duplicate guarantee
- [Audit Log](security/audit-log.md) - Signed, Merkle-hashed audit log with checkpoints, key rotation, and offline verification
- [Web Bot Auth](security/web-bot-auth.md) - Verified identity for automated clients: RFC 9421 request signatures, key directories, signer and verifier

## Compliance

- [Vertex-Only Deployments](compliance/vertex-only-deployments.md) - Guarantee no Gemini Studio (`generativelanguage.googleapis.com`) traffic for HIPAA and data-residency workloads

## Studio

- [ADK Studio](studio/studio.md) - Visual development environment for building agents
- [Action Nodes](studio/action-nodes.md) - Non-LLM programmatic nodes for automation workflows
- [Triggers](studio/triggers.md) - Webhook, schedule, and event triggers for workflows

## Development

- [Development Guidelines](development/development-guidelines.md) - Contributing guide and best practices
- [Composable Templates](development/composable-templates.md) - `cargo adk new` templates, addons, and enterprise patterns
- [cargo adk build](development/cargo-adk-build.md) - Compile an agent project without deploying it
- [Performance 0.8](development/performance-0-8.md) - Optimization release examples and adoption-focused validation

---

## Validation Status

Copy-paste Cargo commands and dependency snippets in the README and official docs are validated by `scripts/check-doc-examples.sh`, and their versions and `adk-rust` feature names by `scripts/check-doc-versions.sh`. CI also checks cargo-adk scaffolds with `scripts/check-cargo-adk-templates.sh`, rejects duplicate example target names with `scripts/check-example-name-collisions.sh`, and compiles every standalone example under `examples/` with `scripts/check-examples-compile.sh`.
