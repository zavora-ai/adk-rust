# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Overlapping and interrupted tool history** (`adk-runner`): model requests
  retain actual tool results from overlapping turns and omit unresolved calls
  from earlier turns without fabricating interruption results. Persisted events
  remain unchanged.
- **OpenAI Responses stream completion** (`adk-model`): completion events retain
  model errors and usage metadata. Open Responses mode ignores empty SSE data
  frames, restores missing arguments by tool identity or output index, and keeps
  streamed arguments when a terminal snapshot is blank.
- **OpenAI-compatible structured tool arguments** (`adk-model`): Chat
  Completions streaming now preserves structured JSON `function.arguments`
  emitted by compatible intermediaries, including repeated and empty snapshot
  chunks, instead of converting the tool call to empty arguments.
- **OpenAI Responses web search** (`adk-tool`): the stable built-in tool now
  serializes as `web_search`, matching the OpenAI Responses API. The explicit
  preview variant remains `web_search_preview_2025_03_11`. URL annotations from
  response text are preserved as ADK citation metadata. Open Responses mode now
  also accepts compatible endpoints that omit output-message IDs, statuses, or
  empty `output_text.annotations`, and reconstructs final function arguments
  from their streaming events when the completed response omits them.
- **OpenAI-compatible usage metadata** (`adk-model`): Chat Completions
  streaming and non-streaming responses now use one normalization path,
  preserve the complete provider-native `usage` object in `provider_usage`,
  and project reported cache-write tokens alongside cache reads.
- **OpenAI tool request stability** (`adk-model`): Chat Completions and
  Responses requests now serialize function tools in deterministic name order,
  preserving identical cacheable prefixes across repeated executions.

## [2.2.0] - 2026-09-01

### Added

- **Progressive skill disclosure** (`adk-skill`, `skills-progressive-disclosure`):
  a Google ADK-style `SkillToolset` exposing `list_skills`, `load_skill` and
  `load_skill_resource`, so an agent discovers a skill index first and pulls full
  skill bodies and resources only when it needs them, rather than carrying every
  skill in the prompt. `ResourceAccessPolicy` bounds which resources a loaded
  skill may read, and `ReadonlyContext::state()` is a new defaulted trait method
  letting dynamic toolsets read session state without mutable access —
  third-party context implementations stay source-compatible.

- **Wave 4 platform services (round two)** — the Govern-pillar invocation
  and consumption features, closing out the Agent Engine plan:
  - `vertex-remote-engine` (adk-server): `RemoteReasoningEngineAgent` —
    invoke other deployed Agent Engine agents as sub-agents over
    `reasoningEngines:streamQuery` (`?alt=sse`, canonical camelCase
    envelope), with `stream_query` fallback, mid-stream error events, and
    URN resolution via the Agent Registry. Wire compatibility with the
    server-side dispatcher is pinned by the shared fixture round-trip.
  - Registry registration CLI (adk-cli `vertex-agent-registry`):
    `adk-rust registry register-agent|register-mcp|register-endpoint|search`
    with idempotent create-or-patch upsert
    (`AgentRegistryClient::register_or_update_service`). Agent Engine
    deployments auto-register and are deliberately not re-registered.
  - Remote skills (adk-skill `vertex-skill-registry` + adk-cli):
    `load_skill_index_from_registry` (registry-sourced skills are
    byte-equivalent to disk-sourced ones through the unchanged
    `SkillDocument` path), `SkillSearchTool`, and
    `adk-rust skills search|pull`.
  - `examples/agent_orchestrator`: discover a registered agent with
    `AgentSearchTool`, then delegate to it via
    `RemoteReasoningEngineAgent`.
- **Wave 4 platform services (round one)** — five opt-in Gemini Enterprise
  Agent Platform integrations, all composable with any preset and appended
  to `gemini-agent-platform`:
  - `vertex-eval` (adk-eval): Gen AI Evaluation Service bridge —
    `VertexEvalClient` for `evaluateInstances` (pointwise + trajectory
    metrics, autorater config) and `VertexEvalJudge` mirroring `LlmJudge`'s
    surface with service-side scoring.
  - `vertex-rag` (adk-rag): Vertex AI RAG Engine retrieval —
    `VertexRagEngineClient` (corpus discovery, `retrieveContexts` via the
    current `ragRetrievalConfig` wire path) and `VertexAiRagRetrievalTool`
    (read-only, concurrency-safe). Ships `examples/vertex_rag`.
  - `agent-retrieval` (adk-rag): Agent Retrieval (formerly Vector Search
    2.0) as a full `VectorStore` backend — BYOE dense vectors, chunk text
    and metadata stored in the Data Object, atomic batched writes,
    dot-product search, and a `hybrid_search` RRF extra. Passes the shared
    `VectorStore` contract/property suite.
  - `vertex-agent-registry` (adk-tool): Agent Registry client
    (registration for custom deployments, URN-aware get, search across
    agents and MCP servers) and `AgentSearchTool` for agent-driven
    discovery.
  - `vertex-skill-registry` (adk-skill): read-only Skill Registry client
    (get/list/semantic search/revisions, sha256-verified inline zip
    payloads) with defense-in-depth archive extraction limits.
- **Vertex RAG grounding declaration** (adk-gemini `vertex`, adk-model
  `gemini-vertex`, no new flag): `Tool::Retrieval` with `VertexRagStore`
  (current `ragRetrievalConfig` path; deprecated fields not exposed),
  `GeminiModel::with_vertex_rag_store(...)`, Studio-backend requests fail
  with a structured error, and grounding responses surface
  `retrievedContext` including `ragChunk` provenance.

### Fixed

- **OpenAI-compatible streaming usage** (`adk-model`): usage-only terminal
  chunks with empty `choices` attach token counts to the final response.
- **Span parenting across suspension points** (`adk-agent`, `adk-runner`): one
  logical invocation was exported as several disconnected traces. `LlmAgent`
  held a `call_llm` span guard across the `.await`/`yield` points of its
  `async_stream` generator — an entered guard is bound to the thread, not the
  task, so it was neither exited on suspension nor re-entered on resumption, and
  everything after the first suspension detached from `call_llm`. `Runner`
  instrumented only the future that constructs the agent stream, not the
  draining of it, so every span the agent produced was created outside
  `agent.execute`. Both now instrument the individual futures, which is correct
  across suspension by construction. Span names, attributes and durations are
  unchanged — only parentage.
- **`adk-realtime` with `livekit` compiles again**: livekit 0.8.1 requires
  `livekit-data-stream = "0.1"`, and 0.1.2 changed `incoming::Manager::new`'s
  arity and added a `topic` field, which broke livekit's own source. The
  workspace now pins `=0.1.1`.
- **`adk-audio` ONNX features compile again**: `ort` 2.0.0-rc.13 moved the CUDA
  and CoreML execution providers out of `ort::execution_providers`, so the
  floating pre-release requirement broke every ONNX model feature. Pinned to
  `=2.0.0-rc.11`.

## [2.1.0] - 2026-08-25

### Added

- **ADK-Rust runtime UI** (`adk-server`): replaces the opaque copied Angular
  bundle with an owned React/TypeScript frontend using the Studio Next design
  language. The responsive system/light/dark interface streams conversations,
  tool activity, failures, and handoffs, and inspects event timelines, session
  and shared state, artifacts, prior sessions, runtime capabilities, and UI
  protocols. `Agent::topology()` is an additive provider-neutral metadata hook;
  `CompiledTeam` exposes its exact members and delegation-versus-handoff edges,
  and `/api/ui/agents/{name}` serves that data without coupling the server to
  `TeamSpec`. The embedded build uses local assets only and ships with CSP,
  no-sniff, referrer, and immutable-cache headers. Strict TypeScript and
  provider-free SSE parser tests accompany the existing Rust server tests.
  Model responses render safe GitHub-flavored Markdown, and live team runs
  animate the active member and incoming edge using Studio Next's directional
  flow pattern while honoring reduced-motion preferences. Transcript messages
  keep a readable minimum height instead of collapsing around long responses.
  `GraphAgent` also exports its entry and declared control-flow edges through
  the same portable topology contract, allowing the UI to render workflow nodes
  by execution level. `examples/runtime_ui_showcase` documents and illustrates
  tool-calling, graph, and team runs through the embedded interface. Realtime
  agents now declare their interaction mode through the shared `Agent` contract;
  the interface coalesces their transcript stream and exposes completed PCM16
  output as playable WAV audio. Dedicated **Telemetry** and **Protocols** views
  make spans, configured artifact/memory services, A2A discovery, and UI/MCP
  Apps support explicit. The ADK Runtime brand links to adk-rust.com.
  `examples/advanced_agents` runs OpenAI chat, an ambient schedule, OpenAI
  Realtime voice, A2A, and MCP `2026-07-28`/SEP-2663 tasks through one server.

- **Anthropic request customization and server-side refusal fallback**
  (`adk-anthropic`): additive client methods now support caller-selected beta
  headers, exact per-request header replacement, bearer-only authentication,
  and API-version overrides without adding fields to the exhaustive
  `MessageCreateParams` struct. `ServerFallbackRequest` models both
  `"default"` routing and one-to-three explicit fallback models, validates
  duplicates and primary-model loops, and has fallback-aware response and SSE
  types so handoff markers and per-attempt usage are not discarded. Fallback
  is documented and tested as a safety-refusal feature, not a retry mechanism
  for rate limits, overloads, or server errors. New live examples read
  credentials from the environment; provider-free wire tests cover headers,
  bodies, responses, and streams.
- **Vertex AI Agent Engine sandbox client** (`adk-code`, feature
  `vertex-sandbox`; same-named umbrella feature, part of
  `gemini-agent-platform`): `VertexSandboxClient` against the v1beta1
  `sandboxEnvironments` surface — create and delete wait their long-running
  operations, get and list (paginated) are plain reads, and `:execute` is
  synchronous. `execute_code` implements the code-execution chunk
  conventions shared with adk-python and the Vertex AI SDK (JSON code
  chunk, `file_name`-attributed file chunks, `msg_out`/`msg_err` console
  output) with the 100 MB per-request file limit enforced before sending.
  `SandboxCodeExecutor` mirrors adk-python's
  `AgentEngineSandboxCodeExecutor`: per-session lazy sandbox creation
  (display name `default_sandbox`, TTL one year) with recreate when the
  sandbox is missing or not running. `VertexSandboxTool` exposes execution
  to LLM agents keyed by the calling session. Built on the shared `adk-gcp`
  plumbing. New example: `examples/vertex_sandbox`.

- **`adk-gcp` crate**: shared Google Cloud REST plumbing for Vertex AI
  backends — `GcpHttpClient` (ADC with cached auth headers per
  `CacheableResource` semantics, redirect-disabled bounded transport,
  HTTPS-or-loopback endpoint validation), `LroPoller` (long-running-operation
  polling with capped backoff, operation identity pinning, and
  project/location scope validation), `VertexResourceName`
  (`projects/*/locations/*/reasoningEngines/*` parse/format), and
  `GcpErrorContext` (consumer-branded errors across the `AdkError`
  boundary). Purely additive: consolidates the pattern duplicated across
  `adk-session`, `adk-memory`, `adk-tool`, `adk-deploy`, and `adk-artifact`;
  call-site migrations follow in later PRs.

- **CLI deploy subcommand** (`adk-cli`, feature `gcp-deploy`):
  `adk-rust deploy agent-engine --image-uri <uri> --project <p> --location <l>
  [--service-account <sa>] [--kms-key <key>] [--display-name <n>]` deploys a
  pushed container image as a Gemini Enterprise Agent Platform engine via the
  adk-deploy `gcp` client — declares the full class-method contract, waits
  for the create operation, and prints the engine resource name. The display
  name defaults to the image name. Install with
  `cargo install adk-cli --features gcp-deploy`.

- **Agent Engine deployment client** (`adk-deploy`, feature `gcp`; umbrella
  feature `gcp-deploy` — host-side tooling, deliberately not part of
  `gemini-agent-platform`): `GcpDeployClient` with exactly four operations
  against the v1beta1 `reasoningEngines` surface — `create_reasoning_engine`,
  `poll_operation` (plus a backoff-polling `wait_for_operation`),
  `get_reasoning_engine`, `delete_reasoning_engine`. Typed camelCase wire
  DTOs for the BYOC create body (`containerSpec.imageUri`, `deploymentSpec`
  env/secret-env/scaling/resource limits/PSC-I, `classMethods`,
  `agentFramework`, `serviceAccount`, CMEK `encryptionSpec`);
  `CreateReasoningEngineRequest::byoc` declares the full WP1 class-method
  contract by default. Image build/push stays with Cloud Build
  (`gcloud_build_submit_command` renders the documented command).
  `reasoningEngines:asyncQuery` is deliberately not declared (cannot be
  added post-create; adk-python parity excludes it).

- **cargo-adk `agent-engine` template** (`cargo adk new my-agent --template
  agent-engine`): scaffolds a Gemini Enterprise Agent Engine BYOC container — a
  binary whose `main` is `serve_agent_engine(agent, AgentEngineOptions::new())`
  (features `["minimal", "agent-engine"]`), the docker addon's `Dockerfile` and
  `.dockerignore`, and `deploy/terraform/` with a
  `google_vertex_ai_reasoning_engine` BYOC resource (`spec.container_spec.image_uri`,
  the full 14-method `class_methods` contract in `jsonencode`,
  `agent_framework = "google-adk"`, optional `service_account`), plus
  `variables.tf`/`outputs.tf` mirroring the containerized-agent codelab. The
  generated README covers `gcloud builds submit`, `terraform apply`, the
  platform-set environment variables, the `/api` passthrough for querying the
  deployed engine, and the optional A2A-routes setup via
  `ServerBuilder::with_agent_engine(true)` + `.with_a2a(...)`. Template and
  addon file fragments now support `{name}` substitution and path override
  (a template file replaces a base file with the same path).
- **Tool guardrails** (`adk-guardrail`; `adk-agent` feature `guardrails`):
  `Guardrail` validates `Content` and never sees a tool call, and
  `ToolConfirmationPolicy` decides per tool *name*, so neither could express
  "this tool may run, but not with these arguments" — argument-level policy had
  nowhere to live inside the framework. `ToolGuardrail::validate_call` receives
  the tool name and arguments and returns `Allow`, `Deny`, or `ReviseArgs`;
  `ToolGuardrailSet::evaluate` runs guardrails in order so revisions compose, and
  stops at the first denial. `LlmAgentBuilder::tool_guardrails` wires a set in.
  Screening happens before the concurrency permit is acquired and before
  confirmation is resolved, so a denied call neither queues behind other work nor
  prompts the user, and the denial is reported as the tool's result so the model
  can correct the call instead of the run stalling. Two implementations ship:
  `DeniedArgumentPattern` (regex over the serialized arguments, optionally scoped
  to named tools) and `PathAllowList` (confines path-valued arguments to allowed
  roots, comparing by path component rather than string prefix so
  `/etc/passwd-backup` is not admitted by a root of `/etc/passwd`, and refusing
  any path containing a `..` component. It also resolves every existing candidate
  component to reject symlink escapes; hostile local races still require secure-open
  primitives in the filesystem tool itself).

- **Skill write path** (`adk-skill`): the crate was read-only — every `fs::write`
  lived behind `#[cfg(test)]` — so an agent could not persist a skill it derived
  at runtime and an operator could not generate one programmatically.
  `SkillWriter` writes into the `.skills` directory `load_skill_index` already
  discovers, through a unique temporary file that is synchronized and atomically
  replaces the destination on Unix and Windows, so a crash mid-write cannot leave
  a half-written skill that breaks the whole index load. `SkillDraft` is a builder
  for the document; `SkillDraft::to_markdown`
  renders frontmatter plus body and omits unset fields, and round-trips through
  `parse_skill_markdown`. `validate_skill_name` enforces the specification's
  `[a-z0-9-]` rule (1–64 characters, no leading or trailing hyphen) and is also
  the path-safety boundary, since a name becomes a filename — `../escape` and
  `nested/name` are rejected before any file is touched. `SkillWriter::remove`
  and `exists` complete the lifecycle. `SkillInjector::reloaded` rescans the root
  and returns a refreshed injector, and `SkillInjector::root` reports what it
  rescans; `reloaded` returns a new value rather than mutating because
  `build_plugin` captures the index by handle, so a plugin already handed to a
  runner must be rebuilt to see new skills.

- **`AgentInvoker` and the ambient runner bridge** (`adk-core`, `adk-runner`,
  `adk-agent` feature `ambient`): `AmbientAgent::start` refuses to run without a
  trigger handler, and writing one meant building `Content`, inventing a session
  id, and calling `Runner::run` by hand. `Runner::run` also resolves an *existing*
  session and yields `session.not_found` through the stream when there is none,
  which an external trigger has no opportunity to pre-register — so the obvious
  wiring failed at the first tick, inside the stream rather than at the call site.
  `adk-core` now defines `AgentInvoker`, a single `invoke(user_id, session_id,
  content)` operation whose implementations create a missing session;
  `adk-runner` implements it for `Runner`; and
  `AmbientAgent::with_invoker(invoker, RunnerTriggerConfig)` supplies the handler.
  `TriggerSessionPolicy` chooses between `PerTrigger` (default — a fresh session
  per event, so a frequent schedule cannot grow one session's history and per-run
  cost without bound) and `Shared(id)`; shared invocations are serialized through
  the returned stream. The wrapper adopts the runner's executable root for accurate
  diagnostics. `RunnerTriggerConfig::with_prompt` shapes the event into prompt text.
  The OpenAI-backed `ambient_cron_agent` example failed at `start()`
  and documented that it did not invoke the agent; it now runs all seven lifecycle
  steps and prints what each run produced.

- **Cron missed-tick handling** (`adk-agent`, feature `ambient`):
  `CronTrigger::subscribe` computes the next tick from the moment it is called, so
  a trigger that restarted after downtime — or ran on a host that suspended —
  resumed at the next future tick and discarded every tick that came due in
  between, with no record that anything was skipped. `MissedTickPolicy` now
  decides that span's fate: `Skip` (default, prior behaviour), `CoalesceOne` (one
  event for the whole gap), or `All` (one event per elapsed tick, oldest first,
  bounded by `CronTrigger::with_max_catch_up`, default 64). Detecting a gap across
  a process restart needs a `TickWatermark`; `FileTickWatermark` stores one
  RFC 3339 cursor using portable atomic replacement. A capped replay persists its
  skipped-through cursor, and a persistence failure stops the stream before
  emission. Replayed events carry
  `scheduled_for`, `catch_up`, and — for `CoalesceOne` — `missed_count` in their
  payload. The watermark advances on emission rather than on consumer completion,
  making delivery at-most-once so a consumer that stops polling cannot replay the
  same gap on every restart.

- **cargo-adk `docker` addon** (`cargo adk new my-agent --addon docker`): emits a
  multi-stage `Dockerfile` (`rust:1.95-slim` build stage kept in lockstep with
  `rust-toolchain.toml`, `gcr.io/distroless/cc-debian12` runtime, `ENV PORT=8080`,
  `ENTRYPOINT ["/app/agent"]`), a fully static `Dockerfile.static`
  (`x86_64-unknown-linux-musl` + `FROM scratch`, CA bundle copied in for
  `rustls-tls-native-roots`, with a compatibility guard naming the feature sets
  that cannot link statically), and a `.dockerignore`. Fixes the README drift
  that advertised a `docker` addon the registry did not provide.
- **Example Store client** (`adk-tool`, feature `example-store`; umbrella
  feature `example-store`, included in `gemini-agent-platform`):
  `ExampleStoreClient` is an ADC-authenticated REST client for the Vertex AI
  Example Store v1beta1 data plane (Preview, `us-central1` only) —
  `upsert_examples`, `search_examples`, and `fetch_examples` against a
  pre-provisioned `projects/*/locations/*/exampleStores/*` resource (no store
  create/delete). `ExampleStoreProvider` packages top-k retrieval as a
  `BeforeModelCallback` that injects the most similar stored examples into the
  request preamble as dynamic few-shot instructions. New standalone example:
  `examples/example_store/`.
- **Vertex AI Memory Bank backend** (`adk-memory`, feature `vertex-memory`;
  umbrella feature `vertex-memory`, included in `gemini-agent-platform`):
  `VertexAiMemoryBankService` implements `MemoryService` over the platform's
  `memories:generate` (LRO-polled) and `memories:retrieve` (similarity
  search) endpoints with the same `{app_name, user_id}` scope adk-python
  writes, so both runtimes share one Memory Bank. `VertexAiMemoryConfig`
  mirrors the session config (`from_env()` reads the platform's container
  variables). `add_events_to_memory` persists a subset of events;
  `delete_user` enumerates and deletes a scope's memories. This completes the
  Agent Engine dispatch surface's `async_add_session_to_memory` /
  `async_search_memory` class methods, which returned `Unsupported` until
  now.

- **Agent Engine turnkey entrypoint** (`adk-server`, feature `agent-engine`;
  umbrella feature `agent-engine`, included in `gemini-agent-platform`):
  `serve_agent_engine(agent, options)` is the whole `main` of a deployable
  Gemini Enterprise Agent Platform engine — binds `0.0.0.0:$PORT` (fallback
  `8080`), installs the crypto provider, and serves the dispatch endpoints
  plus `GET /health`. `AgentEngineOptions` configures session, memory, and
  artifact services, app name, and port; `build_agent_engine_app` returns the
  same app as a plain `Router`. `ServerBuilder::with_agent_engine(true)`
  mounts the dispatch surface alongside the built-in REST/UI/A2A routes.
  New docs page: `docs/official_docs/deployment/agent-engine.md`.

- **Agent Engine dispatch surface** (`adk-server`, feature `agent-engine`): the
  container-side runtime contract that makes an adk-rust agent drivable by the
  Gemini Enterprise Agent Platform (`reasoningEngines.query` /
  `streamQuery`, console Playground, platform SDKs). `agent_engine_router`
  mounts `POST /api/reasoning_engine` (unary, `{"output": ...}`) and
  `POST /api/stream_reasoning_engine` (newline-delimited JSON events), dispatching
  the exact `AdkApp` operation set — session CRUD in sync/async pairs,
  `stream_query` / `async_stream_query`, `streaming_agent_run_with_events`,
  `async_add_session_to_memory` / `async_search_memory` (Unsupported until a
  memory service is configured), and `register_operations`. A shared wire
  fixture (`adk-server/tests/fixtures/agent_engine_wire.json`) pins the
  envelope and streamed-event framing for the future remote-engine client.

- **adk-rag: shared `VectorStore` contract test suite.** A new
  `adk-rag/tests/common/vector_store_contract.rs` holds the behavioral contract
  every vector store backend must satisfy — idempotent collection creation,
  upsert-then-search round trips, descending-score ordering bounded by `top_k`,
  deletion by ID, empty-input no-ops, metadata preservation, collection
  isolation, and teardown — plus proptest search invariants. The suite runs
  against InMemory, SurrealDB (embedded), and LanceDB (embedded, behind the
  `lancedb` feature). LanceDB scopes out the upsert-replaces-by-ID assertion:
  its `upsert` appends instead of replacing, a divergence the suite documents
  rather than fixes.
- **GCS artifact backend** (`adk-artifact`, feature `gcs`): `GcsArtifactService`
  stores artifacts in a Google Cloud Storage bucket over the GCS JSON API with
  ADC authentication, keeping byte-for-byte blob-name parity with adk-python's
  `GcsArtifactService` (session-scoped and `user:`-namespaced layouts, `adkDisplayName`/
  `adkIsText`/`adkFileUri`/`adkFileMimeType` object metadata, versions starting at 0).
  Umbrella feature `gcs-artifacts`, included in the `gemini-agent-platform` meta-feature.
- **adk-gemini: cached content on Vertex AI.** `VertexBackend` implements the five
  cached-content operations (create, get, update, list, delete) against the Vertex
  REST endpoint `…/v1/projects/{project}/locations/{location}/cachedContents`,
  including TTL refresh via `updateCachedContent` so the runner's cache-refresh
  path works on Vertex. Studio-style model names (`models/{model}`) in create
  payloads are normalized to full Vertex resource names. The Files API, batch
  operations, and the Interactions API remain Studio-only on the Vertex backend.
- **Telemetry to Google Cloud** (`adk-telemetry` feature `gcp`, umbrella
  `gcp-telemetry`, included in `gemini-agent-platform`). `init_with_gcp`
  exports traces to `https://telemetry.googleapis.com` with per-request
  `Authorization: Bearer` headers minted from Application Default Credentials
  (refreshed in the background) plus `x-goog-user-project`. Resource
  attributes (`service.name`, `gcp.project_id`,
  `cloud.platform = gcp.agent_engine`) are detected from `K_SERVICE`,
  `GOOGLE_CLOUD_PROJECT`, and `GOOGLE_CLOUD_AGENT_ENGINE_ID`.
  `init_json_logging` emits Cloud Logging structured JSON (severity mapping,
  `logging.googleapis.com/trace` correlation). A collector-sidecar fallback is
  documented in `docs/official_docs/observability/gcp.md`.
### Fixed

- **Model pricing corrected across every provider table.** All rates are now the
  vendors' published standard-tier list prices, verified 2026-08-23, and each
  module records the source URL and verification date.
  - `adk-model` (OpenAI): every GPT-5.x rate was wrong, in both directions.
    `gpt-5` was overstated 2× on input, `gpt-5-mini` 2.4×, `gpt-5-nano` 3×;
    `gpt-5.5-pro` was understated 5× and `gpt-5.4-pro` 7.5×. `gpt-5`,
    `gpt-5-mini`, `gpt-5-nano`, `gpt-5.1`, `gpt-5.2`, `gpt-5.4`, `gpt-5.4-mini`,
    `gpt-5.4-nano`, `gpt-5.4-pro`, `gpt-5.5`, `gpt-5.5-pro`, `gpt-5-pro` and
    `gpt-5.3-codex` are corrected. `gpt-image-2` image output was $32, now $30.
  - `adk-gemini`: Gemini 2.5 Flash-Lite was documented as having no cache
    support and priced cache reads at $0, when Google charges $0.01/MTok.
    `cache_input_long` on 2.5 Flash and 3 Flash Preview held audio cache rates
    ($0.10) rather than long-context rates, inflating cached cost on long
    prompts by 2–3.3×; those models have no long-context tier and now report the
    base rate.
  - `adk-eval`: `default_pricing()` contained no current model and understated
    Gemini 2.5 Flash output 4× and 2.5 Pro output 2×. Replaced with the current
    Gemini, OpenAI, Anthropic and DeepSeek line-ups.
- **Gemini 2.0 shutdown date** corrected in `AGENTS.md` from March 31 2026 to
  June 1 2026, the date Google published.
- **Current provider request contracts are validated before dispatch.** Direct
  `adk-gemini` calls reject sampling, `candidate_count`, and token-based thinking
  settings removed by Gemini 3.7. Anthropic rejects restricted sampling and
  budget-based thinking on the current Claude families, and limits fast mode to
  Claude Opus 5 and Opus 4.8.
- **Gemini Live catalog lifecycle metadata is endpoint-aware.** The Vertex GA
  `gemini-live-2.5-flash-native-audio` remains active through December 2026,
  while the retired AI Studio `gemini-live-2.5-flash-preview` is rejected. The
  catalog also covers additional retired Gemini aliases and Groq's Qwen 3 32B
  deprecation.
- **Runnable examples and crate README quickstarts no longer target superseded
  provider defaults.** Gemini examples now use Gemini 3.7 Flash (or 3.5 Flash
  Lite for deterministic benchmarks), image examples use the GA Gemini 3.1
  Flash Image model, and provider-specific READMEs align with the shared model
  catalog.

### Changed

- **`adk_gemini::Model::default()` is now Gemini 3.7 Flash**, not Gemini 2.5
  Flash. This changes per-token cost for anyone relying on `Default`: 3.7 Flash
  is $0.75/$3.75 per MTok against 2.5 Flash's $0.30/$2.50, so input is 2.5× and
  output 1.5× the previous rate per token. Pin `Model::Gemini25Flash` explicitly
  to keep the old behaviour.
- The umbrella `provider_from_env()` and `run()` helpers now use the same shared
  Gemini and OpenAI catalog defaults as CLI-generated projects.
- **`lookup_pricing` and the new resolvers return `None` for models the vendor
  publishes no rate for**, rather than a fabricated rate. Five OpenAI constants
  (`GPT_55_INSTANT`, `GPT_52_CODEX`, `GPT_51_CODEX`, `GPT_51_CODEX_MAX`,
  `GPT_51_CODEX_MINI`), `GPT_53_CHAT_LATEST` and both deep-research constants are
  deprecated and no longer answer lookups. Treat `None` as unpriced, never free.
- Introductory Gemini rates are marked with their expiry: Gemini 3.7 and 3.6
  Flash double on 2027-01-01, and GPT-5.6 Sol's promotional rate runs at least
  to 2026-11-21.

### Added

- **`ModelPricing::for_model` / `for_model_id`** (`adk-anthropic`) and
  **`GeminiPricing::for_model_id`** (`adk-gemini`) resolve pricing from a wire
  model ID, including the `Custom(..)` identifiers returned by the model
  factories and Anthropic's dated aliases. Previously `adk-anthropic` had no
  model-to-pricing mapping at all and `adk-gemini` special-cased one ID.
- Anthropic Claude Mythos 5 and fast-mode rates (`ModelPricing::MYTHOS_5`,
  `OPUS_5_FAST`), Haiku 3.5, and a `Model::claude_mythos_5()` factory.
- Gemini 3.6 Flash and 3.5 Flash-Lite pricing plus a
  `Model::gemini_3_5_flash_lite()` factory. Gemini 3.6 Flash previously had a
  factory but no pricing entry, so it resolved as unpriced.
- OpenAI GPT-5.6 Cyber, GPT-5.5 Cyber, GPT-5.2 Pro, `chat-latest`,
  `gpt-5-search-api`, `gpt-5.3-codex` fast mode, and long-context constants for
  the GPT-5.4, GPT-5.5 and GPT-5.6 families.
- `OpenAIReasoningEffort` and additive Chat Completions / Responses constructors
  expose `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, and `max` without
  adding variants to the existing exhaustive `ReasoningEffort` enum.
- `scripts/check-model-pricing.sh` checks the encoded rates against the vendor
  pricing pages and runs in the monthly advisory model-freshness workflow.

### Changed

- **`provider_from_env()` now consults the Vertex opt-in flags before any API
  key.** A truthy `GOOGLE_GENAI_USE_ENTERPRISE` or `GOOGLE_GENAI_USE_VERTEXAI`
  (`1` or a case-insensitive `true`) selects Gemini on Vertex AI — via
  Application Default Credentials with `GOOGLE_CLOUD_PROJECT` and
  `GOOGLE_CLOUD_LOCATION` — even when `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or
  `GOOGLE_API_KEY` is set. Previously these flags were ignored and API-key
  sniffing alone decided the provider. If you already set a `GOOGLE_GENAI_USE_*`
  variable for another SDK (e.g. adk-python) and rely on `provider_from_env()`
  picking Anthropic or OpenAI from an API key, unset the flag or set it to a
  falsy value. `GOOGLE_GENAI_USE_ENTERPRISE` takes precedence when both flags
  are set. When a flag is truthy but the `gemini-vertex` feature is not
  compiled, `provider_from_env()` emits a `tracing` warning and falls through
  to API-key detection; a truthy flag with `GOOGLE_CLOUD_PROJECT` /
  `GOOGLE_CLOUD_LOCATION` missing is an error, not a Studio fallback.

### Added

- **`GeminiModel::from_env(model)`** — environment-driven construction: Vertex
  AI via ADC when `GOOGLE_GENAI_USE_ENTERPRISE` / `GOOGLE_GENAI_USE_VERTEXAI`
  is truthy (requires the `gemini-vertex` feature; errors when the feature is
  missing or the Vertex target is incomplete), otherwise the Gemini API via
  `GOOGLE_API_KEY` or `GEMINI_API_KEY`.
- **`adk_model::gemini::vertex_env_requested()`** — reports whether the
  environment opts in to the Vertex AI backend.
- New docs page
  [Vertex-Only Deployments](docs/official_docs/compliance/vertex-only-deployments.md)
  — guaranteeing no `generativelanguage.googleapis.com` traffic for HIPAA and
  data-residency workloads.
### Added

- **`VertexAiSessionConfig::from_env()`** builds the config from
  `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, and
  `GOOGLE_CLOUD_AGENT_ENGINE_ID` (the bare numeric engine ID) — the variables
  the Vertex AI Agent Engine platform sets inside deployed containers. Missing
  or blank variables produce an actionable invalid-input error naming each one.
- **Vertex session expiration**: `VertexAiSessionConfig::with_ttl()` and
  `with_expire_time()` send the `Session.expiration` oneof (`ttl` /
  `expireTime` from `google/cloud/aiplatform/v1beta1/session.proto`) on
  session create. Setting both members, or a TTL below the 24-hour minimum,
  fails at service construction.
- **adk-server**: `agent_skills_from_index` bridges an `adk_skill::SkillIndex` to A2A
  agent-card `skills[]` entries (skill name → `id` and `name`, description →
  `description`, tags → `tags`, version folded into `tags` as `version:{v}`).
  `ServerBuilder::with_skill_index` and `A2aServerBuilder::skill_index` attach an
  index so the card served at `/.well-known/agent.json` includes those entries —
  the surface Agent Registry keyword/prefix search indexes.
  `A2aController::with_skill_index` is the underlying constructor.
- **`POST /api/run` plain-JSON endpoint in `adk-server`.** Accepts the same body as
  `/api/run_sse`, runs the agent to completion, and returns the collected events as a
  JSON array — parity with Google ADK's `api_server` non-streaming `/run` route.

### Fixed

- **adk-rag: SurrealDB `create_collection` parse error.** The `metadata` field
  definition used the pre-3.2 modifier order `FLEXIBLE TYPE object`, which
  surrealdb 3.2 rejects with `FLEXIBLE must be specified after TYPE`. Reordered
  to `TYPE object FLEXIBLE`, and the `surrealdb` feature now has a PR-tier
  `feature-coverage` matrix entry so the backend cannot silently break again
  (#568).

### Changed

- **Wave 3 ADC/LRO consolidation** — the Vertex plumbing duplicated across
  five backends now lives in `adk-gcp`, with no public API change and each
  backend's error codes, categories, and mock contract tests preserved:
  `adk-session` (`vertex-session`), `adk-memory` (`vertex-memory`),
  `adk-tool` (`example-store`), `adk-deploy` (`gcp`), and `adk-artifact`
  (`gcs`, credential handling only).
- **adk-gcp**: `GcpErrorContext` gains `with_response_too_large_code` (a
  dedicated size-limit code override; the default remains the consumer's
  `invalid_response` literal); `GcpHttpClient` gains `send_value_counted`
  (parsed JSON plus decoded body size, for aggregate pagination bounds) and
  a post-construction `with_max_response_bytes` override.

### Contributors

Thank you to [@joseph-wortmann](https://github.com/joseph-wortmann),
[@1111mp](https://github.com/1111mp), and
[@jkmaina](https://github.com/jkmaina) for their contributions to v2.1.0.

## [2.0.0] - 2026-08-09

### Breaking

- **`DeferredNodeConfig` gained a public `min_predecessors` field.** Every struct
  literal needs `min_predecessors: None` to keep the previous behaviour, which is
  to release the join when all direct predecessors have arrived. `Some(n)` releases
  after *n* of them, for a quorum instead of a full join.
- **`CompiledGraph::get_next_nodes` returns `Result<Vec<String>>`.** A router
  answering with a key that is not among the declared targets previously stopped
  that branch and the run reported success with the target never executed. It now
  gives `GraphError::UnknownRouteTarget`, naming the key and listing the declared
  ones. A route to `END` stays legal, because `END` is declared.
- **`CompiledGraph::time_travel` returns `Result<TimeTravelHandle>`.** It used to
  panic when the graph had no checkpointer. Add `?` at the call site.

- **`ConnectionRefresher::call_tool` and `SimpleClient::call_tool` return
  `CallToolResponse`** instead of `CallToolResult`. SEP-2663 lets a server answer
  `tools/call` with a task, and SEP-2322 with a request for more input, so the
  response now says which of the three happened. Both wrappers moved to rmcp's
  `call_tool_once`: the `call_tool` helper fulfils input rounds through the local
  handler on its own and rejects a task response outright, which would break every
  server that materializes one. `McpToolset::execute` handles all three cases
  internally, so agents built on `McpToolset` need no change.
- **Task execution now needs the extension declared at handshake time.** SEP-2663
  moved the declaration from the request to the client's capabilities: a server
  must not answer with a task unless the client declared
  `io.modelcontextprotocol/tasks`. Call
  `AdkClientHandler::with_tasks()` when building the client;
  `McpToolset::with_task_support` continues to set how the client polls. Under
  rmcp 2.2 each call carried its own task metadata, so no handshake declaration
  was needed. `examples/mcp_protocol_revisions` demonstrates both.
- **A tool no longer declares its own task contract.** SEP-2663 removed the
  per-tool signal, so `Tool::is_long_running` on an MCP tool now answers per
  connection: true when tasks are enabled and the server negotiated them. A
  `CodeActAgent` that suspends on long-running tools suspends for any tool on a
  task-capable server rather than only those that declared support.
- **`rmcp::model::SamplingMessageContent` is now `SamplingMessageContentBlock`**
  under the `mcp-sampling` feature. `adk-tool --features mcp-sampling` joins the
  PR-tier feature-coverage matrix; nothing in the workspace enabled it, so its
  only cover was an example crate.

### Changed

- **A graph's default `recursion_limit` is 100, up from 50.** `LlmAgent`'s tool loop
  already allowed 100, so a graph stopped at half the budget an agent got, for no
  stated reason. A cycle now runs twice as far before
  `GraphError::RecursionLimitExceeded`.
- **`RetryPolicy::default()` allows ten attempts, up from one.** One attempt made
  the default a no-op. Ten attempts sleep about 243 seconds in total, so a node that
  keeps failing takes roughly four minutes to give up; lower `max_attempts` where a
  caller is waiting. Retry is still opt-in: a node with **no** policy runs once.

### Added

- **`adk-graph` parity and reliability work.** Each item is off by default, so an
  existing graph behaves as it did:
  - **Routing from inside a node.** `NodeOutput::with_goto` names the successors,
    replacing that node's declared edges, so a node reaches a node it has no edge
    to. `AgentNode::with_goto_mapper` derives the route from the updates its output
    mapper produced, so an agent routes on its own answer. Naming `END` stops the
    branch; an unknown name fails the run.
  - **Per-node retry.** `RetryPolicy` with capped exponential backoff and jitter,
    attached by `with_node_retry`. An interrupt is never retried. The attempt count
    is checkpointed, so a resumed run continues the budget.
  - **A concurrency bound.** `with_max_concurrency` caps how many nodes run at
    once, admitting the frontier in sorted order.
  - **Invoking a node directly.** `ctx.run_node_with(name, input, options)` runs a
    node the graph has no edge to, sized from state. Completed children are
    recorded under `<parent>/<child>@<run_id>`, so a resume returns the recorded
    answer instead of paying for the child again.
  - **A graph or a node as a tool.** `NodeTool::for_graph` and `for_node` expose
    either through the `Tool` trait, reporting long-running so a graph pause travels
    the existing tool-confirmation path.
  - **An *n*-of-*m* join.** `DeferredNodeConfig::min_predecessors` releases a join
    on a quorum.
  - **Channel enforcement.** `with_strict_channels` fails the run when a node writes
    a channel the schema does not declare, which otherwise took the overwrite
    reducer silently.
  - **`StreamEvent::NodeInterrupt`.** A node reporting a pause on the streamed
    path. The executor converts it into the pause and does not forward it, so a
    caller still sees only `Interrupted`.
  - **Subgraphs.** `SubgraphNode` runs a compiled graph as a node, exchanging
    named channels. A pause inside pauses the parent. A channel mapping naming a
    channel neither side declares fails when the parent compiles, through a new
    `Node::validate_against(&parent_schema)` that `compile()` calls for every node.
  - **`NodeOutput::with_goto_parent`.** A node inside a subgraph ends its own graph
    and names a node of the graph that holds it, read from the new
    `CompiledGraph::invoke_detailed`. The counterpart to LangGraph's
    `Command(graph=Command.PARENT)`.
  - **`CompiledGraph::with_node_defaults`.** One retry, timeout or failure handler
    for every node that sets none; a per-node value wins. A graph-wide
    `default_timeout` already existed on `GraphAgentBuilder`.
  - **`CompiledGraph::with_node_error_handler`.** Once a node's retry budget is
    spent, a handler may record the failure and name a recovery node instead of
    ending the run. An interrupt never reaches it.
  - **Checkpoint retention.** `CompiledGraph::with_checkpoint_retention` bounds how
    many checkpoints a thread keeps, by count, by age, or both, pruned after each
    save. The newest is never discarded, because it is the one a resume loads.
    `Checkpointer::prune` has a default that keeps everything, so a custom backend
    is unaffected. A long-running thread previously grew without bound; LangGraph
    documents the same growth and advises an external cron job.
  - **Background runs survive a restart.** `adk-server`'s `RunStore` held runs in
    an in-memory map, so graph state survived a restart through a checkpointer but
    the list of runs did not, and a restarted server could not report what had been
    in flight. `RunPersistence` records them, `FileRunPersistence` writes one JSON
    file through a temporary and a rename, and `RunStore::restore` loads them at
    startup — reporting any run that was `Running` or `Queued` as `Failed`, because
    it cannot still be running. A restored run gets a live cancellation token. A
    networked backend is a follow-up; the trait is the seam for one. Finished runs
    are bounded by default — `RunRetention` keeps the newest 1000 and discards the
    rest from both the map and the records, because a store that persists every
    finished run forever is a leak that only appears after weeks. A run still in
    flight is never discarded.
  - **`FileManagedStateStore`.** The first `ManagedStateStore` reporting
    `Durability::CrashDurable`, so a managed session can be reconstructed after
    process loss. One JSON file per session, synced and renamed, so an acknowledged
    write is persisted and a reader never sees a partial snapshot. Session ids are
    escaped, so an id containing a path separator cannot write outside the root.
    `InMemoryManagedStateStore` remains, and still reports `ProcessLocal`.
  - **Umbrella features.** `graph-functional`, `graph-node-cache`, `graph-delta`,
    `graph-time-travel`, `graph-sqlite`, and `graph-redis-cache` on `adk-rust`
    forward to `adk-graph`, which has no default features. The first four are in
    `full`.
- **`gemini-agent-platform` / `gemini-agent-platform-full` umbrella meta-features.** One
  switch that pulls in every Gemini Enterprise Agent Platform (Vertex/EAP)
  integration, composable with any tier preset:
  `features = ["standard", "gemini-agent-platform"]`. The base variant covers
  `gemini-vertex`, `vertex-session`, and `gcp-secrets` (growing as later
  platform integrations land) and excludes realtime transports — the right
  default for ReasoningEngine BYOC deployments. `gemini-agent-platform-full` adds
  `vertex-live` (Vertex AI Live API, which pulls in the adk-realtime stack).
  Deploy-time tooling is host-side and excluded from both.

### Changed

- **MCP moves to the official `rmcp 3.1` SDK.** The client still advertises MCP
  `2025-11-25`, so every existing server is unaffected: `rmcp 3.1` keeps
  `ProtocolVersion::LATEST` at `2025-11-25`, and a `2026-07-28` server answers
  the same handshake. `2026-07-28` adds a stateless `server/discover` handshake,
  now selectable per connection through `adk_tool::mcp::ClientLifecycleMode`. It
  stays opt-in because the SDK falls back to the legacy handshake only when a
  server refuses the probe with `METHOD_NOT_FOUND`, and applies no timeout to it.
  A new `adk-tool/tests/mcp_protocol_compatibility_tests.rs` holds the contract
  across every revision the SDK knows: the default path must send `initialize`
  first and advertise `2025-11-25`; a server pinned to `2024-11-05` stays
  reachable; and `Discover` and `Auto` settle on `2026-07-28` against a server
  that supports it. Two further tests run against external servers and are
  `#[ignore]`d by default.
  Closes #552.


### Fixed

- **`StreamMode::Messages` ignored every interrupt and wrote no checkpoint.** That
  mode runs nodes in its own loop to forward tokens as they arrive, and both
  interrupt checks lived in `execute_super_step`, which the loop never calls. So
  `interrupt_before`, `interrupt_after`, and a node's own
  `NodeOutput::interrupt` were all silently skipped — an approval gate did not
  hold — and a completed run left nothing to resume from. The checks now live in
  `gate_before`/`gate_after`, which both paths call, a node's pause travels as
  `StreamEvent::NodeInterrupt` because that path yields events and no
  `NodeOutput`, and the loop checkpoints each super-step. `GraphAgent`'s
  `Agent::run` uses `invoke` and was never affected.
- **A static interrupt could not be resumed past.** `interrupt_before` re-armed on
  every resume, so the gated node never ran. `interrupt_after` had the same defect
  and needed the opposite fix, because that node has already applied its updates.
  A `cleared_interrupt` marker is now checkpointed, with a SQLite column, so the
  durable backend keeps it too.
- **A fan-in node ran once per arriving branch.** Branches of unequal length made
  the join fire more than once. A node with more than one incoming direct edge is
  now deferred automatically at compile time. Conditional predecessors are excluded
  from the count, because a branch that never fires would stall the join.
- **Parallel state updates depended on completion order.** Two nodes appending to
  the same channel produced a different array depending on which finished first.
  Updates now apply in sorted node order, and by sorted key within a node.
- **An interrupt's data did not reach a caller through `GraphAgent`.** The pause is
  now carried as a JSON payload under one reserved `provider_metadata` key, with
  `GraphInterruptPayload::from_event` to read it back.
- **`StreamEvent::RouteDispatched` was never emitted.** The debug stream now reports
  one per conditional edge.


- **`adk-memory --features database-memory` compiles again.** `pgvector` accepts
  `sqlx >= 0.8, < 0.10` and resolved to 0.9 while the workspace pinned 0.8, so
  two semver-incompatible `sqlx` majors sat in one graph and `pgvector::Vector`
  implemented the other `sqlx::Type`. The lockfile now holds a single `sqlx`
  0.8.6. `adk-rag --features pgvector` and the three sqlx-backed backends join
  the PR-tier feature-coverage matrix, which no job had built.
- **Inline and file content metadata survives session persistence.**
  `Part::InlineData`, `Part::FileData`, `Part::FunctionResponse`, and multimodal
  function-response parts retain optional annotations; inline data also retains
  its optional source URI. ACP image conversion now round-trips both fields, and
  Vertex sessions route metadata-bearing content through lossless `rawEvent`
  persistence. Existing payloads continue to deserialize with empty metadata.
- **MCP tool annotations now control per-tool safety metadata.** Discovered
  `readOnlyHint` and `idempotentHint` values flow into ADK tool metadata,
  automatic dispatch, and reconnect replay decisions. Tools without hints keep
  the conservative sequential, no-replay defaults.
- **Schema caches keep adapter results isolated.** `SchemaCache` binds one
  `SchemaAdapter` instance at construction, so the same input schema cannot
  return a result produced by another provider or adapter configuration. The
  deprecated per-call adapter API also keys entries by normalized output to
  preserve correctness during migration.
- **`LlmAgent` skill injection preserves prompt-cache prefixes.** Contextual
  skills are injected into the current user turn instead of leading the request,
  so stable instructions and prior conversation history remain reusable by
  provider prompt caches across turns.
- **adk-sandbox's optional dependencies are no longer scoped to Unix.** Every
  optional dependency sat below a `[target.'cfg(unix)'.dependencies]` header, so
  `wasmtime` and `wasmtime-wasi` were unix-only and the `wasm` feature could not
  build on Windows, while `windows-sys` — declared for the AppContainer path —
  could never be selected on any platform. The optional dependencies move back to
  `[dependencies]` and `windows-sys` gets a `cfg(windows)` block.
- **The workspace builds clean on Windows.** Three `collapsible_if` violations
  failed `clippy -D warnings` in code paths no CI job compiled: one in
  `cargo-adk` behind `#[cfg(not(unix))]`, two in `adk-rust`'s `run()` that only
  compile at the `standard` tier and above. An `unneeded_return` in
  `adk-sandbox`'s Windows enforcer surfaced once the manifest fix made that
  module compile.
- **adk-bench's external-runner tests run on a stock Windows host.** They invoked
  `sh` and a bare `echo`, neither of which a plain Windows install provides, and
  passing a shell one-liner as an argument corrupted any embedded JSON because
  `cmd.exe` does not implement `CommandLineToArgvW` escaping. The tests now write
  a temporary script — PowerShell on Windows, `sh` elsewhere — so no payload
  crosses the command line.
- **Sandboxed Rust compilation discovers the Windows SDK outside a Developer
  shell.** `ProcessBackend` now obtains the MSVC `PATH`, `LIB`, `LIBPATH`, and
  `INCLUDE` values from the installed Build Tools when the parent environment
  does not provide them. The compiler receives the SDK import-library paths while
  the generated program still starts with a cleared environment. The exact
  compile-and-run regression now runs in the PR-tier Windows smoke gate.
- **adk-sandbox now preserves Windows shell quoting and selects the intended Rust
  linker.** Raw command text reaches `cmd.exe` without `CommandLineToArgvW`
  escaping, so quoted executable and script paths work. Direct sandboxed Rust
  compilation uses the toolchain's `rust-lld` instead of accidentally resolving
  Git's unrelated GNU `link.exe` from `PATH` on hosted Windows runners.

### Added

- **adk-code: in-process Python execution via Pydantic Monty** (`embedded-python`
  feature). One `MontyExecutorBuilder` produces two `CodeExecutor` products:
  `MontyOneShotExecutor` (fresh interpreter per call) and `MontyReplExecutor`
  (variables, functions, and imports persist across calls, with
  `start`/`stop`/`restart` lifecycle). OS access is host-granted at
  construction — filesystem mounts (read-only/read-write), an explicit
  environment map, and the clock — and serviced in place; ungranted access
  raises a catchable in-script `OSError`. Monty has no network or subprocess
  surface at all. Registered `HostFunction`s (sync or async) become callable
  Python functions; the drive loop segments across `spawn_blocking` so async
  host functions are awaited without holding interpreter state across `await`.
  The per-request `SandboxPolicy` may only narrow within the grants — a grant
  covers its entire directory subtree (a granted mount or any subdirectory of
  one may be requested), and requests exceeding the grants are rejected
  fail-closed with `UnsupportedPolicy`. Mount virtual paths are validated at
  build time (normalized absolute, unique). Host-side buffers are bounded:
  `print()` output is capped at `max_stdout_bytes` *during* the drive, and
  JSON↔Monty conversion is depth-capped so iteratively built deep nesting
  degrades to a placeholder instead of overflowing the host stack.
  `SandboxPolicy::strict_python()` added.
- **adk-code: `CodeExecutor::prompt_snippet()`** — a new defaulted trait method
  (default `None`, backward compatible) letting backends describe their built
  execution environment for LLM-facing tool descriptions. Both Monty executors
  implement it: mode semantics, granted mounts, environment variable names
  (never values), clock availability, and Python stubs for registered host
  functions.
- **adk-tool: `MontyPythonCodeTool`** (`monty_python_code`, feature
  `code-embedded-python`) — the agent-facing tool over the Monty executors,
  complementing the container-backed `PythonCodeTool` (which remains unchanged
  for full-CPython workloads: pip packages, C extensions, the complete
  standard library). One-shot mode shares a stateless executor; REPL mode keys
  interpreter sessions by the full ADK session identity (app, user, session
  id) with an LRU cap (default 100). The
  tool description is composed from the executor's `prompt_snippet()`, the
  schema is mode-aware (`reset` exists only in REPL mode), and
  `MontyPythonCodeTool::builder()` forwards grants, host functions, and
  limits. The umbrella crate forwards the feature as `code-embedded-python`.
  New standalone example: `examples/monty_python_code_tool`.
- **adk-codeact-monty is now publishable** (Experimental tier). The Python
  `CodeRuntime` for the `CodeActAgent` joins the crates.io release train at the
  workspace version, so `CodeActAgent` + Python is reachable from a published
  dependency instead of a git checkout. Its duplicated Monty plumbing is gone:
  the crate now consumes `adk-code`'s `embedded-python` integration kernel —
  shared JSON↔Monty conversion (`json_to_monty`/`monty_to_json`), shared
  OS-call servicing (`resolve_os_call`), a shared `PathAccess`, the shared
  `pathlib` capability listing, and re-exports of the `monty`/`monty-types`/
  `monty-fs` crates — so the Monty release is pinned exactly once, by
  `adk-code`. Behavior change: an ungranted clock now raises a catchable
  `OSError` (previously `RuntimeError`), matching the executors.
- **adk-rust: `codeact` and `codeact-monty` umbrella features.** `codeact`
  forwards `adk-agent/codeact` (the `CodeActAgent` was previously unreachable
  through the umbrella); `codeact-monty` adds the `adk-codeact-monty` runtime
  and re-exports it as `adk_rust::codeact_monty`. Both are opt-in specialist
  features, composable with any tier — like `code-embedded-python`, they are
  not part of `full`. The docs.rs build now enables the Monty opt-ins so their
  modules appear in the umbrella documentation.
- **adk-rust: `code-tools`, `code-embedded-js`, and `code-docker` umbrella
  features — and `full` now includes `code-tools`.** Previously no tier enabled
  `adk-tool/code`, so the code-execution tools (`CodeTool`, `PythonCodeTool`,
  `JavaScriptCodeTool`, `FrontendCodeTool`, `MontyPythonCodeTool`) were
  unreachable through the umbrella even on `full`, which compiled both
  `adk-code` and `adk-tool` but not the integration layer between them.
  `code-tools` forwards `adk-tool/code` (with `code` and `sandbox`, since
  constructing `CodeTool` takes an adk-sandbox backend); `code-embedded-js`
  and `code-docker` forward the embedded-JS and persistent-Docker live paths,
  completing the family alongside `code-embedded-python` (which now implies
  `code-tools`).
- **adk-realtime: `DisconnectReason`, so a closed stream can say why.**
  `RealtimeSession::disconnect_reason()` reports the close code and reason the
  provider sent, and `RealtimeRunner::disconnect_reason()` forwards it. The
  Gemini session records its close frame before the event stream ends.

  Applications that *poll* `RealtimeRunner::next_event()` never reach the
  runner's `on_disconnect` dispatch, and `next_event` returns a bare `None`
  whether the provider deliberately closed an idle session or the socket died.
  Both therefore landed in the caller's terminal record as the same generic
  stream failure — one that reads like a network defect when it was a policy
  close. Google's Live API closes an idle session with `1008` and
  `"The operation was aborted."`; that string was reachable in logs but nowhere
  a durable record could use it.

  The trait method is defaulted to `None`, so existing `RealtimeSession`
  implementations are unaffected and this is not a breaking change.

- **`scripts/setup-dev.ps1` — first-time setup for Windows.** `make setup`,
  `scripts/setup-dev.sh`, and `devenv shell` all require a POSIX shell, so
  Windows had no scripted path. Checks the toolchain and MSVC environment,
  installs `sccache`, `cargo-nextest`, `protoc`, and NASM, verifies `bash` and
  `python3` resolve, registers lefthook, and persists `RUSTC_WRAPPER`,
  `CMAKE_POLICY_VERSION_MINIMUM`, and `PROTOC`. `-Check` reports without
  changing anything.
- **`scripts/setup-dev.sh` now checks `cargo-nextest` and `protoc`.** Both are
  required by gates that CI installs for itself, so a missing local copy failed
  the build while CI stayed green.
- **PSScriptAnalyzer pre-commit gate.** `scripts/lint-powershell.ps1` backs a new
  lefthook hook for staged `*.ps1`, the PowerShell counterpart to the shellcheck
  gate. Skips itself when the module is not installed.

### Security

- **Wasmtime is updated to 46.0.3 and Monty to 0.0.21.** Wasmtime resolves
  RUSTSEC-2026-0222 and RUSTSEC-2026-0223. Monty moves to `jiter 0.16` and
  PyO3 0.29, resolving GHSA-36hh-v3qg-5jq4 and GHSA-chgr-c6px-7xpp even though
  PyO3 remains behind jiter's disabled `python` feature. The Monty update also
  brings descriptor-based mount confinement and fixes for Windows mount escape
  and cached-overlay path revalidation. The supply-chain license policy
  recognizes Monty's OSI-approved Unicode-DFS-2016 data license.
- **Example web interfaces no longer build DOM from untrusted HTML.** The
  realtime voice example renders provider, memory, and pipeline data with DOM
  text nodes, while the streaming bash example uses a cryptographic session ID
  and a `Map` for untrusted tool-call keys. This resolves the five open CodeQL
  XSS, prototype-pollution, and insecure-randomness findings.
- **The audio `fx` feature drops unused `rubato` and `dasp` dependencies.** Its
  processors already use ADK-Rust's internal bounded implementations and never
  called either crate, so the public feature and behavior are unchanged while
  the unnecessary dependency surface is removed.
- **HTTP and cloud SDK dependencies are refreshed for RUSTSEC-2026-0258.** The
  active HTTP/2 stack now uses `h2 0.4.19`; Azure Identity is aligned with the
  0.22 Azure Core generation already used by Key Vault, and AWS Secrets Manager
  no longer enables its legacy Hyper 0.14 TLS transport. The lockfile-only
  `rkyv 0.7` advisory is documented as unreachable because no workspace feature
  enables `rust_decimal`'s optional archive integration.

### Changed

- **PostgreSQL vector memory remains aligned with SQLx 0.8.** `pgvector` is
  pinned to `0.4.1`, the latest release whose SQLx integration uses the
  workspace's SQLx 0.8 line. `pgvector 0.4.2` moved that integration to SQLx
  0.9 and is intentionally excluded until the workspace upgrades SQLx.
- **adk-codeact-monty joined the root workspace.** Monty is on crates.io since
  `0.0.19`, so the crate's git dependency (and the empty `[workspace]` table it
  forced) is gone: it now depends on `monty`, `monty-types`, and `monty-fs`
  `0.0.21` from crates.io and is covered by the standard workspace gates
  (`clippy`, `nextest`, docs) on every PR. The dedicated out-of-workspace CI
  (`codeact-monty.yml`, the `out-of-workspace-monty` merge-tier job) is retired,
  and `examples/codeact_monty_agent` compiles in the PR-tier examples gate. The
  workspace `Cargo.lock` pins `get-size2` to `0.10.1` — `monty 0.0.21` pulls
  `ruff_python_ast 0.0.3`, which derives `GetSize` on `compact_str 0.9` fields
  while `get-size2 0.10.2+` moved to `compact_str 0.10`; the pin keeps the two
  aligned until monty upgrades past `ruff_python_ast 0.0.3`. Porting to the
  0.0.21 API: `MontyRuntimeBuilder::max_allocations` is removed (Monty's
  `ResourceLimits` no longer counts allocations — the time/memory caps remain),
  and per-step `print()` capture is capped at Monty's 10 MiB collector default
  (exceeding it raises `MemoryError` in the script). The crate is published on
  the workspace release train.

### Breaking

Major version bump. The complete list of public API breakage since
1.0.0, as reported by `cargo semver-checks check-release --release-type minor`
against the published 1.0.0 baseline, is below. A migration guide with
before/after code is in
[docs/official_docs/migration/1.0-to-2.0.md](docs/official_docs/migration/1.0-to-2.0.md).

| Crate | Change | Downstream impact |
|---|---|---|
| `adk-acp` | `PermissionDecision::Allow` **removed**, replaced by `Select(String)`, `AllowOnce`, and `AllowAlways` | Code constructing or matching `Allow` must pick the intended variant |
| `adk-acp` | New public fields: `PermissionRequest::{session_id, tool_call_id, kind, raw_input}`, `AcpAgentConfig::{mcp_servers, filesystem, terminal}`, `PermissionOption::kind` | Struct literals must add the fields; prefer `..Default::default()` |
| `adk-acp` | New variants: `OutputChunk::{ToolUpdate, Usage}`, `PermissionPolicy::AsyncCustom` | Exhaustive `match` must add arms |
| `adk-acp` | `AcpAgentConfig` no longer `UnwindSafe`/`RefUnwindSafe` | Affects code storing it across `catch_unwind` |
| `adk-computer-use` | `ComputerUseRuntime::verify` takes the action postcondition and returns `VerificationOutcome` instead of `bool` | Match the outcome; `is_verified()` is true only when the postcondition was observed, `is_committed()` covers "performed but unverified" |
| `adk-agent` | `TriggerEvent` gains a public `principal` field | Add `principal: None` to struct literals; webhook events carry the verified principal |
| `adk-agent` | `WebhookTrigger` binds loopback by default, requires a verifier for any wider address, and rejects non-JSON bodies | Call `with_bind_address` plus `with_verifier` to expose it; `accept_non_json()` restores the old body handling |
| `adk-agent` | `WebhookTrigger` no longer `UnwindSafe`/`RefUnwindSafe` | It now holds a verifier behind `Arc<dyn ...>`; affects code storing it across `catch_unwind` |
| `adk-managed` | `ManagedAgentRuntime::start_session` requires a `ManagedOwner`, and rejects an `EnvironmentConfig` it cannot honour | Pass an owner; supply `None` for `env` unless a sandboxed runtime is configured |
| `adk-anthropic` | New variants: `ContentBlock::WebFetchToolResult`, `ToolUnionParam::WebFetch20250910`, `ServerTool::WebFetch20250910` | Exhaustive `match` must add arms |
| `adk-core` | New public fields: `RunConfig::{tool_confirmation_handler, runtime_toolsets}` | Struct literals must add the fields; prefer `RunConfig::builder()` |
| `adk-core` | New variant `Part::EmbeddedResource` (`Part` is not `#[non_exhaustive]`) | Exhaustive `match` must add an arm |
| `adk-core` | `RunConfig` and `RunConfigBuilder` no longer `UnwindSafe`/`RefUnwindSafe` | Affects code holding them across `catch_unwind` |
| `adk-core` | `Memory::search_in_project` and `Memory::add_to_project` now return an error by default instead of silently operating globally; new `Memory::supports_project_scoping` | A custom `Memory` that relied on the fallback must implement the project methods or accept the error |
| `adk-memory` | `MemoryService::{add_session_to_project, add_entry_to_project, delete_entries_in_project}` now return an error by default; new `MemoryService::supports_project_scoping` | A custom backend must implement them; `GraphMemoryService` now refuses project calls it previously answered globally |
| `adk-core` | `RunConfig::tool_confirmation_decisions` is now keyed by **function call ID** instead of tool name | Approvals keyed by tool name are no longer found, so the call stays unconfirmed; key by `ToolConfirmationRequest::function_call_id` |
| `adk-core` | New public field `RunConfig::tool_confirmation_fingerprints` | Struct literals must add the field; prefer `RunConfig::builder()` |
| `adk-graph` | `TimeTravelHandle::replay` renamed to `state_history` | Rename the call; behaviour is unchanged, and it never re-executed anything despite the old name |
| `adk-graph` | New public field `ExecutionConfig::parent_context` | Struct literals must add the field; prefer `ExecutionConfig::new` plus `with_parent_context` |
| `adk-graph` | New public field `StateGraph::deferred_configs` | Struct literals must add the field |
| `adk-runner` | `MutableSession::conversation_history_for_agent_impl` now takes two parameters (an `agent_name` and a `branch`) instead of one | Direct callers must pass the invocation branch; pass `""` for unscoped behaviour |
| `adk-realtime` | `ClientEvent` and `ServerEvent` are now `#[non_exhaustive]` | Downstream `match` needs a wildcard arm; the enums can no longer be constructed exhaustively outside the crate |
| `adk-realtime` | `ServerEvent::Unknown` discriminant changed 21 → 23 | Affects code depending on the numeric discriminant |
| `adk-realtime` | New public field `RealtimeConfig::affective_dialog` | Struct literals must add the field |
| `adk-sandbox` | `EnforcedLimits::filesystem_isolation` replaced by `filesystem_write_isolation` and `filesystem_read_isolation` | Read the field that matches what you need; the two are not equivalent on macOS |
| `adk-telemetry` | `AdkSpanLayer::new` now takes one generic type parameter instead of none | Call sites passing explicit generics must be updated |
| `adk-telemetry` | `AdkSpanLayer` no longer `UnwindSafe`/`RefUnwindSafe` | Affects code holding it across `catch_unwind` |

### Security

- **adk-computer-use: the reference graph now enforces execution-time bindings and
  deterministic reservation cleanup.** Generic `ComputerUseRuntime` implementations could
  return a well-formed lease, reservation, or receipt without the graph applying the binding
  validators used by the MCP adapter. The graph now validates each response before storing it
  and revalidates the envelope, lease, reservation, approval route, action digest, and policy
  digest immediately before the single mutation. Reservations bind the exact action intent,
  active state, expiry, and app/window scope. Every post-reservation terminal path attempts
  release; a primary failure remains primary, and a cleanup failure is reported alongside it.
  The checkpointed preview is append-only, so resume input cannot replace it while supplying a
  matching forged approval. Verification now reads the postcondition from the stored preview
  envelope rather than a nonexistent `envelope` state channel. The MCP adapter also retains the
  exact preview envelope and rejects direct execution if it changes.

- **adk-sandbox: the process output cap now bounds memory instead of only the report.**
  `ProcessBackend::execute` called `child.wait_with_output()`, which buffers a process's entire
  stdout and stderr, and applied the 1 MiB `MAX_OUTPUT_BYTES` limit afterwards. A sandboxed
  process writing gigabytes allocated gigabytes; the cap limited only what was returned. Both
  pipes are now read concurrently with the limit applied as bytes arrive. Reading continues past
  the cap and discards the excess, because stopping would block the child on a full pipe until
  the execution timeout. Truncation is logged.

- **adk-computer-use: lease validation enforces expiry, remaining budget, and target
  boundaries.** `validate_lease` checked session, principal, agent, execution mode, and
  `state == "active"`, plus `action_budget == 0` — the *total* budget, so a lease with
  `action_budget: 1, actions_used: 1` passed while authorizing nothing. It never read
  `expires_at` or `boundaries`, so an expired lease and a lease scoped to a different application
  were accepted as well. All three are now checked, and an expiry that cannot be parsed is
  rejected rather than skipped.
- **adk-awp: subscription HMAC secrets no longer leave the process.** `EventSubscription`
  serialized `secret` as a plain field and `GET /awp/events/subscriptions` returned
  `Json(subs)`, so listing subscriptions disclosed every subscriber's signing key — on an
  endpoint with no authentication, meaning anyone who could reach it could collect the keys and
  forge signed webhook deliveries. The field is now `skip_serializing`, and a hand-written
  `Debug` redacts it so capturing a subscription in a `tracing` call cannot write a live
  credential to the logs. The secret remains available in-process for signing.
- **adk-awp: public execution and management boundaries now fail closed.** `/awp/a2a` returned
  `200 acknowledged` without dispatching a message, so callers were told work succeeded when no
  agent ran. `AwpA2aHandler` now supplies application dispatch; the unconfigured endpoint returns
  `503`, message IDs and bodies are bounded, and public routes apply the configured rate limiter.
  `DefaultTrustAssigner` no longer treats an unverified authorization header as known identity.
  `awp_routes` now returns only public routes; subscription CRUD requires the explicit
  `awp_management_routes` router and an application auth layer. Malformed version headers return
  `400` instead of silently becoming the current version. Subscription configuration now requires
  bounded fields, HTTPS callback URLs, and signing secrets of at least 32 bytes. The no-op
  `webhook-delivery` feature is removed; the in-memory service signs and logs deliveries, while
  production HTTP delivery stays behind the `EventSubscriptionService` interface.
- **adk-server: A2A JSON-RPC routes now sit behind the configured authentication layer.**
  `/a2a` and `/a2a/stream` were merged at the router root, outside the layer applied to `/api`,
  in both `create_app_with_a2a` and `ServerBuilder::build`. A deployment that authenticated every
  other mutation surface still allowed any client that could reach the port to drive the agent,
  call its tools, and incur the cost. Discovery (`/.well-known/agent.json`) stays public, since
  peers fetch the card before they hold a credential, and with no extractor configured the routes
  remain open so existing deployments are unaffected.
- **adk-server: `A2aServer` binds loopback by default.** The builder defaulted to
  `0.0.0.0:8080`, publishing an agent-executing server to every interface on `build()`. It now
  defaults to `127.0.0.1:8080`; `bind_addr` opts into a wider bind. The generated `a2a-server`
  scaffold does the same and reads `BIND_HOST`.
- **adk-server: `--features a2a-v1` and `--all-features` compile again.** Two test initializers
  for `RemoteA2aV1Config` omitted the `streaming` field, so both configurations failed to build —
  which also meant the A2A v1 surface had no executing test coverage.

- **adk-tool: MCP tool-call replay now requires explicit opt-in.** A transport
  failure after request transmission can leave a mutating tool's external
  result uncertain. `ConnectionRefresher` and `McpToolset` no longer replay
  `tools/call` by default after reconnecting; callers may opt in with
  `with_tool_call_retries()` for read-only or provider-idempotent operations.
  Discovery and resource retries retain their existing reconnect behavior.
- **adk-computer-use: security-relevant MCP responses are bound to the request that produced
  them.** `ControlLease`, `TargetReservation`, and `ExecutionReceipt` were deserialized and
  returned straight into graph state. Typed deserialization proves shape, not provenance, and
  none of these structs has an invariant-enforcing constructor, so a well-formed object
  belonging to another session, principal, agent, mode, or action parsed cleanly and was
  accepted. Each is now validated against the requesting envelope — including active lease
  state, remaining action budget, and the receipt's `action_digest` against the envelope's
  approval-bound `args_digest` — and a mismatch raises
  `ComputerUseError::IdentityMismatch` naming the field. The external runtime remains
  authoritative; this is the local defense that stops a stale or confused response from
  propagating.
- **adk-computer-use: verification no longer equates "committed" with "verified".** `verify`
  returned `receipt.status == ReceiptStatus::Committed`, collapsing two distinct claims: that
  the runtime performed the action, and that the intended effect occurred. A committed action
  whose effect did not happen was reported as completed, from the node the reference graph
  labels "verify". `verify` now receives the envelope's declared `ActionPostcondition` — which
  the old signature could not even see — and returns `VerificationOutcome::Verified`,
  `CommittedUnverified { reason }`, or `Failed { reason }`. Verification requires evidence on
  the receipt bound to the postcondition's digest; absence of evidence is reported as
  committed-but-unverified rather than treated as success. The graph writes `verified`,
  `committed`, and `result.verificationDetail` separately.
- **adk-agent: `WebhookTrigger` has a trust boundary and a bounded lifetime.** The trigger
  bound `0.0.0.0:<port>` and accepted every POST on its path — no signature check, no
  authentication hook, no principal, no body policy — so any caller who could reach the port
  could start application-defined agent work, and a malformed body was wrapped as a JSON
  string and delivered as a trigger event indistinguishable from a deliberate one. It now
  binds loopback by default, and serving any wider address requires a `WebhookVerifier`;
  subscribing without one fails with `agent.ambient.webhook_unauthenticated` rather than
  exposing an open trigger. Verified requests carry their principal on
  `TriggerEvent::principal`. Rejections return a bare `401` with the reason logged, so the
  endpoint cannot be used to probe which part of a credential was wrong. Bodies are capped
  (1 MiB by default, `with_max_body_bytes`) and non-JSON bodies are rejected with `400`
  unless `accept_non_json()` is set.

  The HTTP listener also outlived its consumer: `axum::serve` was spawned with no shutdown
  signal, so dropping the event stream left the port bound, accepting requests it could not
  deliver and blocking a restart on the same port. The server's lifetime is now tied to the
  subscription stream, which shuts it down gracefully on drop.
- **adk-realtime: schema-drift warnings no longer log raw provider frames.** When a
  recognized OpenAI realtime event failed to deserialize, the warning logged the first 300
  bytes of the raw WebSocket text with no payload-recording opt-in and no redaction.
  Realtime frames carry transcripts, tool arguments, tool results, and identifiers, so
  provider schema drift could push conversation content into warning logs — at exactly the
  moment operators widen log collection. The warning now reports `event_type`, the parse
  error, `payload.bytes`, and a correlation `payload.digest`, with `payload.raw` set to
  `<redacted>`. The new `record-payloads` feature on `adk-realtime` restores bounded raw
  recording for diagnosis, matching the flag `adk-agent` already uses for trace payloads.
- **adk-devtools: `bash` no longer inherits the agent's environment, and a timeout takes
  descendants with it.** `BashTool` ran `sh -c` with only `current_dir` set. It never
  called `env_clear`, so a model-directed command could read the parent environment — an
  agent process routinely holds provider API keys — with nothing more than `env`. A
  timeout called `start_kill` on the direct child, so anything `sh` had started (a
  background build, a spawned server) kept running after the tool returned.

  The command now receives only `PATH`, `HOME`, `LANG`, `LC_ALL`, `TMPDIR`, `TERM`,
  `USER`, and `SHELL`; `Workspace::inherit_env(true)` restores the previous behaviour and
  `env_allowlist` replaces the set. The child leads its own process group and a timeout
  signals the group, so descendants are killed too. Goal-mode `--until` checks in the CLI
  run under the same policy, so a check cannot see credentials the agent cannot.

  The surface is no longer described as sandboxed. A working directory is not an OS
  boundary: `bash` can still use absolute paths and reach the network, and nothing limits
  memory or CPU. The CLI help, README, and coding-agent docs now say what is enforced —
  path containment for file tools, environment isolation, bounded output and time — and
  what is not.
- **adk-realtime: integrated ADK tools run through the policy pipeline, and plugin failures
  fail closed.** The live `next_event` path called `RealtimeRunner::dispatch_tool_call`, which
  invokes the `ToolBridgeAdapter` directly — the adapter builds a context and calls
  `Tool::execute` with no plugin pipeline, no callbacks, and no confirmation. A tool controlled
  in the standard agent loop therefore ran uncontrolled in realtime, and the richer
  `execute_tool_with_plugins` was unreachable from the live path. ADK tools are now dispatched
  through it; a name that is not a registered ADK tool falls through to native-handler dispatch,
  making that bypass explicit rather than universal. The `before_tool_call` error branch also
  logged "non-fatal" and then executed the tool anyway — authorization, redaction, and policy
  live in before-tool plugins, so a broken guard became no guard. It now refuses the tool and
  returns the failure to the model.
- **adk-realtime: `RealtimeAgent` honours before-tool callback decisions.** The dispatch loop
  built `(error_result, EventActions::default())` as a discarded expression statement and then
  fell through to `tool.execute`, so a before-tool callback could neither deny a tool nor
  substitute a result — it reported a decision that had no effect, which is worse than having
  no gate, because the gate looked present. `Ok(Some(content))` now substitutes a result and
  skips execution, `Err` refuses the tool and skips after-callbacks, and after-callback
  substitutions and errors are applied instead of dropped by `let _ =`. This matches the
  standard agent loop exactly.
- **adk-realtime: realtime tools see the caller's scopes, secrets, and shared state.**
  `RealtimeToolContext` implemented only the required trait methods, so `user_scopes()`
  returned an empty list, `get_secret()` returned `None`, and `shared_state()` returned
  `None`. A scope- or secret-checking tool therefore behaved differently in realtime than
  under a `Runner`, and could not distinguish an unauthenticated caller from a context that
  simply dropped the scopes. All three now delegate to the parent invocation context.
- **adk-sandbox: filesystem isolation is reported as read and write separately, and the
  Windows enforcer reports itself unavailable.** `EnforcedLimits::filesystem_isolation` was
  set true whenever any enforcer was configured. The macOS Seatbelt profile denies network,
  fork, and *writes* before re-allowing writes to configured paths — it never denies reads,
  so sandboxed code could read host files outside the allowed paths while the capability
  said the filesystem was isolated. Read-only entries in `allowed_paths` were effectively
  documentation. The field is now `filesystem_write_isolation` and
  `filesystem_read_isolation`, and macOS reports write isolation without read isolation;
  the platform table and the Seatbelt description say so.

  The Windows `probe` checked that `CreateAppContainerProfile` links, which proves the
  platform API exists but not that the enforcer works — `configure_command` still returns
  `EnforcerFailed` because container creation, ACLs, capabilities, and job-object cleanup
  are unimplemented. A caller selecting an enforcer by probing would pick it and fail at
  run time, so `probe` now returns `EnforcerUnavailable` naming AppContainer. The README,
  sandbox docs, example README, and AGENTS.md no longer list AppContainer as supported.

- **adk-sandbox: Rust compilation runs inside the boundary, policy env is applied, and the
  isolation class is reported.** `ProcessBackend` compiled Rust source with a command
  built outside `run_command` and awaited with `output()`, so the compile phase had no
  enforcer wrapper, no request timeout, and no process group. Compilation is not inert —
  `include_str!` reads files and procedural macros run arbitrary code — so a configured OS
  policy did not cover the phase that could already touch the host, and a compiler that
  blocked ran past the requested timeout. Compilation now goes through the same path as
  execution.

  `SandboxPolicy::env` was never applied; only `ExecRequest::env` reached the child, so a
  policy that set variables silently supplied none. The policy now supplies defaults and
  the request overrides them, which the documentation states.

  New `ProcessBackend::isolation()` returns `IsolationClass::SubprocessOnly` or
  `OsEnforced`, so a caller can tell what it is getting rather than inferring it from the
  crate name; `default()` is subprocess-only.

  Programs are also resolved to an absolute path against the caller's `PATH` *before* the
  environment is cleared. A bare `python3`, `node`, or `rustc` previously required the
  caller to put `PATH` into `ExecRequest::env`, which also handed the executed code
  everything else on that `PATH`. The compile phase additionally receives toolchain
  variables when set, because `rustc` cannot invoke a linker without them; that widening
  is documented, and an enforcer is what constrains it.

- **adk-core/adk-auth: secret access from tools is authorizable and audited.**
  `SecretService` and `SecretProvider` received only a secret *name*. Once a provider
  was attached to an invocation, policy collapsed to whatever the backing cloud
  credentials could read: nothing distinguished a weather tool requesting its own API
  key from the same tool requesting a payment or database secret, and the ADK layer kept
  no record of the access.

  New `adk_core::SecretRequest` carries the requested name plus the identity the
  framework observed — tool, app, user, session, invocation — and an optional purpose.
  `SecretService::get_secret_for` and `InvocationContext::get_secret_for` take it, both
  defaulting to the previous name-only behaviour so existing implementations keep
  compiling. `ToolContext::get_secret_for_purpose` lets a tool state why it needs a
  secret.

  The identity is not something a tool asserts: `LlmAgent` stamps the dispatched tool's
  name onto the request from its own dispatch record, so a tool cannot present another
  tool's identity. Every workflow wrapper forwards the described access, as with the
  other context capabilities.

  `adk_auth::secrets::authorizing::AuthorizingSecretService` enforces declarative
  per-tool grants (exact names or a namespace prefix), denying everything until granted.
  A denied name never reaches the provider, so it does not even appear as an attempted
  read in provider-side logs. Every decision goes to `SecretAuditSink` and to tracing
  with the outcome, name, tool, user, invocation, and reason — never a value.

  One boundary remains: an agent invoked as a tool crosses a `ToolContext`, which
  carries no identity of its own, so accesses inside that agent present the outer
  agent's identity rather than the inner tool's. That is documented rather than papered
  over.

- **adk-auth: the secret cache is now bounded, revocable, and cleared.**
  `CachedSecretProvider` stored values in an unbounded `HashMap` and checked the TTL
  only when the same name was read again. Expired entries were never removed, a
  rotated secret could not be dropped before its TTL elapsed, there was no capacity
  limit, and values were not cleared on drop. The TTL therefore governed what the
  cache *returned* while a value requested once stayed resident for the lifetime of
  the process, and many distinct names could grow the cache without bound.

  New controls: `with_max_entries` (default 128, least-recently-used eviction, `0`
  disables caching), `invalidate`, `invalidate_all`, and `purge_expired`. Entries are
  zeroized on drop, and `Debug` for the cache is redacted so a diagnostic print cannot
  leak a value.

  This shortens residency to roughly the TTL rather than closing the window: a
  `String` may already have been reallocated, copied by the allocator, swapped, or
  captured in a core dump. The documentation states that, and also states plainly that
  the provider interface takes only a secret name — there is no per-tool grant or
  access audit at the ADK layer, so the cloud credentials remain the real boundary.
  Secret providers were previously absent from the official documentation entirely.
- **adk-core/adk-memory: project-scoped memory no longer falls back to global
  scope.** `add_session_to_project`, `add_entry_to_project`, and
  `delete_entries_in_project` had default implementations that discarded `project_id`
  and called their global equivalents, and `Memory::search_in_project` and
  `Memory::add_to_project` did the same. A backend therefore compiled as
  project-aware without implementing a single project method: the call succeeded
  while operating in global scope, and neither the type system nor the return value
  said so. Data intended for one project became visible to everything under the same
  app and user, and a project-scoped delete removed entries outside the project.

  Those defaults now return an error naming the method and the reason. Six built-in
  backends (in-memory, SQLite, PostgreSQL, Redis, MongoDB, Neo4j) implement all four
  project methods and now advertise `supports_project_scoping() == true`.
  `GraphMemoryService` implements none of them and therefore refuses project calls it
  previously answered in global scope — the behaviour change is the fix.
  `MemoryServiceAdapter` reports the capability of the backend it wraps.
- **adk-devtools: workspace containment was bypassable through symlinks.**
  `Workspace::resolve` normalized a requested path lexically and checked
  `starts_with(root)`. A symlink sitting lexically under the root satisfies that
  check while pointing anywhere on the host, and ordinary file I/O follows it, so
  `read_file`, `write_file`, and `edit_file` could reach host files outside the
  advertised workspace. A symlinked parent directory redirected creation and writes
  the same way. The existing containment test covered `..` traversal only.

  Containment is now enforced against the resolved path: the deepest existing
  ancestor of the target is canonicalized, resolving every link along the way, and
  the result must still be inside the root. That covers both a symlinked final
  component and a symlinked parent directory, including creation of a file that does
  not exist yet under a redirected directory. A symlink whose target stays inside the
  workspace keeps working, because repositories legitimately contain internal links
  and refusing them would break ordinary work without improving containment.

  This is a check, not a lock. A symlink planted between the check and the
  subsequent open would still be followed; closing that window needs
  descriptor-relative traversal with platform no-follow semantics, which the
  documentation now states plainly.
- **adk-server: UI routes bypassed configured authentication.** The session,
  artifact, and debug routers received the authentication layer; `ui_api_router` was
  merged without it. With an extractor configured, an unauthenticated caller could
  create and mutate MCP-UI bridge state under any chosen `(app_name, user_id,
  session_id)` tuple, poll another user's notifications, and list, read, or overwrite
  globally registered UI resources — including replacing the HTML text of an existing
  resource URI.

  All `/api/ui/*` routes now carry the same authentication layer as the other
  routers. Bridge handlers substitute the authenticated user for the user named in
  the request body, so one authenticated caller can no longer address another's
  bridge state. A registered UI resource records the user that registered it; only
  that user may read or replace it, and a read of another user's resource answers 404
  rather than disclosing that the URI exists.

  Servers with no extractor configured are unchanged: there is no authenticated
  identity to bind, so routes stay open and resources stay globally visible.

- **Dependency advisories resolved.** Bumped `surrealdb` (optional `adk-rag`
  backend) to 3.2.1, fixing GHSA-cc8f-fcx3-gpjr (high: arbitrary file read via
  `DEFINE ANALYZER` mapper filter) plus four related medium advisories. Bumped
  the OpenTelemetry stack (`opentelemetry`, `opentelemetry_sdk`,
  `opentelemetry-otlp`) from 0.31 to 0.32 and `tracing-opentelemetry` from 0.32
  to 0.33 in `adk-telemetry` and `adk-auth`, fixing GHSA-w9wp-h8wv-79jx
  (unbounded memory allocation in W3C Baggage propagation).

### Added

- **adk-managed: managed state has a store seam that reports its own durability.**
  `CheckpointManager` held events in a `Vec` and run state in a field while documenting
  `checkpoint` as "atomically persist" with a guarantee that "replay will see a consistent view
  after any crash", and describing a load as returning "everything needed to reconstruct a
  session after a restart". Neither held: both operated on in-memory fields with no transaction
  against any persistent store, so a crash lost event history, sequence position, parked-tool
  state, and lifecycle status. The new `ManagedStateStore` trait carries a `Durability`
  (`ProcessLocal` or `CrashDurable`) that a caller can check instead of inferring durability from
  the presence of checkpointing. `InMemoryManagedStateStore` is the shipped backend, named as
  such and reporting `ProcessLocal`; `CheckpointManager::with_store`, `flush`, and `restore`
  connect a manager to one. A crash-durable implementation is not provided — the seam and the
  honest reporting come first, so callers requiring resume-after-restart can detect its absence.

- **adk-computer-use: governed computer-use orchestration crate.** First-party
  ADK-Rust graph, wire contracts, scope authorization, cancellation bridge, and
  tamper-evident evaluation receipts for the `computer-use-mcp` desktop-automation
  server (the crate performs no actuation itself). Ships a deterministic
  reference graph (parallel observation, digest-bound approval interrupts,
  single-executor mutation, independent verification), a typed
  `ComputerUseError` with an `AdkError` conversion, a portable `minimal_graph`
  example, and a cross-platform `live_clipboard` example (macOS, Linux, Windows).
- **adk-realtime: typed raw-audio submission through `RealtimeRunner`.**
  `RealtimeRunner::send_audio_chunk` preserves `AudioFormat` through the provider-neutral
  boundary, and the LiveKit input bridges now use it instead of forcing callers through
  the base64 compatibility API.
- **Reconnect-safe MCP resource notifications.** Applications can register a
  `ResourceNotificationHandler` for resource and catalog updates. `McpToolset`,
  Streamable HTTP clients, and `McpServerManager` retain the callback and
  restore active subscriptions after connection or managed-process recovery.
- **Current MCP client surface.** `McpToolset` now exposes `list_prompts`,
  `get_prompt`, prompt and resource argument completion, resource subscribe and
  unsubscribe, and the negotiated MCP task lifecycle. Public MCP catalog types
  and the exact `rmcp` SDK version used internally are available through
  `adk_tool::mcp`.
- **Dynamic local MCP server registry.** `McpServerManager` can add, start,
  update with rollback, enable, disable, remove, export, and atomically persist
  local stdio server definitions while the application is running. Tool names
  are prefixed with the server ID only when two servers publish the same name.
- **Deterministic MCP manager example.** `examples/mcp_manager` now starts a
  real Rust MCP child process and verifies discovery, tool execution, runtime
  registry changes, persistence, and shutdown without Node.js, a model API key,
  or network access.
- **Official MCP documentation.** Added dedicated architecture, client,
  dynamic-manager, server-authoring, security, and testing guides. The crate
  README now uses versioned local binaries and deployment-owned remote URLs
  instead of mutable package tags and unverified public endpoints.
- **adk-acp: complete stable ACP v1 client/host surface.** Applications can now
  supply opt-in `AcpFileSystem` and `AcpTerminal` callbacks, attach typed MCP
  server configuration to new sessions, await asynchronous human permission
  policy, and cancel an in-flight persistent prompt through a cloneable
  `AcpCancellationHandle`.
- **adk-acp server: per-session stdio MCP tools.** Client-supplied MCP servers
  are validated, started in the session workspace with a bounded handshake,
  exposed to `LlmAgent` and `CodeActAgent` through invocation-scoped toolsets,
  and cancelled on close, delete, failed startup, or server shutdown.
- **ACP examples and official documentation.** Added the vendor-neutral
  `examples/acp_client_host` crate with deterministic workspace-boundary tests;
  expanded the Kiro and server examples; and added dedicated architecture,
  client, server, testing, security, and support-matrix documentation.
- **adk-agent: `CodeAgent` — a CodeAct agent** (`codeact` feature) — a peer to
  `LlmAgent` that acts by writing and executing one code script per turn instead
  of emitting tool calls one at a time. Tools are exposed as callable functions;
  the script returns a tagged `ScriptOutput` (`Observation` / `Error` /
  `FinalResult` / `TransferToAgent`). Language-agnostic via the `CodeRuntime`
  step-wise interpreter seam (the intended adapter wraps Pydantic's Monty). The
  agent is stateless across invocations: HITL confirmation and long-running
  tools **suspend** by serializing the live interpreter continuation into a
  `CodeActCheckpoint` in session state and **resume** on the next `run()`, the
  same save-rebuild-continue model as `LlmAgent`. Tool state/artifact deltas and
  `escalate` propagate, so an `AgentTool`-wrapped sub-agent works as a callable.
  Tool dispatch is sequential by design (single-call seam) to keep the
  durability model sound.
- **adk-agent: `CodeAgent` configuration parity with `LlmAgent`** — generation
  config (+ `temperature`/`top_p`/`top_k`/`max_output_tokens`), `tool_timeout`,
  `output_key`, agent/global instructions (static + dynamic providers, with
  `{state.key}` injection), `include_contents` conversation history,
  per-invocation `toolset`s, retry budgets, circuit breaker, `on_tool_error`
  fallbacks, `output_schema`/`output_type` validation with a correction-retry
  loop, sub-agent transfer with
  `disallow_transfer_to_parent`/`disallow_transfer_to_peers`, and feature-gated
  guardrails (`guardrails`), skills (`skills`), and an `EnhancedPlugin` pipeline
  intercepting tool and model calls (`enhanced-plugins`).
- **adk-agent: `CodeAgent` callback surface, tool-context, and robustness** —
  full lifecycle/interception callbacks matching `LlmAgent`
  (`before_callback`/`after_callback`, `before_model_callback`/
  `after_model_callback`, `before_tool_callback`/`after_tool_callback`/
  `after_tool_callback_full`, the last with `ToolOutcome` exposed via
  `CallbackContext::tool_outcome()`); a fresh per-call `ToolContext` that carries
  the interpreter call id and delegates artifacts, memory, shared state, user
  scopes, and secrets to the live invocation; full tool `EventActions`
  propagation (`state_delta`/`artifact_delta`/`route`) with `escalate`/
  `skip_summarization`/tool-set `transfer_to_agent` treated as terminal on the
  inline, resume/recovery, confirmation-approval, and long-running paths; output
  guardrails that redact the value stored under `output_key`; a SAVE-AFTER
  checkpoint persisted before resuming an executed tool (so once it is persisted
  recovery never re-runs the tool — an at-least-once boundary in the narrow
  window before it lands, like `LlmAgent`); long-running completion matched by
  call id; tool-panic capture; duplicate sub-agent-name validation;
  max-iterations now an error; and confirmation suspends marked
  `interrupted`/`turn_complete`. New `examples/codeact_agent` demonstrates the
  loop end-to-end with a self-contained `CodeRuntime`.
- **adk-codeact-monty: Python `CodeRuntime` backed by Pydantic Monty** — a
  reusable, stateless `CodeRuntime` that runs LLM-authored Python in-process via
  the [Monty](https://github.com/pydantic/monty) interpreter, with
  snapshot-at-call-boundary suspend/resume, per-step `stdout` capture, and a
  tool catalog the model invokes through a single built-in function,
  `call_tool("name", {"arg": value})`. `MontyRuntime::new()` ships with
  conservative default resource limits (per-advance time and memory caps) for
  untrusted code; `MontyRuntime::builder().unlimited()` removes them for trusted
  scripts. `call_tool` is the only way to call a tool — the tool name is a string
  literal embedded in the call (so it survives suspend/resume with no host-side
  name table) and every argument is a string-keyed entry in one dict, so any tool
  name and any argument name is valid (hyphens, Python keywords, even
  `"call_tool"`) and arguments bind by name exactly with no positional inference.
  Any other form — a bare call, keyword arguments to `call_tool`, a non-dict
  argument, or a non-string argument key — is refused with a corrective error
  rather than silently dispatched. Kept outside the workspace (Monty is a git
  dependency, not yet on crates.io); a runnable `examples/codeact_monty_agent`
  drives it offline. **Configurable OS access:** filesystem, environment, and
  clock OS calls are serviced *in place* (never tools, never pausing the agent
  loop) against a host-controlled `OsAccess` policy. `MontyRuntimeBuilder`
  exposes `allow_path(virtual, host, PathAccess::ReadOnly|ReadWrite)` to mount
  host directories (boundary-enforced by Monty), `environ`/`environ_var` to
  expose an explicit environment map to `os.getenv`/`os.environ`, and
  `system_clock(bool)` to gate `date.today()`/`datetime.now()`. The default is
  fully sandboxed (no filesystem access, empty environment, host clock enabled),
  and the granted access is described to the model in the system prompt —
  including the exact subset of `pathlib.Path` Monty implements (read/query,
  write, and pure path ops) whenever paths are mounted, since Monty does not
  support the full `pathlib.Path` API.
- **adk-agent: `CodeRuntime` interpreter seam** — the language-agnostic contract
  a CodeAct runtime implements. `PendingCall` exposes a call's arguments the way
  an interpreter produces them — `positional_args()` and `keyword_args()`
  separately — and the driver binds them onto a tool's parameters centrally via
  `adk_agent::codeact::bind_call_args`, so a runtime never needs a tool schema at
  the call boundary. `RunStep::{Call,Complete,Raised}` are struct variants that
  each carry the `stdout` the script printed since the previous step (constructed
  with `RunStep::{call,complete,raised}` + `with_stdout`); the agent surfaces
  captured output back to the model and persists it into checkpoints so it
  survives suspend/resume and crash recovery. Script-visible failures — including
  syntax/parse errors — flow through `RunStep::Raised`, while `RuntimeError` is
  strictly host failure (snapshot/internal). `render_tools` is a pure function of
  the tool slice (no schema caching required).

- **adk-core: streaming tool progress as events** — `ToolContext::emit_progress(stream, chunk)`
  lets a tool push intermediate stdout/stderr (or any labelled channel) to the UI
  *while it is still running*. The framework forwards each chunk as a partial
  `Event` on the agent's `EventStream` — the same stream the model's reply
  travels on — so UIs render live terminal output without a side channel or log
  scraping. New `Event::tool_progress` constructor and `Event::tool_progress_stream()`
  accessor; progress events carry the originating call id via
  `TOOL_PROGRESS_CALL_ID_KEY`. The default `emit_progress` is a no-op, so
  non-streaming tools and runners are unaffected.
- **adk-core: first-class tool-call/result events** — `Event::tool_calls()` and
  `Event::tool_results()` return typed, render-ready views (`ToolCallView`,
  `ToolResultView`) over an event's tool activity, so UIs consume tool calls and
  their results generically without matching `Part` internals. Because a tool's
  result is an ordinary event, the output of **any** tool — streaming (`bash`) or
  one-shot (`read_file`, `grep`, a web API) — is renderable, not just shell
  tools. `call_id` correlates a call, its progress chunks, and its result.
- **adk-devtools: `bash` streams output** — `BashTool` emits stdout/stderr
  line-by-line via `emit_progress` as the command runs, so terminal output
  appears live in UIs.
- **New example: `streaming_bash`** — an `LlmAgent` with a web UI (Axum + WebSocket)
  that renders live `bash` output and one-shot tool results (`read_file`, `grep`,
  `glob`) from a single event feed. Also runs as a console demo (`-- cli`).

- **adk-core: embedded-resource content part (issue #400).** New
  `Part::EmbeddedResource` variant carries a complete resource — a source URI
  plus inline text or binary contents — mirroring the MCP / ACP embedded-resource
  block. Adds `EmbeddedResource` (untagged `Text` | `Blob`),
  `TextResourceContents`, `BlobResourceContents`, and the
  `Content::with_embedded_resource(..)` helper. `BlobResourceContents::new(..)`
  is a checked constructor that rejects payloads larger than
  `MAX_INLINE_DATA_SIZE` instead of truncating. The variant is additive: older
  serialized `Content` values continue to deserialize unchanged, and existing
  `Part::InlineData` construction sites are untouched.
- **adk-acp: embedded-resource prompt content.** A new shared `content` module
  (`block_to_part` / `part_to_block`) maps ACP `ContentBlock`s to `adk_core::Part`
  in both directions. The server prompt parser and streamer route through it, so
  `@`-mentioned file context arrives as a `Part::EmbeddedResource` and ADK
  embedded-resource content streams back as an ACP embedded-resource block. Text
  resources are preserved verbatim; binary resources are base64-encoded on the
  wire and decoded to raw bytes internally. The server now advertises the
  `embedded_context` prompt capability.
- **adk-acp: usage updates.** The server emits a `SessionUpdate::UsageUpdate`
  derived from ADK usage metadata (token counts, and cost in USD when reported).
  Events without usage metadata produce no update, and counts are never
  fabricated.
- **adk-acp: richer tool-call updates.** `ToolCallUpdate` now carries the tool
  result `content`, affected file `locations` (from a reported `path` string or
  `paths`/`locations` arrays), and a tool `kind` inferred from the tool name,
  while preserving the existing tool-call identifier correlation between a
  `ToolCall` and its later `ToolCallUpdate`.
- **adk-acp: `session/load` with history replay.** The server implements
  `session/load`: it reactivates a persisted session (validating the supplied
  `cwd` like `session/resume`), then reads the stored events and replays each
  user, agent, thought, and tool event as an ordered `session/update`
  notification in original chronological order before completing the request.
  The server advertises the `load_session` capability. A load for an unknown
  session returns a session-not-found error, and a mismatched working directory
  is rejected rather than reattached.
- **adk-acp: multimodal prompt content.** The server accepts image and audio
  prompt content blocks, mapping each to a `Part::InlineData` that preserves the
  MIME type and decoded bytes, and advertises the corresponding `image` and
  `audio` prompt capabilities alongside `embedded_context`. Prompt content of a
  type the server has not advertised is rejected with a descriptive error. The
  client prompt path now transmits non-text ADK content (embedded-resource,
  image, audio) as the matching ACP content block rather than dropping it.
- **adk-acp: server-side permission bridge.** When the ADK Runner pauses on a
  `ToolConfirmationRequest` during a prompt turn, the server now maps it to an
  ACP `session/request_permission` request describing the tool and its
  arguments, awaits the client's outcome, and resumes execution with the mapped
  decision fed back through `RunConfig::tool_confirmation_decisions`, correlated
  by function-call id (allow → approve, deny/cancel → deny). The nested request
  is issued from the spawned prompt task, so the outer prompt response still
  completes — the earlier concern that the official Rust SDK loses the outer
  response after a nested request does not reproduce with this pause/resume
  flow, and the stale limitation note has been removed.
- **adk-acp: client-direction tool-update and usage fidelity.** The client
  streaming surface (`OutputChunk`) now surfaces an External_Agent's
  `ToolCallUpdate` as `OutputChunk::ToolUpdate` (id-correlated, with status,
  kind, title, extracted content text, and affected file locations) and its
  `UsageUpdate` as `OutputChunk::Usage` (tokens used/size, plus cost and
  currency when reported), in addition to the existing text, thought, and
  tool-call surfaces. Agent message text is surfaced unchanged.
- **adk-acp: `examples/acp_full_protocol` reference crate.** A new no-API-key,
  `Runner`-backed `AcpServer` reference example that exercises the full Phase 2
  server-direction surface in-process. It ships a deterministic `ScriptedAgent`
  (no LLM) plus a confirmation-gated `delete_file` tool, and a validating test
  drives the server through the official `agent-client-protocol` SDK over an
  `Channel::duplex()` transport — covering embedded-resource prompts,
  image/audio multimodal prompts, the permission bridge (allow/deny), a
  `session/load` replay-ordering check, and `UsageUpdate` / `ToolCallUpdate`
  surfacing — so Phase 2 regressions are caught without a subprocess or model
  credentials.
- **adk-acp: session modes and configuration options.** A new `SessionControls`
  provider trait lets an agent declare session modes (e.g. "ask" / "code") and
  configuration options (selects, toggles), wired through
  `AcpServerConfigBuilder::session_controls`. The server advertises the declared
  modes and options — and only those — in `session/new`, `session/load`,
  `session/resume`, and `session/fork` responses, and implements `session/set_mode`
  and `session/set_config_option`: a known value is validated, recorded, and
  echoed back as a `CurrentModeUpdate` / `ConfigOptionUpdate` notification, while
  an unknown mode, unknown option, or invalid value is rejected and leaves state
  unchanged. Selections persist in ADK session state (`acp:mode`,
  `acp:config:<id>`) so they survive load / resume / fork. An agent that declares
  no controls advertises no modes and no options, preserving capability accuracy.
- **adk-acp: `session/fork`.** The server implements `session/fork`, branching a
  persisted session into a new session id whose stored history is a copy of the
  source's, carrying over the relevant state (`cwd`, additional directories,
  mode, config) while leaving the source session's persisted history unchanged.
  A fork for an unknown session returns a session-not-found error. The server
  advertises the `session.fork` capability.
- **adk-acp: available-commands and session-info updates.** On session
  activation (create / load / resume / fork) the server emits a
  `SessionUpdate::AvailableCommandsUpdate` for any commands the agent's
  `SessionControls` declares (and none when it declares none), and a
  `SessionUpdate::SessionInfoUpdate` carrying the session title when one is
  recorded (`acp:title`, set via `set_session_title`). A `Plan` `SessionUpdate`
  mapping exists but stays dormant until an ADK plan primitive surfaces plan
  entries, so no plan update is emitted today.

- **adk-realtime: GA realtime providers + integration tool dispatch** — OpenAI
  `gpt-realtime` and Gemini Live wired end-to-end through
  `IntegratedRealtimeRunner`, with **server-side tool execution**, transcript and
  memory integration, and a per-provider audio-rate handshake. Revives the
  realtime stack after the preview-model shutdowns; verified live against both
  providers.
- **adk-realtime: multimodal video input** — new `RealtimeSession::send_video_frame`
  (exposed on `RealtimeRunner` and `IntegratedRealtimeRunner`) sends image frames
  to the model: Gemini Live as continuous `realtimeInput` media chunks, OpenAI
  Realtime as `input_image` conversation items. Lets an agent see what the user
  shows the camera. The default trait impl is a no-op, so other backends are
  unaffected.
- **adk-realtime: affective dialogue** — `RealtimeConfig::with_affective_dialog`
  emits `enableAffectiveDialog` (inside `generationConfig`) for Gemini Live
  native-audio models, so the model adapts its tone to the user's emotion.
- **adk-memory: bi-temporal knowledge-graph memory** (`graph-memory` feature) —
  `GraphMemoryService`, a SQLite-backed knowledge graph (entities, typed
  relations, and time-stamped observations with `valid_from`/`valid_to`
  supersession) implementing `MemoryService`. Serves a compact **profile card**
  for prompt injection and records episodic turns off the hot path; recall is
  token-based so `search` / `load_memory` actually retrieve relevant facts.
- **adk-tool: knowledge-graph curation tools** (`graph-memory-tools` feature) —
  agent-callable `remember` / `relate` write tools plus a `GraphMemoryToolset`,
  so any `LlmAgent` can curate structured long-term memory (recall continues via
  `LoadMemoryTool` over any `MemoryService`).
- **adk-model: configurable parallel tool calls for OpenAI** — opt in/out of
  `parallel_tool_calls` on the OpenAI client (#387).
- **New examples**:
  - `customer_service` — multimodal customer-support agent (camera vision, tone,
    server-side `process_refund` / `connect_to_human` tools), OpenAI or Gemini.
  - `live_translation` — real-time speech-to-speech translation via the dedicated
    translation models (OpenAI `gpt-realtime-translate`, Gemini
    `gemini-3.5-live-translate-preview`).
  - `knowledge_graph_agent` — a plain text `LlmAgent` with KG memory that persists
    across sessions.
  - `realtime_tools` — headless function-calling demo over the GA realtime API.
  - `realtime_voice` ("Mia") reworked to be backed by a real knowledge graph.
  - All web-UI examples ship **system / light / dark** themes.
- **adk-telemetry: direct SQLite span export** (`sqlite` feature; facade forwards
  as `telemetry-sqlite`) — zero-infrastructure tracing with no collector or
  backend to deploy (#373, export half):
  - `SqliteSpanExporter` — spans flow to a dedicated writer thread over an
    unbounded channel and commit in batched transactions (WAL); the traced code
    path pays one channel send. `flush()` for graceful exit.
  - `SqliteTraceReader` — query API (`sessions`, `session_trace`, `trace`,
    `recent_spans`) over a single `spans` table with a JSON attributes column,
    so any SQLite client works.
  - `init_with_sqlite(service, path)` one-line initializer, and a `SpanSink`
    trait so `AdkSpanLayer` can target any sink (the in-memory exporter is
    unchanged).
  - Runnable example: `examples/telemetry_sqlite_export` (agentic Gemini run
    with a tool call, traced end-to-end and read back).
- **cargo-adk: advertised templates implemented for real.** `tools`, `rag`,
  `api`, `openai`, and `a2a` were aliases that silently produced a plain llm
  agent when combined with any `--addon`; they are now real templates
  (`tools` scaffolds a working `#[tool]` in `src/tools.rs`; `rag` wires an
  embedding + vector-store pipeline with `RagTool`; `api` generates a REST
  server — including the previously missing `axum` dependency; `openai`
  defaults its provider; `a2a` resolves to the `a2a-server` pattern).
- **cargo-adk: custom template directories.** `--template-dir` loads TOML
  template manifests (documented in `TemplateRegistry::load_custom_dir`);
  same-name templates override built-ins.
- **cargo-adk: generated projects run out of the box.** Scaffolds end with an
  interactive `Launcher` console when nothing else drives the agent, missing
  API keys produce a friendly error instead of a panic, `--model` and
  `--with-yaml` work on all templates, and `--provider` defaults to the
  template's provider.
- **Quality gates via lefthook** (#374): pre-commit runs fmt + clippy
  (workspace, `-D warnings`) + shellcheck on staged scripts; pre-push runs the
  full nextest suite. The devenv shell registers the hooks automatically;
  lefthook is the single hook manager (devenv git-hooks integration retired).
- **Shell-agnostic publishing: `cargo xtask publish`** (`--resume`,
  `--dry-run`) — works from bash/zsh/PowerShell/cmd; the publish order is
  computed from `cargo metadata` at runtime, replacing the hand-maintained
  tier list. `publish.sh` remains as a thin bash wrapper.
- **Release tooling and CI guards**: `scripts/bump-version.sh` (updates
  Cargo.toml, docs, READMEs, and doc-comment snippets; never touches
  CHANGELOG, lock files, or historical text), `scripts/check-doc-versions.sh`
  (doc snippet versions must match the workspace version; documented adk-rust
  features must exist), and `scripts/check-publish-order.sh` (a valid publish
  order must exist; warns on versioned internal dev-deps).
- **adk-anthropic**: added support for WebFetch to mirror the support
  for WebSearch.
- **Coding agent — a native, end-to-end coding-agent capability.**
  - **adk-devtools** (new crate) — the inner-loop developer toolset
    (`read_file`, `write_file`, `edit_file`, `glob`, `grep`, `bash`) as a
    `DevToolset`, all scoped to a sandboxed `Workspace` (path containment,
    read-only mode, bash timeout/output caps). `edit_file` requires a prior
    `read_file` and a unique match by default.
  - **adk-agent: `CodingAgent` harness** (`coding` feature) — one-call
    `CodingAgent::builder()` that wires the dev toolset, a planning `write_todos`
    tool, and a minimal coding prompt onto an `LlmAgent`; `coding.todos()`
    surfaces the live plan. Default `adk-agent` build is unchanged (feature off).
  - **adk-cli: native `code`, `goal`, and `ultracode` commands.** `code` runs a
    one-shot task; `goal` is autonomous goal mode that loops plan → act → verify
    against a `--until` success command, **durable & resumable** via an atomic
    on-disk checkpoint (`<dir>/.adk/goal.json`, `--resume`); `ultracode` fans out
    to parallel correctness/edge-case/style reviewers and revises until they
    approve. Keys resolve non-interactively from the environment.
  - **adk-graph: fan-in support on `StateGraph`** — new `add_deferred_node_fn`
    and `mark_deferred` bring deferred fan-in nodes (run once, after all upstream
    paths complete) to the core builder, at parity with `GraphAgentBuilder`.
    Enables correct fan-out/fan-in (parallel branches + a single aggregator).
  - **New examples**: `coding_agent` (demo / scenario `tour` / `multiturn` build),
    `coding_graph` (ultra-review workflow), `coding_goal` (durable autonomous goal
    loop) — each a real agent that self-verifies by running the produced code.
  - Design: `docs/design/coding-agent.md`; guide: `docs/official_docs/coding-agent/`.

### Changed

- **The Runner-level context cache is documented as experimental.**
  `RunnerConfig::context_cache_config` and `cache_capable` drive Gemini's explicit
  `cachedContents` API from the Runner. That API requires the cache to **replace**
  `system_instruction`, `tools`, and `tool_config`; sending a cache alongside any of them
  is rejected with `INVALID_ARGUMENT`. The Runner selects a cache before the agent
  resolves its tools, so it cannot assemble that request, and enabling these fields does
  not produce cache hits. Both default to unset and prompt caching needs no Runner
  configuration — Anthropic and Bedrock cache by default, OpenAI caches server-side, and
  Gemini caches implicitly on 2.5/3.x — so no supported configuration changes behavior.
  The Runner reference now states this, and guaranteed caching for Gemini is deferred to
  the model integration where the other providers already handle it.
- **OpenAI integrations now use `async-openai` 0.41.** Chat Completions,
  Responses, and Realtime integrations adopt the current 0.41 types and
  transport dependencies.
- **Toolchain pinned to Rust 1.95.0.** `rust-toolchain.toml` is now the single source: rustup
  reads it locally and devenv reads the same file through `languages.rust.toolchainFile`, so a
  devenv shell and a plain `cargo` invocation cannot drift. The workspace resolver moves to `3`
  and `rust-version` to `1.95`; `Cargo.lock` is unchanged by the resolver bump. Five new 1.95
  clippy lints are fixed — three `collapsible_match`, one `iter_kv_map`, and one `sort_by_key`
  that needed `Reverse` because the sort is descending. 1.95 also clears the AWS SDK MSRV floor,
  so `cargo update` no longer breaks the `bedrock` feature.

- **MCP now uses official `rmcp 2.2` and MCP `2025-11-25` protocol types.**
  ADK-Rust re-exports its aligned SDK for advanced transports and server
  authoring. Sampling remains an opt-in deprecated-compatibility feature under
  upstream SEP-2577. Downstream code that names `rmcp 1.x` content, elicitation,
  or service types directly must migrate those annotations or import aligned
  types through `adk_tool::mcp::rmcp`.
- **MCP HTTP configuration is now effective.** Request timeouts, custom headers,
  custom API-key headers, bearer or fixed client-credentials tokens, and bounded
  expired-session recovery are applied to the Streamable HTTP client.
- **MCP approval semantics are documented precisely.** `autoApprove` remains a
  round-tripped configuration-compatibility field; it is not interpreted as
  ADK-Rust authorization or human approval policy.
- **adk-acp now uses the official `agent-client-protocol` 1.2 SDK while
  negotiating stable wire protocol v1.** The client and server share the SDK's
  typed JSON-RPC connection rather than maintaining a parallel wire model.
- **ACP capability publication is now exact.** Unsupported media, remote
  transports, and optional protocol features remain unadvertised. Stdio MCP is
  accepted as required by stable v1; optional HTTP and SSE MCP are sent by the
  client only when the external agent advertises them.
- **ADK agent tool confirmation can be resolved live.** `RunConfig` accepts an
  asynchronous `ToolConfirmationHandler`, and allow-once decisions are keyed by
  exact function-call ID rather than tool name.

### Fixed

- **adk-session: the Vertex AI backend follows the Session API wire contract and
  preserves complete ADK events.** Create and append requests now send the
  GA `v1` `Session` and `SessionEvent` bodies directly, create/delete operations
  are polled to completion with bounded backoff, list filtering uses `user_id`,
  caller-supplied logical session IDs retain the core identity rules, and their
  deterministic derived remote IDs follow the service validation rules.
  Canonical Vertex content preserves text, thought signatures, inline/file
  data, top-level media `displayName`, function calls/responses, and bounded
  `mediaResolution` objects through its sidecar. GA `v1`
  function-call/response IDs and empty or noncanonical Base64 thought-signature
  bytes use lossless raw persistence because their canonical proto messages
  cannot preserve those values. `rawEvent` exposes Google ADK-compatible replay
  fields and stores the complete Rust event under the versioned `_adkRust`
  envelope. Arbitrary Struct values remain opaque and retain every original
  key/value when the reserved envelope is removed; malformed pre-existing
  `_adkRust` values fail closed, and incompatible Google-shaped projections
  fall back to opaque preservation. Logical session IDs are isolated by the
  complete app/user/session identity through deterministic remote IDs and a
  protected state marker.
  Without a fixed engine, `app_name` must be a canonical nonzero numeric engine
  ID; fixed shared engines require an explicit per-app opt-in before accessing
  unmarked pre-v2 sessions. Vertex user IDs are limited to 128 Unicode scalar
  values. Global and multi-region locations use their documented endpoints,
  custom endpoints are origin-only and redirect-disabled, and create/delete
  long-running-operation responses validate their GA protobuf `Any` type URLs.
  Encoded request bodies, decoded response bodies, and aggregate pagination
  default to 64 MiB byte budgets, complete pagination has a 120-second deadline,
  nested JSON/Vertex Struct values are bounded, and recent-event
  limits/timestamp filters are applied server-side. Transport failures and
  unresolved polling or successful-response validation after mutation
  transmission return non-retryable create/delete/append outcome-ambiguity
  codes with reconciliation guidance. Terminal LRO errors remain known
  `operation_failed` results with category-derived retry hints. Proto3-omitted
  empty optional scalars and Struct-normalized schema version `1.0` restore
  exact private scalar presence; safe integer/double-normalized Vertex Struct
  values compare semantically without discarding preserved canonical
  extensions.
  **Breaking:** `VertexAiSessionService::with_credentials()` now returns
  `Result<Self>` because endpoint validation and bounded HTTP-client construction
  can fail.
  `vertex-session` is now available through the `adk-rust` umbrella crate and
  joins the PR feature-coverage gate.
- **adk-acp: session replay preserves message roles and multimodal content.**
  `session/load` now emits stored user content as `UserMessageChunk` and
  model/agent content as `AgentMessageChunk`. Image and audio bytes, file
  references, embedded resources, and multimodal function results use their
  native ACP content blocks instead of disappearing during replay. Inbound
  Base64 binary content is bounded before decode allocation and checked against
  the core 10 MiB limit after decoding. Outbound URI-less inline data maps only to
  matching `image/*` and `audio/*` ACP blocks instead of mislabeling other MIME
  types as images. The load regression test specifies expected roles and
  ordering independently of the production mapper.
- **adk-agent: automatic tool dispatch now requires both safety signals.**
  `ToolExecutionStrategy::Auto` previously ran every read-only tool concurrently
  without consulting `is_concurrency_safe()`. A read-only tool backed by a
  stateful cursor, cache, or non-thread-safe client could therefore race. `Auto`
  now includes a call in its concurrent subset only when the selected tool is
  both read-only and concurrency-safe, then executes the remaining calls
  sequentially. `Parallel` remains an explicit caller override that bypasses
  metadata, with caller-owned safety.
- **adk-runner: `SandboxRunner::run` runs the agent instead of reporting a completed run that
  did nothing.** The method provisioned the workspace, started a session, bound tools, then
  executed a placeholder future and returned `Ok`. It now drives the inner `Runner` with the
  bound sandbox tools injected through `RunConfig::runtime_toolsets`, returns the buffered events,
  snapshots the live workspace when configured, and always stops the sandbox before returning.
  Identity validation happens before side effects; session creation occurs only for a structured
  not-found result; and execution, snapshot, and stop failures follow deterministic precedence
  with later cleanup failures retained as error metadata. **Breaking:** `SandboxRunner::run` takes
  a `user_content: Content` argument — an agent loop cannot run without input — and
  `SandboxRunResult` now includes the emitted events.
- **adk-runner: `Runner::run_with_config` supplies a per-invocation `RunConfig`.** Needed for
  tools that exist only for the duration of one run. `Runner::run` delegates to it with `None`.
  New accessors expose the runner's application name, session service, and base run config.
- **adk-session: missing sessions have one structured contract across every backend.**
  `SessionService::get` returns `session.not_found` with the `NotFound` category only when a
  valid `(app_name, user_id, session_id)` identity has no matching record. SQLite and PostgreSQL
  use `fetch_optional`, so query failures are no longer misreported as missing sessions. The
  Firestore backend also verifies the stored user before returning a session.
- **adk-server: `message/stream` drives the agent instead of emitting synthetic events.**
  The handler created a task, transitioned `Working` then `Completed`, and never invoked the
  Runner, so a streaming client received a task that reported success and produced no output.
  It now streams `TaskArtifactUpdateEvent`s as the agent produces them, keyed to one artifact ID
  with `append`/`lastChunk` derived from each event's `partial` flag, and reports `Failed` when
  the agent errors. The joined text is persisted so a later `tasks/get` returns what was
  streamed. `tasks/resubscribe` is documented as the snapshot it is rather than implying a live
  re-attach.
- **adk-server: `--features a2a-v1` passes clippy and is gated by CI.** No workflow built the
  feature, so 11 `collapsible_if` failures had accumulated in the A2A v1 surface unseen.
  `adk-server --features a2a-v1` joins the feature-coverage matrix.

- **adk-auth: the `sso` feature compiles and is gated by CI again.** No workflow built
  `adk-auth --features sso`, so its SSO/OAuth surface had drifted out of the clippy gate and
  failed `-D warnings` on four `collapsible_if` lints. `jsonwebtoken` moves to 11, whose
  `Algorithm` is `#[non_exhaustive]`; the validator now rejects unrecognised algorithms rather
  than matching exhaustively. `adk-auth --features sso` joins the feature-coverage matrix.

- **adk-sandbox: sandboxed compilation uses the caller's toolchain.** The compile phase passed
  `RUSTUP_HOME` and `CARGO_HOME` into the sandbox but not `RUSTUP_TOOLCHAIN`. `rustc` on `PATH` is
  usually a rustup shim, so without it the shim ignored the caller's selection and resolved
  `rust-toolchain.toml` instead — compiling with a different toolchain than intended, or, when the
  pinned one is not installed, attempting a download that the sandbox's network denial blocks. That
  surfaced as `info: syncing channel updates for …` reported as a compile failure.

- **adk-sandbox: the Linux bubblewrap enforcer is selectable again.** `LinuxEnforcer::probe` ran
  `bwrap --unshare-user -- /bin/true` with no bind mounts. bwrap gives the new namespace an empty
  root, so `/bin/true` did not exist inside it and `execvp` failed — which the probe reported as
  "user namespaces are not available. Check that `kernel.unprivileged_userns_clone` sysctl is set
  to 1". The check therefore failed on **every** host, including hosts where bubblewrap works
  perfectly, so `get_enforcer()` never returned the bubblewrap enforcer: Linux ran with no
  OS-level sandbox while the documentation advertised one, and the diagnostic pointed operators at
  a sysctl that was never the cause. The probe now binds the root filesystem, so it tests what it
  claims to.
- **adk-sandbox: the bubblewrap argument property tests compile.** `bwrap_args_property_tests.rs`
  used an inline `{args:?}` capture inside `prop_assert_eq!`, which expands through `concat!` and
  cannot capture. The file is both `cfg(target_os = "linux")` and behind the `sandbox-linux`
  feature, so no build ever compiled it and the failure went unnoticed; the bwrap argument
  construction had no executing test coverage on the only platform where it runs.
- **adk-graph: functional `TaskContext::interrupt` can be resumed with a typed value.** The method
  emitted an event, saved a checkpoint, recorded an `__interrupt__` task, and then always returned
  `InterruptTypeMismatch { message: "workflow interrupted" }` — its own comment deferred
  suspension and resumption to future work, and nothing outside the method consumed a resume
  value. The signature promised typed resumption that could not occur. Each interrupt site now
  gets a continuation key from its position in the run (`interrupt-1`, `interrupt-2`, …), reported
  in the new `FunctionalError::Suspended` and written to the checkpoint as `continuation_key`.
  `TaskContext::with_resume_values` supplies values by key, and an interrupt whose key is present
  returns the deserialized value at the call site. `InterruptTypeMismatch` now means what its name
  says: the supplied value did not deserialize into the expected type.
- **adk-agent: an ambient agent invokes the agent, delivers its output, and does not serialize
  triggers.** `AmbientAgent::start` succeeded without a trigger handler and then only logged each
  event, so `AmbientAgent::new(..).start()` appeared to run an agent that was never invoked; it
  now fails with an error naming what is missing. Events and errors the agent produces are
  delivered through `take_output(capacity)` instead of being logged at debug level and dropped.
  Triggers are dispatched under `with_max_concurrent_triggers` (default 4) rather than one at a
  time — the loop previously drained a handler's entire event stream before polling the source
  again, so one slow trigger blocked every later one. Durable offsets, dead letters, and retry
  remain the caller's responsibility and are documented as such.
- **Release statements are checked against one source.** The workspace version, the changelog
  heading, the README release banner, and the README roadmap's "current" marker were
  maintained independently. `scripts/check-doc-versions.sh` skips `CHANGELOG.md` and never
  looked at the banner or the roadmap, so nothing detected drift between them. The new
  `scripts/check-release-consistency.sh` derives all three from the workspace version and runs
  in the PR-tier `templates` job. Its `--release` mode additionally requires a `v<version>` tag so a
  published artifact can be attributed to an exact commit; outside release mode it reports the
  commit a release would be cut from. There is currently **no `v2.0.0` tag**, which is why a
  defect cannot be attributed to the published artifact from this repository alone.
- **adk-managed no longer described as durable.** The crate README, crate docs, root README,
  and AGENTS.md called managed execution durable, and the README claimed sessions "survive
  process crashes with zero event loss". Checkpoints, the agent registry, and active sessions
  are held in memory: they support replay and resume within a process and do not survive
  process loss. The "atomic checkpoint persistence" wording is corrected too — it is a single
  assignment under a lock, not a transaction with a persistent store.
- **adk-model: content parts a provider cannot carry are recorded, not silently dropped.**
  The Bedrock converter returned `None` at five sites — one carrying the comment
  `// Unsupported MIME type — skip silently` — so audio, video, arbitrary binary, some file
  references, unsupported embedded blobs, and Gemini-specific server-tool parts vanished on
  the way to the model. A request could reach the provider without material the caller
  supplied, and the model could answer as though it had seen a document it never received.
  The new `adk_model::part_conversion` module classifies every part as `Converted`,
  `Downgraded`, or `Omitted`, warns as each loss is recorded, and offers
  `ConversionReport::into_error` for callers that must fail before dispatch rather than send
  an incomplete request. `adk_model::bedrock::convert::report_for_contents` exposes the
  outcome without issuing a request. Accounting is complete by construction: a part that
  leaves an adapter with no recorded fate is reported as an unexplained omission.
- **adk-graph: an action node whose backend does not exist is rejected when the graph is
  built.** Database actions validated a connection and then returned an error explaining
  that no driver is integrated; email monitor and send did the same; JavaScript and
  TypeScript code execution is a placeholder; and a node needing an unenabled feature
  failed the same way. A workflow could deserialize, validate, and compile, then fail
  only when that node executed — after earlier nodes had already had their side effects.

  `Node::validate` is a new defaulted trait method, and `StateGraph::compile` calls it
  for every node, so an unavailable configuration is refused up front with the node name
  and the reason. `ActionNodeExecutor` reports database nodes, email nodes, JS/TS code
  nodes, and feature-gated nodes whose feature is off. Rust code nodes and every
  implemented action are unaffected. A custom node can take part by overriding
  `validate`. The node-type table in the docs now marks what is not implemented instead
  of listing it as available.
- **adk-graph: an agent inside a graph keeps the caller's runtime.** `AgentNode` built a
  `GraphInvocationContext` from scratch for every run: it hardcoded
  `user_id = "graph_user"`, `app_name = "graph_app"`, and branch `main`, used a default
  `RunConfig`, returned `None` for artifacts and memory, and let every optional
  capability fall back to its default — so secrets, shared state, cancellation, scopes,
  and request metadata all disappeared. An identity-dependent tool saw a synthetic
  principal inside a graph and the real one outside it, and `Runner::interrupt` could
  not reach an agent running as a node.

  `ExecutionConfig::with_parent_context` carries the invocation into the graph, and
  `GraphAgent` sets it automatically, so identity, scopes, request metadata, secrets,
  memory, artifacts, shared state, cancellation, and `RunConfig` all reach the agent.
  The branch is deliberately *derived* as `{caller_branch}.{agent_name}` so a node's
  events stay attributable. A graph invoked directly, with no parent, keeps the
  synthetic identity — that is now an explicit standalone mode rather than the only
  behaviour. Node conversation history remains scoped to the node's own graph session.

- **adk-graph: `TimeTravelHandle::replay` is renamed to `state_history`.** Its rustdoc
  said it "re-executes the graph", and the module described replaying. The
  implementation listed checkpoints, sorted and filtered them, and returned stored
  `(step, state)` pairs — it never invoked a node. Callers could have treated stored
  snapshots as a fresh replay. The method now says what it does: it reads checkpoints
  and executes nothing, and `fork_at` plus a normal invoke is the way to actually re-run
  from a point in history. `adk graph replay` keeps its name but no longer claims to
  replay; its output states that nothing was re-executed.
- **adk-managed: sessions belong to an owner, and ignored environment configuration is
  refused.** `start_session` named its environment argument `_env` and never read it, so a
  caller supplying environment variables or a working directory received a session that
  silently ignored them. Every session was also persisted under the constants `managed` /
  `managed_user`, and the session loop repeated them for each Runner call, so all managed
  sessions shared one logical namespace: lookup, memory, and deletion could not be scoped to a
  caller and no session could be attributed to one. `start_session` now requires a validated
  `ManagedOwner`, persists the session under it, and makes Runner calls with it;
  `EnvironmentConfig` requesting anything is rejected with an explanation, because sessions run
  in-process and applying it would mutate state shared with every other session.
- **adk-managed: deleting a session now deletes its persisted conversation.**
  `delete_session` archived the session, cancelled its loop, and dropped the in-memory
  handle, but never called `SessionService::delete` — even though `start_session` had seeded a
  persistent session that the Runner appended every turn to. The API reported deletion while
  the conversation remained in the configured backend, outliving the process that deleted it.
  The identity used at creation is now recorded on the session and used for deletion, so a
  change to session addressing cannot orphan data. If backend deletion fails, the error names
  the app, user, and session that still hold data.
- **adk-managed: session status reflects normal execution, not only control-plane calls.**
  `ActiveSession` owned the `Arc<RwLock<SessionStatus>>` that `ManagedAgentRuntime::status`
  reads, while `SessionLoop` owned a separate plain field, despite a comment claiming the two
  were shared. Queued → running → idle transitions updated only the loop's copy, so a session
  actively executing turns kept reporting `Queued`; only pause, resume, archive, and deletion
  moved the public value. The loop now writes to the caller's handle via
  `SessionLoop::with_shared_status`.

- **adk-realtime: integrated sessions actually receive the history and memory they load.**
  `IntegratedRealtimeRunner::connect` fetched the prior session into `_session` and dropped it,
  and the memory branch logged "injecting memory entries into session context" next to a
  comment saying injection was a future enhancement. A resumed session began with neither the
  history nor the memory the builder implies, while the logs reported otherwise. Both are now
  rendered into one bounded block and prepended to the system instruction before the provider
  session is created, governed by `max_memory_injection` and the new `max_history_injection`.
  `IntegratedRealtimeRunner::instruction()` and `RealtimeRunner::instruction()` expose what a
  session was created with, so carried context can be asserted instead of inferred from logs.
- **adk-realtime: `max_concurrent_tools` is enforced, and tools no longer stall the event
  loop.** The field defaulted to 4 and was read by nothing — no semaphore, no scheduler.
  `FunctionCallDone` was awaited inline in `handle_event`, which the run loop awaited before
  reading the next event, so tool calls ran strictly one at a time and blocked audio,
  transcripts, and interruptions for the full duration of each call. Tool calls are now
  dispatched onto the run loop under a semaphore sized by `max_concurrent_tools`, so event
  intake continues while tools run. The single follow-up `create_response` owed after
  automatic tool output is now issued once both the dispatching response has closed and
  every dispatched tool has reported, in either order — previously the ordering was implicit
  in the inline await and would have been lost.
- **adk-realtime: transport loss is distinguishable from a graceful close.**
  `EventHandler::on_disconnect` is called when the provider transport ends, before `run`
  returns. `run` returns `Ok(())` for both cases, so a caller previously could not tell
  them apart. The runner still does not reconnect automatically; the policy is documented.

- **adk-server: background runs execute a workflow instead of reporting success.**
  `BackgroundRunner::run_with_timeout` received neither the workflow ID nor the input. It
  checked cancellation and returned `Completed` with an empty object, so a client got a
  completed status for work that never ran — and because the placeholder could not fail,
  the retry budget was never exercised by anything.

  A run now goes through a `WorkflowExecutor`, which resolves the `workflow_id` and
  receives the input and the cancellation token. `WorkflowRegistry` provides a
  closure-based implementation. Submitting an unregistered workflow returns 404 instead
  of queuing a run that can never execute, and a run submitted with no executor
  configured **fails** rather than reporting completion. Retry re-executes from the
  beginning with the original input; the documentation previously claimed it resumed from
  the last checkpoint, which nothing implemented, and now says what it does. Run records
  remain in-memory and are lost on restart, which the docs now state rather than implying
  durability.

- **adk-server: a cron occurrence is claimed once.** Due detection used
  `last_execution.unwrap_or(created_at)`, and under the `Queue` policy scheduling state
  advanced only when a run *started*. An occurrence waiting behind an active run
  therefore stayed due and was enqueued again on every one-second poll, turning one
  schedule point into many runs. When the active run finished, its monitor started one
  queued run without creating a monitor for it, so `active_run_count` stayed nonzero for
  good and every later `Skip` and `Queue` decision was stuck.

  `due_occurrences` now reports the exact schedule point, `claim_occurrence` takes it
  atomically under one write lock, and the scheduler acts only on a won claim. Every run
  — including one taken off the queue — goes through the same monitored path, which now
  follows the whole chain and releases the active slot even if a run record disappears.

- **adk-server: `GET /cron/{job_id}` is mounted.** The module documented the endpoint and
  a store `get` existed, but only PATCH and DELETE were routed, so clients had to list
  every job and filter locally.
- **adk-model: DeepSeek requests now carry a response format when a schema is set.**
  `DeepSeekClient::build_request` read temperature, top-p, token limits, tools,
  thinking, and reasoning effort, but always sent `response_format: None` — even with
  `GenerateContentConfig::response_schema` present — while the provider module
  advertised structured JSON output. Native enforcement was never requested, so
  structured turns relied entirely on the agent's textual instruction and could cost
  extra retries.

  A response schema now enables DeepSeek's JSON Output
  (`response_format: {"type": "json_object"}`), which is the only mode the API
  supports; there is no `json_schema` variant, so the schema itself remains enforced
  by the agent's validation and the module documentation now says so rather than
  implying provider-side schema enforcement. DeepSeek also requires the word "json"
  to appear in the prompt when JSON Output is on, or the API can return empty
  content, so the adapter adds that mention when the conversation does not already
  contain it.
- **adk-agent: tool progress is bounded.** Each tool batch created a
  `tokio::sync::mpsc::unbounded_channel`, and `emit_progress` sent into it with no
  backpressure and no aggregate limit. A tool producing output faster than the
  client consumed it — a compiler log, a shell command, a runaway loop — grew the
  queue until it was drained or the process ran out of memory, and a slow SSE
  consumer made it worse.

  The queue is now bounded at 256 events, a chunk is capped at 8 KiB (truncated on
  a character boundary, so multi-byte text is never split), and a call may forward
  1 MiB of progress in total. A tool that outruns its consumer waits up to 100 ms
  for space and then drops the chunk, so a stalled consumer slows the tool briefly
  but can never stall it indefinitely. Whenever output is dropped, exactly one
  progress event carrying `[adk: tool progress truncated]` is emitted for that
  call, so a gap is visible rather than silent. Final tool results are unaffected.
- **adk-core/adk-agent: tool approvals are scoped to one exact call.** Live
  decisions from a `ToolConfirmationHandler` were tracked by function-call ID, but
  static decisions in `RunConfig::tool_confirmation_decisions` were looked up by
  **tool name**. One `delete_file` approval therefore authorized every
  `delete_file` call evaluated against that map, whatever its arguments, and two
  calls to the same tool in one turn could not receive different decisions. A
  decision intended for one action could be replayed onto a materially different
  one, which weakens the authorization boundary precisely in the resumed and
  web-driven flows that rely on the static map.

  Static decisions are now keyed by function-call ID, matching the live path and
  the ID already reported on `ToolConfirmationRequest`. An unrecognized key means
  "no decision", so the call stays pending rather than executing.

  The new `RunConfig::tool_confirmation_fingerprints` optionally binds a decision
  to the arguments it was granted for, using the new
  `adk_core::tool_call_fingerprint`. This defends the case where a call ID is
  replayed with different arguments after a round trip through something
  untrusted, such as a browser. A mismatch is treated as unconfirmed. The
  fingerprint is canonical over object key order, so re-serialized arguments still
  match.

  Consumers updated to the call-keyed contract: `adk-acp`'s permission bridge
  (whose own module documentation already claimed call-level correlation while the
  code keyed by name), and both confirmation gates in `adk-agent`'s CodeAct agent.
- **adk-runner: runs and persistence writes are keyed by full identity.** Two
  defects with the same root cause — the identity triple was resolved and then
  discarded.

  Active runs were tracked in a `HashMap<String, CancellationToken>` keyed by the
  raw session ID. Because a session ID is only unique within an app and user,
  `(app-a, user-a, shared-id)` and `(app-b, user-b, shared-id)` collided inside one
  `Runner`, and two concurrent runs for one identity overwrote each other's token.
  The drop guard then removed the key unconditionally, so a finishing run could
  deregister a different run that was still going. Runs are now keyed by a unique
  run ID carrying the full identity, and cleanup removes only the entry it
  inserted. `Runner::interrupt(session_id)` now cancels every run for that session
  rather than whichever registered last; the new `Runner::interrupt_identity`
  targets one exact identity, and `Runner::active_runs` reports identities.
  Registration was also eager while cleanup was lazy inside the stream generator,
  so a stream dropped before its first poll leaked its registration permanently;
  the guard is now created eagerly.

  Separately, the Runner resolved sessions with the full triple but persisted every
  event through `append_event(session_id, event)`. All five write sites now use
  `append_event_for_identity`, so a backend whose natural key is composite can bind
  each event to its tenant.
- **adk-graph: checkpoints recorded finished work as pending, and streamed runs
  never checkpointed at all.** Three defects in `PregelExecutor`:

  1. `run` saved its checkpoint *before* advancing the frontier, so the stored
     `pending_nodes` were the nodes that had just completed. Resuming re-executed
     them and re-applied their updates, which is wrong for any node that is not
     idempotent — counters, accumulators, appends, and external side effects.
     Checkpoints are now written after the frontier advances, and a finished run
     records an empty frontier so resuming a completed thread returns the final
     state instead of restarting the graph. Interrupts deliberately keep saving
     the executing frontier, because an interrupted node still owes its updates.
  2. `run_stream` saved no checkpoints, and its interrupt path returned without
     saving one — so a streamed run could not be resumed and a streamed
     human-in-the-loop interrupt was unrecoverable. Streamed runs now checkpoint
     on the same schedule as blocking runs, including on interrupt.
  3. In `StreamMode::Messages` the executor drained `execute_stream` for events
     and then called `execute` again to obtain state updates, running every node
     twice per super-step. For `AgentNode` that meant two billed model calls per
     node, and the streamed tokens came from a different execution than the state
     that was kept. Nodes now report their updates on the stream as
     `StreamEvent::Updates`, and the executor applies those from the single
     execution.

  `Node::execute_stream` therefore carries a new contract: an implementation must
  yield a `StreamEvent::Updates` event with its state updates. The default
  implementation does this, so nodes built from closures are unaffected. A custom
  override that does not will stream events but contribute no state in `Messages`
  mode. `AgentNode::execute_stream` now applies its output mapper, which it
  previously never did. Timeout policies now apply to the streamed execution
  itself, where `idle_timeout` means no event was produced within the limit.
- **adk-agent: provider errors are no longer reported as successful turns.**
  `LlmResponse` carries `interrupted`, `error_code`, and `error_message`, and a
  provider adapter can report a terminal failure inside an otherwise successful
  stream item — `adk-anthropic`'s `from_stream_error`, the OpenAI Responses error
  event, and the OpenAI websocket transport all do. `LlmAgent` copied content,
  finish reason, usage, and provider metadata onto its events but not those three
  fields, and never inspected them, so a failed turn arrived as an ordinary event
  with no content and the run completed successfully. Callers, persistence, retry
  policy, and telemetry could not distinguish a provider failure from a model that
  simply said nothing.

  Those fields now travel with both partial and final events, and a terminal
  `error_code` ends the run with an `AdkError` coded `model.provider_error`,
  carrying the provider's own code in the error details under
  `provider_error_code`. The event is emitted *before* the failure so the failed
  turn stays observable and persisted rather than vanishing into an error.
  `interrupted` is recorded but is not treated as terminal, and truncation is
  unaffected: a response cut short by a token limit reports
  `finish_reason: MaxTokens` and no error code.

- **adk-agent/adk-tool: workflow context wrappers no longer drop cancellation,
  secrets, and shared state.** Each wrapper re-implements `InvocationContext` and
  delegates to an inner context, but most capability methods have permissive trait
  defaults — `is_cancelled()` returns `false`, `get_secret()` returns `None`,
  `shared_state()`/`user_scopes()`/`request_metadata()` return empty — so a
  wrapper that omitted one still compiled and silently lost it. Capabilities
  therefore depended on which workflow agent a sub-agent happened to run under:
  `LoopAgent`'s history wrapper dropped cancellation, secrets, and shared state;
  the skill-injection wrapper dropped all five; `ParallelAgent`'s shared-state
  wrapper dropped cancellation and secrets. Most visibly, `Runner::interrupt` did
  not reach an LLM agent nested under any of them, because the agent's
  `is_cancelled()` checks read `false` through the wrapper.

  All wrappers now forward every capability. `adk-tool`'s agent-as-tool context
  additionally forwards `user_scopes`, `get_secret`, and `shared_state`, so a
  scope-guarded tool invoked by an agent-as-tool sees the caller's grants. A
  conformance suite pushes a sentinel context with a non-default value for every
  capability through each wrapper — and through a composed stack, since an inner
  wrapper that drops a capability defeats a correct outer one.

  Known remaining gap: `is_cancelled` and `request_metadata` are not part of the
  `ToolContext` surface that the agent-as-tool wrapper is built from, so
  cancellation still does not reach an agent invoked as a tool. Closing that needs
  those methods added to `ToolContext`.

- **adk-agent/adk-core/adk-runner: `ParallelAgent` sub-agents no longer read each
  other's output.** Concurrent branches all read the full session history, so an
  analyst in a fan-out could see a sibling's answer before forming its own — while
  the docs described sub-agents as working independently. `ParallelAgent` now
  places each sub-agent on its own conversation branch
  (`{parent}.{parallel_agent}.{sub_agent}`) and stamps emitted events with it, and
  history reads are scoped to that branch: an event is visible when its branch
  equals the reader's or is an *ancestor* of it, so the conversation leading to
  the fan-out stays visible while siblings and nested descendants do not. This
  mirrors ADK Python's `_is_event_belongs_to_branch` and ADK Go's
  `eventBelongsToBranch`, including the delimiter guard that stops `agent_0` from
  matching `agent_00`.

  The mechanism is additive and opt-in by construction: an empty branch on either
  side matches everything, so events written without a branch stay globally
  visible and agents outside a fan-out are unaffected. New API:
  `Session::conversation_history_scoped` (defaulted, so existing `Session`
  implementations keep compiling) and `adk_core::event_belongs_to_branch`. A
  branch already carrying a deeper path is preserved, so nested workflows compose
  rather than the outer agent overwriting the inner one.

- **adk-agent: `ParallelAgent` branches now actually run in parallel.** The
  previous implementation resolved every sub-agent's `run()` future together but
  then drained each returned `EventStream` to completion in turn. Since
  `Agent::run` only *builds* a stream — the work happens when the stream is
  polled — branches executed one at a time, so latency was additive and a slow
  branch blocked every later one (two 300 ms branches took 604 ms; they now take
  302 ms). Branch streams are merged with `select_all`, matching the ADK Python
  (`_merge_agent_run`) and ADK Go (`parallelagent`) designs, including their
  per-branch backpressure: a branch cannot run ahead while an event it already
  produced is still being consumed, so the runner's per-event persistence stays
  in step with execution. Dropping the merged stream now tears down in-flight
  branches instead of leaving them running. Error handling is unchanged — every
  branch still drains and a single terminal error is surfaced — except that the
  reported error is now chosen by sub-agent declaration order rather than by
  whichever branch failed first, which concurrency would otherwise make a race.

- **adk-runner/adk-agent: workflow agents are resumed at the root across turns**
  (#419) — in a session that persists across turns, `find_agent_to_run` no
  longer routes a follow-up user message to a single sub-agent that responded
  last when that sub-agent lives under a workflow agent (parallel, sequential,
  loop, conditional). A new `Agent::supports_agent_transfer` hook (default
  `true`, overridden to `false` by workflow agents) makes the runner resume the
  workflow root so every sub-agent runs again. Mirrors Google ADK's
  `_is_transferable_across_agent_tree`.
- **adk-model: `turn_complete` on tool-call responses** (#401) — `deepseek` and
  `openai_compatible` providers no longer set `turn_complete: true` when a
  response carries tool calls; the turn continues until tool results are
  processed. Adds `Content::has_function_calls()` to `adk-core`. Direct
  `LlmResponseStream` consumers can now rely on `turn_complete` instead of
  scanning for `Part::FunctionCall`.

- **adk-realtime: one `response.create` per turn for parallel tool calls** — the
  runner fired a response per tool, so two tools in one turn hit OpenAI's
  *"conversation already has an active response in progress"* and stalled the
  session. Tool outputs are now sent as they complete and a single response is
  triggered once the dispatch response finishes (`send_tool_output` +
  `respond_after_tools`); works for both the `run()` loop and the
  `IntegratedRealtimeRunner` pull-loop. Regression tests added.
- **adk-model (OpenAI-compatible): reasoning-field fallback** — providers that
  return reasoning under a `reasoning` field are now surfaced correctly (#388).
- **adk-gemini: fix tool definition for Gemini 3 compatibility** — renamed
  `function_declarations` field to `functionDeclarations` to match the
  `camelCase` requirement of the Gemini 3 / 2.5 REST API.
- **adk-sandbox: WASM timeout isolation.** Each execution gets its own
  wasmtime engine; previously the engine was shared and any execution's
  timeout timer (including stale timers from finished runs) tripped the epoch
  deadline of every in-flight execution, causing spurious timeouts. Covered
  by a regression test.
- **adk-sandbox: the `wasm` feature compiles again.** A dependency bump had
  moved `wasmtime` to 45 while `wasmtime-wasi` stayed at 44 (two incompatible
  majors); aligned at 45.
- **adk-model: transport span no longer shadows the agent-layer `call_llm`.**
  The Gemini client's `generate_content` span shared the agent layer's name,
  so every LLM call exported as a duplicate pair. The transport span is now
  `model.generate_content`, applied uniformly across all providers (OpenAI,
  Responses, compatible presets, OpenRouter, Anthropic, DeepSeek, Groq,
  Ollama, Bedrock, Azure AI, mistral.rs).
- **Publishing can no longer deadlock on dev-deps.** Internal dev-deps are
  path-only (stripped from published manifests) — the class of failure that
  required emergency manifest surgery during the v1.0.0 release is gone.
- **docs.rs / crates.io landing pages corrected**: feature presets now match
  reality (default is `minimal`; the documented `labs` feature no longer
  exists), stale version snippets bumped (including doc headers that survived
  several releases at 0.8.2), README template/addon lists match the CLI, and
  example counts are accurate.
- **gemini_openai_compat_agent example relocated** from adk-model to adk-agent
  (it is an agent-level example; adk-model no longer needs upward dev-deps),
  and clippy violations across feature-gated and test code fixed —
  adk-model now passes `--all-features --all-targets -D warnings`.

- **MCP tasks now use the official request flow.** Tool calls send task metadata,
  receive `CreateTaskResult`, poll `tasks/get`, fetch `tasks/result`, and cancel
  the remote task when local bounds are exceeded. Required and optional task
  behavior follows negotiated server and tool capabilities.
- **MCP manager supervision and locking.** Failed restart attempts remain eligible
  until the configured limit, duplicate monitors are prevented, monitoring can
  restart after being stopped, disabled servers cannot be started directly, and
  tool discovery no longer holds the registry lock during network calls.
- **MCP HTTP authentication and session handling.** API-key headers and caller
  headers now reach the wire, OAuth client-credentials tokens are attached as
  bearer credentials, configured request timeouts are honored, and one bounded
  reinitialization can recover an expired Streamable HTTP session.
- **adk-acp permission decisions no longer trust menu order or fabricated
  option IDs.** Allow and reject choices are matched by protocol semantics and
  the original opaque ID is returned; invalid selections cancel the request.
- **ACP cancellation now cleans up the active turn without poisoning the next
  prompt.** Both `session/cancel` and JSON-RPC request cancellation are covered
  by official-SDK interoperability tests.
- **adk-core: Event serialization no longer produces duplicate `"provider_metadata"` keys.**
  `Event.provider_metadata` is now serialized as `"event_metadata"` to avoid collision
  with `LlmResponse.provider_metadata` (which is flattened into the same JSON object).
  Regression test added. (`#414`)

- **adk-realtime: preserve split PCM16 samples in the LiveKit audio bridge.**
  Incomplete channel frames are retained only for the matching response item and cleared at
  response and error boundaries, preventing malformed samples, stereo channel-phase shifts,
  data loss, and cross-item audio contamination.

- **adk-model: preserve whitespace-only text deltas when streaming through OpenAI-compatible providers.**
  Whitespace-only stream deltas and boundary whitespace are no longer dropped, so concatenating
  the emitted `Part::Text` reconstructs the source byte-exactly (Markdown, indentation, tables,
  and prose spacing are preserved). (`#441`)

## [1.0.0] - 2026-06-07

> **Note:** 0.10.0 was an internal-only release and was never published to crates.io. All changes below were shipped as part of 1.0.0.

### Breaking

- **Workspace version bump to 0.10.0.** This is a breaking (0.x major) release.
  All `adk-*` crates move from 0.9.x to 0.10.0 in lockstep.
- **adk-mistralrs: Now a workspace member.** Previously excluded due to git
  dependencies; now included since mistral.rs published to crates.io.
  Uses workspace `rust-version` (1.94.0).
- **adk-core: `LlmResponse` and `LlmRequest` gained public fields.** These structs
  are not `#[non_exhaustive]` and are constructed with struct literals downstream,
  so the additions below are breaking changes for external code that builds them
  by struct literal (use `..Default::default()`, `LlmResponse::new`, or
  `LlmRequest::new` to be forward-compatible):
  - `LlmResponse.interaction_id: Option<String>`
  - `LlmRequest.previous_response_id: Option<String>`
- **adk-gemini: `FunctionResponse` gained a public `id` field**, and the `Model`
  enum (not `#[non_exhaustive]`) gained new variants — both breaking for external
  struct-literal / exhaustive-match consumers.

### Added

- **First stable release.** All 39 workspace crates promoted to 1.0.0, committing to semantic versioning guarantees. This milestone marks ADK-Rust as production-ready with a stable public API.

- **adk-bench: benchmarking framework** — measures framework-level runtime
  performance against real LLM APIs and supports cross-framework comparison with
  the Python ADK (e.g., cold-start and simple-tool-call scenarios). Ships
  Criterion benchmarks and reproducible result tables.

- **adk-eval: Competitive parity features** — 10 new capabilities bringing the
  evaluation framework to parity with Braintrust, LangSmith, and Inspect AI:
  - **StructuredJudge** — typed verdicts (pass/fail/partial) with scores and
    reasoning via function-calling or JSON fallback. Lenient JSON extractor
    handles markdown fences, raw JSON, and embedded JSON in prose.
  - **EmbeddingScorer** (feature: `embedding`) — cosine similarity between
    embedding vectors using any `EmbeddingProvider` implementation.
  - **CostTracker** — token usage extraction from event streams, dollar cost
    estimation with per-model pricing tables (Gemini, OpenAI, Anthropic, DeepSeek).
  - **TraceAnalyzer** — detects redundant tool calls and execution loops,
    computes efficiency score (useful_calls / total_calls).
  - **BaselineStore** — save/load metric snapshots as `.eval-baseline.json`,
    detect regressions with configurable tolerance.
  - **JunitReporter** (feature: `ci-helpers`) — JUnit XML generation for native
    CI integration (GitHub Actions, Jenkins, GitLab CI).
  - **AnnotationStore** — JSONL export/import for human review workflows with
    case_id validation and unmatched entry warnings.
  - **AbComparator** (feature: `statistics`) — A/B agent comparison using Wilcoxon
    signed-rank test for paired statistical significance.
  - **TestGenerator** — LLM-driven case generation from descriptions, plus direct
    extraction from production event logs (no LLM needed).
  - **ConversationScorer** — multi-turn metrics: context retention, goal completion,
    coherence, and topic drift (via StructuredJudge or EmbeddingScorer).
  - **CLI integration** — `cargo adk eval` subcommand with `--save-baseline`,
    `--check-regression`, `--format` (table/json/junit), `--concurrency`, and
    non-zero exit on regression detection.
  - **EvaluationResult extended** — optional `cost_metrics`, `trace_analysis`, and
    `verdicts` fields (backward-compatible with `#[serde(default)]`).
  - **EvalCase extended** — optional `metadata` field for generation tracking.
  - New feature flags: `embedding`, `ci-helpers`, `statistics`.
  - Example crate: `examples/eval_showcase/` demonstrating all features.

- **adk-enterprise** — Native Rust SDK for the ADK-Rust Enterprise Managed Agent Service. Lightweight HTTP/SSE client with zero adk-* runtime dependencies. Supports any model (Gemini, OpenAI, Anthropic, DeepSeek, Ollama), auto-reconnect SSE streaming, automatic retry with exponential backoff, idempotency keys, and self-hosted deployments. (Experimental)

- **adk-graph: Functional API** (feature: `functional`) — Write agent workflows as
  normal async Rust functions with automatic checkpointing, typed state reducers,
  and interrupt/resume support. Includes:
  - `#[entrypoint]` and `#[task]` proc macros in `adk-rust-macros`
  - `TaskContext` — runtime context with state, checkpointing, streaming, interrupts
  - `ReducedValue<T>` — append-only state container persisted across checkpoints
  - `UntrackedValue<T>` — transient state container excluded from checkpoints
  - `MessagesValue` — chat messages with ID-based deduplication
  - `TypedReducer` trait with built-in reducers (Replace, Append, Merge)
  - `StateSchemaValidator` — type-level validation at workflow boundaries
  - `ExecutionLog` — task completion tracking for resume-skip behavior
  - Loop iteration checkpoint keying (`"task::iter_N"`)
  - 3 example crates: `functional_workflow`, `background_runs`, `cron_scheduling`
- **adk-server: Background Runs** (feature: `background`) — REST API for async
  workflow execution. `POST /runs` submits a workflow, `GET /runs/{id}` polls status,
  `DELETE /runs/{id}` cancels. Status transitions: queued → running → completed/failed/cancelled.
  Timeout enforcement, retry with checkpoint resume, `BackgroundRunner` orchestrator.
- **adk-server: Cron Scheduling** (feature: `background`) — Cron job management
  with REST endpoints (POST/GET/PATCH/DELETE /cron). Supports 5-field and 6-field
  cron expressions, concurrency policies (skip/allow/queue), background scheduling
  loop, pause/resume lifecycle management.
- **adk-model: `cancel_response()` for OpenAI background responses** — new method
  on `OpenAIResponsesClient` that calls `POST /v1/responses/{id}/cancel` to cancel
  a running background response. Returns an `LlmResponse` with cancelled status.
  Useful for deep research and other long-running background requests.
- **adk-model: Six new OpenAI Responses API example crates** — standalone examples
  demonstrating WebSocket transport (`openai_ws_minimal`), background mode
  (`openai_background`), Conversations API (`openai_conversations`), built-in
  tools (`openai_builtin_tools`), deep research (`openai_deep_research`), and
  Open Responses compatibility (`openai_open_responses`).
- **adk-anthropic: Claude Opus 4.8 support** — added `claude-opus-4-8` as the
  latest flagship model. Adaptive thinking only (same as Opus 4.7).
- **adk-managed: Managed Agent Runtime** (feature: `managed-runtime`) — New crate
  providing a provider-neutral, durable, resumable agent execution engine. The
  runtime takes a declarative `ManagedAgentDef`, builds a runnable agent, and
  operates it as a checkpoint-resumable, event-streaming background session.
  Key capabilities:
  - `ManagedAgentRuntime` trait and `DefaultManagedAgentRuntime` implementation
  - Durable sessions: checkpoint after every event, resume from last consistent state
  - Provider-neutral event stream (`SessionEvent`) — identical sequences across
    Gemini, OpenAI, Anthropic, Ollama, and OpenAI-compatible providers
  - Custom tool parking with configurable timeout
  - Event replay (`stream_events(from_seq)`) for SSE reconnection
  - `ScriptedLlm` test double for deterministic offline testing ($0)
  - Golden fixture conformance tests (F-1 through F-8)
  - STABILITY: Experimental, additive, feature-gated behind `managed-runtime`
- **adk-anthropic: Managed Agents API client** — full implementation of the
  Anthropic Managed Agents API, feature-gated behind `managed-agents`. Includes:
  - Agent, Environment, Session CRUD with SSE streaming
  - Custom tool flow, tool confirmation (allow/deny)
  - Vaults and credentials for MCP authentication
  - Memory stores with versioning and optimistic concurrency
  - Dreams API for memory curation (Research Preview)
  - Webhook signature verification (HMAC-SHA256)
  - Multiagent orchestration with session threads
  - Self-hosted environment work queue management
  - File upload and session resource mounting
  - 5 examples: hello, custom_tools, files, memory, multiagent
  - 47 integration tests passing against live API
- **adk-anthropic: Files API client** — upload, download, list, get, delete files
  for use with Messages API. Feature-gated behind `files`. Auto MIME type inference.
- **adk-mistralrs: Now publishable to crates.io** — switched from git dependency
  (`mistralrs = { git = "..." }`) to crates.io (`mistralrs = "0.8"`). The crate
  is now a full workspace member and can be published alongside other ADK crates.
  Supports 50+ model architectures including Gemma 4, Qwen 3.5, Llama 4, Voxtral,
  GPT-OSS, and multimodal models with text/image/audio/video input.
- **cargo-adk: `managed-agents` template** — `cargo adk new my-agent --template managed-agents`
  scaffolds a complete Anthropic Managed Agents project with `--provider` support
  for future multi-provider managed agents.
- **adk-model: Gemini OpenAI-compatible preset** — `OpenAICompatibleConfig::gemini(api_key, model)`
  targets Gemini's OpenAI-compatibility endpoint
  (`https://generativelanguage.googleapis.com/v1beta/openai`), letting callers on
  the `openai` feature use a `GEMINI_API_KEY` and a Gemini model through the
  OpenAI Chat Completions wire format (chat, streaming, function calling,
  structured output, reasoning effort). Two examples added:
  `gemini_openai_compat` (direct client) and `gemini_openai_compat_agent`
  (the compat client driving a normal `LlmAgent`/`Runner`).
- **adk-gemini: Interactions API (Beta)** — first-class support for Google's new
  Interactions API, the forward direction for the Gemini API. Gated behind the
  new `interactions` feature flag (no new dependencies; additive to the existing
  `generateContent` surface). New `adk_gemini::interactions` module with:
  - `Gemini::create_interaction()` fluent builder — single-turn, streaming
    (`step.delta` events), multimodal input, tools, structured output, and
    server-side multi-turn via `previous_interaction_id`.
  - `Gemini::get_interaction()`, `delete_interaction()`, `cancel_interaction()`
    for the stored-interaction lifecycle.
  - Typed `Step` timeline (user input, model output, thought, function call,
    function result) with forward-compatible `Step::Other` for server-side tool
    steps; polymorphic `Content`, `Tool`, and `ResponseFormat`; `Usage`,
    `InteractionStatus`, and SSE `InteractionSseEvent`/`StepDelta` types.
  - `Interaction::output_text()` and `pending_function_calls()` convenience
    accessors mirroring the official SDKs.
  - Requests pin the `Api-Revision: 2026-05-20` steps-schema contract.
  - Example: `cargo run -p adk-gemini --features interactions --example interactions_basic`.
- **adk-gemini: Managed Agents & Environments** — extends the Interactions API
  with Google's Managed Agents (Antigravity, Deep Research) and sandbox
  Environments. All code behind the existing `interactions` feature flag:
  - `InteractionBuilder::antigravity()` — one-call setup for the Antigravity
    coding agent (`agent = "antigravity-preview-05-2026"`, `store = true`).
  - `InteractionBuilder::deep_research(agent_id)` — one-call setup for Deep
    Research agents (`background = true`, `store = true`).
  - `InteractionBuilder::environment(env)` — attach a sandbox (fresh `"remote"`,
    resume by ID, or inline `EnvironmentConfig` with sources + network rules).
  - `InteractionBuilder::agent_config(config)` — attach agent-specific config
    (Deep Research thinking summaries, visualization, collaborative planning).
  - `Environment`, `EnvironmentConfig`, `EnvironmentSource` (Inline, Repository,
    Gcs), `NetworkConfig` (Disabled, Allowlist), `NetworkRule`, `TransformMap`
    wire types with full serde round-trip and `#[non_exhaustive]`.
  - `AgentConfig` enum (DeepResearch + forward-compatible `Other` variant).
  - Client-side validation: model/agent mutual exclusivity, Antigravity
    constraints (no background, no unsupported gen params, no function tools, no
    audio/video/document), Deep Research constraints (background required).
  - Managed Agent CRUD: `Gemini::create_agent()` (fluent builder),
    `list_agents()`, `get_agent(id)`, `delete_agent(id)`.
  - `Gemini::download_environment(env_id)` — download sandbox snapshot as tar.
  - `Interaction.environment_id` response field for sandbox resume.
  - `TransformMap` custom `Debug` redacts credential values in logs.
  - Example: `cargo run -p gemini-managed-agents` (4 practical agents with
    streaming, auto-cancellation, and live progress display).
- **adk-model / adk-rust: `gemini-interactions` feature** — forwards the
  Interactions API surface up the stack. `adk_model::gemini::interactions`
  re-exports the module when enabled.
- **adk-model: Gemini Interactions API runtime transport (Beta)** — a transport
  toggle on `GeminiModel` routes the standard `LlmAgent`/`Runner`/tool loop
  through Google's Interactions API instead of `generateContent`, with no new
  agent type or rewritten agent setup. Gated behind the `gemini-interactions`
  feature; `generateContent` remains the default and recommended path. Highlights:
  - `GeminiModel` builder `use_interactions_api(true)` selects the transport;
    `interaction_options(...)` configures `store`, stateful continuation,
    background mode, and poll interval (`InteractionOptions`/`BackgroundMode`).
  - Faithful API defaults: `store=true`, server-side stateful continuation (via
    `previous_interaction_id`), and `background=true` for agent targets only.
  - Restricted target allowlist (`InteractionTarget`) with `InvalidInput` errors
    naming the supported models/agents for unsupported targets.
  - `bypass_multi_tools_limit`: built-in tools (Google Search, URL context, File
    Search) can be converted to function-calling tools
    (`with_bypass_multi_tools_limit` / the `BypassMultiToolsLimit` trait) so they
    coexist with custom function tools under the Interactions API.
  - Streaming (SSE) and background-poll completion surfaced through the same
    response stream the runner consumes.
  - Example: `examples/gemini_interactions_agent/`.
- **adk-core: provider-neutral interaction continuity fields** — new fields that
  carry conversation continuity across providers (see Breaking above):
  - `LlmResponse.interaction_id: Option<String>` plus an `Event::interaction_id()`
    accessor (mirrors ADK-Python's `event.interaction_id`).
  - `LlmRequest.previous_response_id: Option<String>`, populated by `LlmAgent`
    from the most recent event's `interaction_id`; the Gemini Interactions
    transport maps it to `previous_interaction_id`, falling back to transcript
    input transparently when a stored interaction has expired.
- **adk-gemini: May 2026 GA models** — Added `Model` variants for models that
  shipped or replaced previews in May 2026:
  - `Gemini31FlashLite` (`gemini-3.1-flash-lite`, GA) — replaces the preview,
    which was shut down May 25, 2026.
  - `Gemini31FlashImage` (`gemini-3.1-flash-image`, Nano Banana 2, GA).
  - `Gemini3ProImage` (`gemini-3-pro-image`, Nano Banana Pro, GA).
  - `GeminiEmbedding2` (`gemini-embedding-2`, GA multimodal embeddings).
- **adk-gemini: pricing for Gemini 3.5 Flash** — `GeminiPricing::GEMINI_35_FLASH`
  ($1.50/MTok input, $9.00/MTok output incl. thinking).
- **adk-gemini: `GeminiPricing::for_model(&Model)`** — maps a `Model` to its
  standard per-token pricing, returning `None` for `Custom` models.
- **adk-gemini: `FunctionResponse.id` + `FunctionResponse::with_id()`** — echo the
  originating `FunctionCall` id to satisfy Gemini 3.x strict response matching
  (id + name + count). adk-model's Gemini provider now forwards the call id
  automatically.

### Deprecated

- **adk-gemini: `Model::Gemini31FlashLitePreview`** — shut down May 25, 2026;
  use `Model::Gemini31FlashLite`.
- **adk-gemini: `Model::Gemini3ProImagePreview`** — shuts down June 25, 2026;
  use `Model::Gemini3ProImage`.

### Changed

- **adk-gemini: `ThinkingLevel` docs** — clarified that `Medium` is the default
  for Gemini 3.5 Flash while `High` remains the default for Gemini 3 Flash
  Preview and Gemini 3.1 Pro, and that `temperature`/`top_p`/`top_k` are no
  longer recommended for Gemini 3.x.

## [0.9.2] - 2026-05-24

### Fixed

- **Composable template codegen** — Generated code now uses correct published API constructors (`GeminiModel::new`, `OpenAIClient::new`, etc.) instead of non-existent convenience methods. Adds proper `api_key` loading from environment variables.
- **Dead code warnings** — Scaffolded projects no longer produce unused variable/import warnings for auth, sessions, and memory addons.

### Changed

- **Default models updated to latest** — `gemini-3.5-flash`, `gpt-5.5`, `claude-sonnet-4-6`, `deepseek-v4-flash`, `gemma4` (Ollama), `qwen/qwen3.7-max` (OpenRouter), `anthropic.claude-opus-4-6-v1` (Bedrock), `gpt-5.5` (Azure AI).

### Added

- **`--model` flag** for `cargo adk new` — override the default model for any provider.
- **`Gemini35Flash` variant** in the `adk-gemini` Model enum — new default model.

## [0.9.0] - 2026-05-24

### Added

- **A2A Simple Scaffolding** — Dead-simple A2A agent creation:
  - `A2aServer::quick_start(agent)` — one-liner to expose any agent via A2A protocol
  - `A2aServer::builder()` — configurable builder for port, agent card metadata, session backend
  - `cargo adk new my-agent --template a2a` — scaffold a complete A2A project
  - `--with-yaml` flag support for YAML agent definitions
  - `a2a` feature alias in umbrella crate (`a2a = ["server", "adk-server/a2a-v1"]`)
  - `server` feature now includes `a2a-v1` automatically (standard tier gets A2A)
  - Minimal example at `examples/a2a_quickstart/`
  - Getting-started documentation at `docs/official_docs/a2a/getting-started.md`
  - Property tests for template generation, builder composition, and error clarity
  - Live integration tests against external A2A agents

- **Composable Template System** — Modular project scaffolding via `cargo adk new`:
  - 8 base templates: `basic`, `tools`, `rag`, `api`, `openai`, `a2a`, `graph`, `realtime`
  - 9 addons: composable feature modules added via the `--addon` flag (e.g., `--addon telemetry`, `--addon auth`, `--addon eval`)
  - 5 enterprise patterns: production-ready project structures for common deployment scenarios
  - `--addon` flag for combining any base template with one or more addons
  - Templates generate complete project structures with Cargo.toml, src/, tests/, and documentation

- **Cargo Adk Build** — Compile-without-deploy subcommand:
  - `cargo adk build` compiles an agent project and verifies it is deployment-ready without actually deploying
  - Validates project structure, dependencies, and configuration
  - Useful as a pre-deployment verification step in CI pipelines
  - Supports all standard `cargo build` flags (e.g., `--release`, `--features`)

### Changed

- **Version bump from 0.8.5 to 0.9.2** — All workspace crates updated to version 0.9.2. This release includes new features (composable templates, cargo adk build, A2A scaffolding), security fixes, and documentation improvements.

### Security

- **hickory-proto 0.26.1** — Updated from 0.24.x to address DNS resolution vulnerabilities that could allow cache poisoning in certain network configurations. Severity: moderate.
- **openssl 0.10.80** — Updated from 0.10.78 to fix a potential memory safety issue in certificate chain validation that could lead to incorrect trust decisions. Severity: moderate.
- **rubato 3.0** — Updated from 2.x to address an integer overflow in sample rate conversion that could cause buffer overruns when processing untrusted audio input. Severity: low.
- **similar 3** — Updated from 2.x to fix a denial-of-service vulnerability where crafted diff inputs could trigger quadratic time complexity. Severity: low.

## [0.8.5] - 2026-05-19

### Breaking

- **`DatabaseSessionService` type alias removed** (deprecated since 0.4.0). Use `SqliteSessionService` directly instead. The alias was a backward-compatibility shim that has been deprecated for 4 minor releases.

- **`RustCodeTool` struct removed** (deprecated since 0.5.0). Use `adk_code::CodeTool` instead. The struct and its module have been fully removed from `adk-tool`.

- **`RunnerConfig` and `RunConfig` are now `#[non_exhaustive]`**. Struct literal construction is no longer possible from downstream crates. Use the builder pattern instead:
  ```rust
  // RunnerConfig — use the typestate builder
  let runner = Runner::builder()
      .agent(agent)
      .session_service(session_service)
      .build();

  // RunConfig — use the builder or Default
  let run_config = RunConfig::builder()
      .input_text("Hello")
      .build();
  ```

### Changed

- **Beta-to-Stable promotions**: The following crates have been promoted from Beta to Stable tier, committing to semantic versioning guarantees:
  - `adk-server` — HTTP server (Axum) and A2A protocol
  - `adk-graph` — Graph-based workflow orchestration with checkpoints
  - `adk-memory` — Semantic memory and RAG search
  - `adk-anthropic` — Dedicated Anthropic client and tool search

### Added

- **`#![deny(missing_docs)]` enforced on all 7 original Stable-tier crates**: `adk-core`, `adk-agent`, `adk-model`, `adk-gemini`, `adk-tool`, `adk-runner`, `adk-session`. All public items now have rustdoc documentation.

- **Property tests for 4 crates**: Added proptest-based property tests (100+ iterations each) for `adk-agent` (event stream well-formedness), `adk-runner` (config builder round-trip), `adk-gemini` (serialization round-trip, thinking config validation), and `adk-tool` (schema generation, MCP round-trip).

- **CI documentation lint job**: New `docs` job in CI that runs `RUSTDOCFLAGS="-D missing_docs" cargo doc --no-deps` for all 12 Stable-tier crates, failing on any undocumented public items.

## [0.8.4] - 2026-05-18

### Fixed

- **Gemini schema: array types require `items`**: Fixed "missing field" errors from Gemini API when array-typed properties had tuple validation `items` (JSON array). Instead of stripping `items` entirely (which left arrays without the required field), tuple `items` are now converted to a single schema using the first element. Arrays without any `items` field also get a default `{"type": "string"}` added.

## [0.8.3] - 2026-05-18

### Fixed

- **Gemini schema `items` tuple validation error**: Fixed 400 errors from Gemini API when MCP tools declared `items` using JSON array syntax (tuple validation). Gemini's Schema proto only supports `items` as a single schema object for array element types. Tuple validation syntax (`items: [{...}, {...}]`) is now stripped.

- **Comprehensive unsupported keyword stripping**: Expanded the Gemini schema adapter's unsupported keywords list from 7 to 32 entries based on the official Gemini API documentation. The Schema proto only supports `type`, `description`, `enum`, `items`, `properties`, `required`, `nullable`, and `format` (limited values). All other JSON Schema keywords are now stripped, including:
  - Numeric constraints: `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `multipleOf`
  - String constraints: `minLength`, `maxLength`, `pattern`
  - Array constraints: `minItems`, `maxItems`, `uniqueItems`, `contains`, `prefixItems`
  - Object constraints: `minProperties`, `maxProperties`, `dependentRequired`, `dependentSchemas`
  - Annotations: `title`, `default`, `deprecated`, `examples`, `readOnly`, `writeOnly`
  - Content: `contentMediaType`, `contentEncoding`
  - Meta: `$id`

## [0.8.2] - 2026-05-16

### Added

- **Provider-aware schema normalization**: MCP tool schemas are now normalized per-provider at request time instead of applying Gemini-specific transforms universally at tool registration. Each LLM adapter (Gemini, OpenAI, Anthropic, etc.) normalizes schemas according to its own backend requirements.
  - `SchemaAdapter` trait in `adk-core` — common interface for schema normalization
  - `GeminiSchemaAdapter` — full destructive transforms (resolves `$ref`, collapses combiners, enforces depth limits)
  - `OpenAiStrictSchemaAdapter` — preserves `$ref`/`$defs`/`anyOf`, adds `additionalProperties: false`
  - `OpenAiSchemaAdapter` — minimal safe fixes for non-strict mode
  - `AnthropicSchemaAdapter` — near pass-through (only strips `$schema` and conditionals)
  - `GenericSchemaAdapter` — conservative default for unknown providers (Ollama, etc.)
  - `SchemaCache` — thread-safe normalized schema cache keyed by content hash
  - `Llm::schema_adapter()` method with default returning `GenericSchemaAdapter`
  - Shared utility module `schema_utils` with composable transform functions
  - Tool name truncation to 64 bytes at valid UTF-8 character boundaries
  - Vertex AI surface variant (sets `additionalProperties: false` instead of removing it)

- **Example**: `examples/schema_normalization/` — demonstrates all adapters normalizing the same schema differently. No API keys needed.

### Changed

- **McpToolset returns raw schemas**: `McpToolset::tools()` now returns unmodified `inputSchema` from MCP servers. Schema normalization happens at request time in each model adapter.
- **Removed `sanitize_schema`**: The monolithic Gemini-specific `sanitize_schema` function has been removed from `adk-tool`. All normalization is now provider-specific.

- **ACP Server** (`adk-acp`, feature `server`): Expose ADK agents as ACP-compatible agents for IDE connections.
  - `AcpServer::run(config)` → `AcpServerHandle` (programmatic API)
  - `AcpServerConfig` + builder with validation
  - `AcpSessionHandler` — session registry, prompt routing via Runner
  - `ResponseStreamer` — ADK Event → SessionNotification (text, tool calls, thoughts)
  - `PermissionBridge` — bidirectional ADK ↔ ACP permission flow with timeout
  - `StdioTransport` — newline-delimited JSON over stdin/stdout for IDE connections
  - `HttpTransport` — stub for future HTTP/SSE remote deployments

- **YAML Agent Config enhancements** (`adk-server`, feature `yaml-agent`):
  - Environment variable interpolation (`${VAR}` and `${VAR:-default}`)
  - Plugin references in YAML definitions
  - Session/memory backend configuration in YAML
  - Round-trip serialization (`serialize_definition()`)

- **Examples**: `examples/plugin_system/`, `examples/retry_reflect/`, `examples/acp_server/`, `examples/schema_normalization/`

### Fixed

- MCP tools with `$ref`, `anyOf`, `oneOf`, `allOf` now work correctly with OpenAI and Anthropic (previously these were destroyed by Gemini-specific sanitization)
- Tools with `const` keywords now work with Anthropic (previously converted to `enum` for all providers)
- Tools with non-standard `format` values now work with Anthropic (previously stripped for all providers)

## [0.8.1] - 2026-05-13

### Added

- **adk-acp**: New crate for Agent Client Protocol (ACP) integration. Connect ADK agents to external ACP agents (Claude Code, Codex, Kiro CLI, etc.) as tools.
  - `AcpAgentTool` — wraps any ACP agent as an ADK `Tool` for task delegation
  - `AcpToolset` — multiple ACP agents as a single `Toolset`
  - `prompt_agent()` — low-level prompt/response function
  - Auto-approve mode for permission requests
  - Uses published `agent-client-protocol` + `agent-client-protocol-tokio` crates
  - Feature-gated via `acp` on the umbrella crate

- **cargo-adk deploy**: New `cargo adk deploy` subcommand for pushing agents to ADK Platform
  - Authenticates via --token, cached credentials, or ephemeral login
  - Uploads secrets from .env matching manifest [[secrets]] declarations
  - Creates .tar.gz bundles with correct paths (no ./ prefix)
  - Supports --dry-run for CI validation without pushing
  - Convention: UPPER_SNAKE_CASE env vars map to lower-kebab-case secret keys

### Fixed

- **MCP schema sanitization for Gemini**: `sanitize_schema` now strips `exclusiveMinimum`/`exclusiveMaximum`, collapses `"type": ["string", "null"]` to `"type": "string"`, and removes `items` on non-array types. Fixes Gemini API rejections when using computer-use or playwright MCP tools.

## [0.8.0] - 2026-04-28

### Breaking Changes

- **Default feature changed from `standard` to `minimal`**: `adk-rust = "0.8.0"` now activates only `agents`, `models`, `gemini`, `runner`, and `sessions`. Add `standard` for production support crates, and opt into provider features such as `openai` and `anthropic` explicitly. For the production preset:
  ```toml
  adk-rust = { version = "0.8.0", features = ["standard"] }
  ```

### Changed

#### Dependency Diet — New Feature Tiers

Restructured the `adk-rust` umbrella crate feature tiers to reduce default build times. A hello-world agent now compiles ~165 fewer crates.

| Tier | What's included | Use case |
|------|----------------|----------|
| `minimal` (default) | agents, models, gemini, runner, sessions | Fast Gemini starter agents |
| `standard` | minimal + openai, anthropic, tools, memory, telemetry, skills, graph, auth, server, eval, guardrail, plugin, artifacts | Production deployment |
| `enterprise` | standard + realtime, browser, rag, payments, awp | Full-featured production |
| `audio` | adk-audio (STT/TTS/desktop) | Voice agents (composable add-on) |
| `code` | adk-code + adk-sandbox | Code execution (composable add-on) |
| `full` | enterprise + audio + code + sandbox | Everything |

- **Removed `labs` tier** — replaced by composable add-ons (`audio`, `code`, `sandbox`) that can be mixed with any tier
- **Added `enterprise` tier** — between `standard` and `full` for production deployments needing realtime, browser, RAG, payments, and AWP
- **Added `awp` feature** — `adk-awp` is now available through the umbrella crate

## [0.7.0] - 2026-04-18

### Added

#### Agentic Web Protocol (`awp-types`, `adk-awp`)

Two new workspace crates implementing the Agentic Web Protocol (AWP) — a protocol for making websites and services natively accessible to AI agents.

- **awp-types**: Pure protocol types with zero `adk-*` dependencies. Includes `TrustLevel` (Anonymous/Known/Partner/Internal), `RequesterType` (Human/Agent), `AwpVersion` with compatibility checks, `AwpError` with HTTP status mapping, `AwpRequest`/`AwpResponse` envelopes, `A2aMessage`/`A2aMessageType`, `AwpDiscoveryDocument`, `CapabilityManifest` (JSON-LD), `BusinessContext` with full schema (business identity, brand voice, products, channels, payments, support, content, reviews, outreach), `PaymentIntent`/`PaymentIntentState`/`PaymentPolicy` for owner-policy-driven payments, `AwpMessageType` with 9 typed message categories for agent routing.
- **adk-awp**: Full protocol implementation with axum 0.8. Includes `BusinessContextLoader` with hot-reload via ArcSwap, discovery document and capability manifest generation, requester type detection (human vs agent from headers), trust level assignment, `InMemoryRateLimiter` with per-trust-level sliding window (30/120/600/unlimited), `InMemoryConsentService` and `FileConsentService` (JSON file-backed for GDPR/KPA compliance), `InMemoryEventSubscriptionService` with HMAC-SHA256 webhook signing, `HealthStateMachine` (Healthy/Degrading/Degraded) with event emission, AWP version negotiation middleware, `awp_routes()` returning 7 Axum endpoints, `AwpStateBuilder` with sensible defaults.
- **AWP Endpoints**: `GET /.well-known/awp.json` (discovery), `GET /awp/manifest` (JSON-LD capabilities), `GET /awp/health` (health state), `POST /awp/events/subscribe`, `GET /awp/events/subscriptions`, `DELETE /awp/events/subscriptions/{id}`, `POST /awp/a2a` (A2A messages).
- **examples/awp_agent**: Standalone example with LLM agent serving AWP-compliant endpoints. Loads `business.toml`, derives agent instructions from business context, exercises all endpoints.
- **docs/official_docs/deployment/awp.md**: Dedicated AWP documentation section covering architecture, quick start, all endpoints, trust levels, rate limiting, version negotiation, events, health, consent, message types, payment intents, and full `business.toml` schema reference.

#### Video Avatar Providers (`adk-realtime`)

- **adk-realtime**: Added `AvatarProvider` trait (object-safe, Send+Sync+Debug) with `start_session`, `stop_session`, `push_audio`, `is_active` methods.
- **adk-realtime**: Added `HeyGenProvider` — REST API session management + LiveKit audio publishing. Feature flag: `heygen-avatar`.
- **adk-realtime**: Added `DIDProvider` — REST API session management + WebRTC signaling. Feature flag: `did-avatar`.
- **adk-realtime**: Wired avatar providers into `RealtimeAgent` event loop with session lifecycle, audio routing, keep-alive, and graceful degradation.
- **adk-realtime**: HTTPS enforcement on all avatar provider API URLs (CodeQL fix).
- **adk-realtime**: Feature flags: `heygen-avatar`, `did-avatar`, `video-avatar` (both).
- **examples/video_avatar**: HeyGen and D-ID avatar provider examples.

#### GeminiModel Thinking Config (`adk-model`)

- **adk-model**: Added `with_thinking_config()` and `set_thinking_config()` to `GeminiModel`, matching OpenAI (`reasoning_effort`) and Anthropic (`thinking_mode`) patterns.
- **adk-model**: Re-exported `ThinkingConfig` and `ThinkingLevel` from `adk_model::gemini`.

### Changed

- **adk-awp**: Upgraded from axum 0.7 to axum 0.8, aligning with adk-server, adk-auth, and adk-payments.

### Fixed

- **adk-agent**: Fixed streaming chunk bloat in `SequentialAgent`/`LoopAgent` history. Consecutive same-role streaming chunks are now consolidated into a single `Content` entry instead of creating N separate entries that bloat LLM context for subsequent agents.
- **adk-realtime**: Enforced HTTPS on all avatar provider API URLs (CodeQL cleartext transmission fix).
- **examples/secret_provider**: Removed all secret-derived output from print statements (CodeQL cleartext logging fix).

#### DeepSeek V4 Provider (`adk-model`)

- **adk-model**: Added `DeepSeekConfig::v4_pro()` and `DeepSeekConfig::v4_flash()` constructors for DeepSeek V4 models.
- **adk-model**: Added `ThinkingMode` enum (`Enabled`/`Disabled`) replacing the boolean `thinking_enabled` toggle. Can now explicitly disable thinking on V4 models that default to enabled.
- **adk-model**: Added `ReasoningEffort` enum (`High`/`Max`) for controlling thinking depth via the `reasoning_effort` request parameter.
- **adk-model**: Added strict tool mode support (beta) — `with_strict_tools()` adds `"strict": true` to tool definitions.
- **adk-model**: Added beta base URL support — `with_beta()` for prefix completion, FIM, and strict tools.
- **adk-model**: Added `DEEPSEEK_ANTHROPIC_API_BASE` constant for Anthropic API compatibility.
- **adk-model**: Exposed `prompt_cache_miss_tokens` in usage metadata.
- **adk-model**: Updated default DeepSeek model from `deepseek-chat` to `deepseek-v4-flash`.
- **adk-model**: Full backward compatibility — `chat()` and `reasoner()` constructors unchanged.
- **examples/deepseek_v4**: New standalone example demonstrating all 7 V4 API features (flash, thinking high/max, tool calls with thinking, thinking disabled, multi-turn, legacy compat).

### Security

- **rustls-webpki**: Updated 0.103.10 → 0.103.13 (fixes DoS via panic on malformed CRL BIT STRING).
- **openssl**: Updated 0.10.76 → 0.10.78 (fixes buffer overflow, unchecked callback length, incorrect bounds assertion, OOB read in PEM callback).
- **rand**: Updated 0.8.5 → 0.8.6 (fixes unsound behavior with custom logger).
- **examples**: Updated all 34 standalone example lockfiles with patched openssl and rustls-webpki.

#### Project-Scoped Memory (`adk-memory`, `adk-core`)

Optional `project_id` dimension for memory isolation. Memories are now scoped by `(app_name, user_id, project_id?)` — global entries (no project) are visible everywhere, project entries are isolated to their project.

- **adk-memory**: Added `project_id: Option<String>` field with `#[serde(default)]` to `SearchRequest`. Existing callers that construct `SearchRequest` must add `project_id: None` to the struct literal (the field has no default in struct construction). JSON deserialization is backward-compatible via `#[serde(default)]`.
- **adk-memory**: Added `validate_project_id()` — rejects empty strings and strings over 256 characters.
- **adk-memory**: Added `MemoryService::add_session_to_project()` — store session entries scoped to a project. Default delegates to `add_session`.
- **adk-memory**: Added `MemoryService::add_entry_to_project()` — store a single entry scoped to a project. Default delegates to `add_entry`.
- **adk-memory**: Added `MemoryService::delete_entries_in_project()` — delete entries matching a query within a project. Default delegates to `delete_entries`.
- **adk-memory**: Added `MemoryService::delete_project()` — delete all entries for a project. Default returns "not implemented" error.
- **adk-memory**: Added `MemoryServiceAdapter::with_project_id()` builder — binds a project scope so all `search`/`add`/`delete` operations go through project-scoped methods.
- **adk-memory**: All six backends (InMemory, SQLite, PostgreSQL, Redis, MongoDB, Neo4j) implement project-scoped storage, search isolation, and deletion.
- **adk-memory**: SQLite, PostgreSQL, MongoDB, and Neo4j backends include migration v2 for the `project_id` column/index/property.
- **adk-core**: Added `Memory::search_in_project(query, project_id)` — search within a project scope. Default delegates to `search`.
- **adk-core**: Added `Memory::add_to_project(entry, project_id)` — add an entry to a project scope. Default delegates to `add`.
- **examples/project_scoped_memory**: New standalone example demonstrating all project-scoped memory capabilities.

### Breaking Changes (minor)

- **adk-memory**: `SearchRequest` now has a `project_id: Option<String>` field. Code that constructs `SearchRequest` via struct literal must add `project_id: None`. This does **not** affect JSON deserialization (the field defaults to `None` via `#[serde(default)]`).
- **adk-memory**: `InMemoryMemoryService::delete_entries()` now only deletes global entries (entries with `project_id = None`). Previously it deleted all matching entries regardless of scope. Use `delete_entries_in_project()` to delete project-scoped entries.

#### MCP Server Lifecycle Management (`adk-tool`)

- **adk-tool**: Added `McpServerManager` for managing the full lifecycle of multiple local MCP server child processes. Spawns processes via `TokioChildProcess`, connects them into `McpToolset` instances, monitors health, auto-restarts on crash with exponential backoff, and aggregates tools from all managed servers behind the `Toolset` trait.
- **adk-tool**: Added `McpServerConfig` and `RestartPolicy` types for server configuration, compatible with Kiro's `mcp.json` format via `#[serde(rename_all = "camelCase")]`.
- **adk-tool**: Added `ServerStatus` enum (`Running`, `Stopped`, `Crashed`, `Restarting`, `Disabled`, `FailedToStart`) for lifecycle state tracking.
- **adk-tool**: Added `from_json()` and `from_json_file()` constructors for loading server configs from Kiro `mcp.json` format.
- **adk-tool**: Added `start_all()` for concurrent startup of all non-disabled servers with per-server result reporting.
- **adk-tool**: Added `add_server()` and `remove_server()` for dynamic server management at runtime.
- **adk-tool**: Added `start_monitoring()` / `stop_monitoring()` for background health checks with auto-restart using exponential backoff.
- **adk-tool**: Added tool name collision resolution — duplicate names across servers are prefixed with `{server_id}__{tool_name}`.
- **adk-tool**: Added `shutdown()` for graceful shutdown of all managed servers (cancel token → grace period → force-kill).
- **examples/mcp_manager**: New standalone example crate demonstrating `McpServerManager` with JSON config loading, tool aggregation, dynamic add/remove, and graceful shutdown.

#### Agent Interruption API (`adk-runner`)

- **adk-runner**: Added `Runner::interrupt(session_id)` for cancelling a running agent mid-execution. Preserves events already produced, stops future processing within 1 event cycle. Returns `true` if a running session was found.
- **adk-runner**: Added `Runner::active_session_ids()` to list currently running sessions.
- **adk-runner**: Per-session cancellation tokens — each `run()` call gets its own token, composable with the global `cancellation_token` from `RunnerConfig`.

#### `ServerBuilder` API (`adk-server`)

- **adk-server**: Added `ServerBuilder` for registering custom Axum controllers alongside built-in routes with shared middleware (auth, CORS, tracing, timeout, security headers). Methods: `add_api_routes()`, `add_root_routes()`, `with_a2a()`, `build()`.
- **adk-server**: Added `ShutdownHandle` and `POST /api/shutdown` endpoint for graceful shutdown. Enable via `ServerBuilder::enable_shutdown_endpoint()`. The server stops accepting new connections, completes in-flight requests, and exits cleanly — preventing data corruption in SQLite/WAL.
- **examples/server_builder**: New standalone example crate demonstrating `ServerBuilder` with custom controllers and graceful shutdown endpoint.

#### v0.7.0 Feature Examples

#### Desktop Audio Pipeline (`adk-audio`)

Cross-platform desktop audio I/O behind the `desktop-audio` feature flag. Three new components — `AudioCapture`, `AudioPlayback`, and `VadTurnManager` — provide microphone capture, speaker playback, and VAD-driven turn-taking. All components produce and consume the existing `AudioFrame` type and integrate with `AudioPipelineBuilder`.

- **adk-audio**: Added `desktop-audio` feature flag (`desktop-audio = ["dep:cpal", "vad"]`). Intentionally excluded from the `all` feature to avoid pulling `cpal` into CI builds without audio hardware.
- **adk-audio**: Added `AudioError::Device(String)` variant gated behind `#[cfg(feature = "desktop-audio")]` for system audio device errors.
- **adk-audio**: Added `AudioDevice` struct — opaque device descriptor with `id()` and `name()` accessors, shared by capture and playback.
- **adk-audio**: Added `CaptureConfig` struct with `sample_rate`, `channels`, `frame_duration_ms` fields and `validate()` method that rejects zero values.
- **adk-audio**: Added `AudioCapture` struct — microphone capture via `cpal`. Methods: `list_input_devices()`, `start_capture(device_id, config) -> AudioStream`, `stop_capture()`. Produces `AudioFrame` values through a bounded mpsc channel (capacity 64).
- **adk-audio**: Added `AudioStream` type alias (`tokio::sync::mpsc::Receiver<AudioFrame>`).
- **adk-audio**: Added `AudioPlayback` struct — speaker playback via `cpal`. Methods: `list_output_devices()`, `play(device_id, frame)`, `stop()`. Accepts `AudioFrame` values in PCM-16 LE format.
- **adk-audio**: Added `VadMode` enum (`HandsFree`, `PushToTalk`) and `VadConfig` struct with `validate()` method.
- **adk-audio**: Added `VoiceActivityEvent` enum (`SpeechStarted`, `SpeechEnded { duration_ms }`) for turn-taking events.
- **adk-audio**: Added `VadTurnManager` struct — consumes `AudioStream`, applies `VadProcessor::is_speech()`, emits `VoiceActivityEvent` via callback. HandsFree mode detects speech boundaries using configurable thresholds. PushToTalk mode suppresses automatic events.
- **adk-audio**: Extended `AudioPipelineBuilder` with `capture()` and `playback()` builder methods gated behind `desktop-audio`.
- **adk-audio**: All new types are `Send + Sync` for use across Tokio tasks.
- **examples/desktop_audio**: New standalone example crate with 6 binaries:
  - `list-devices` — enumerate input/output audio devices
  - `capture-audio` — capture 3 seconds of microphone audio
  - `playback-audio` — play silence through speakers
  - `vad-turn-taking` — HandsFree VAD with speech start/end events
  - `voice-agent` — full conversational voice agent: Mic → GeminiStt → LlmAgent (gemini-2.5-flash) → GeminiTts → Speaker. Uses real Gemini cloud providers, no mocks.
  - `config-validation` — demonstrate config validation and error handling

Eleven standalone example crates, each demonstrating one v0.7.0 feature. All are standalone crates in `examples/` with `[workspace]` key, path dependencies to workspace crates, consistent boilerplate (dotenvy, tracing, banner, env validation), and full README documentation.

- **examples/yaml_agent**: YAML agent definition loading — `AgentConfigLoader::load_file()`, `load_directory()`, sub-agent cross-references, validation error handling
- **examples/agent_registry**: Agent Registry REST API — in-process Axum server, CRUD operations on `AgentCard` entries, tag filtering (no LLM required)
- **examples/mcp_sampling**: MCP Sampling — `McpToolset::with_sampling_handler()`, `LlmSamplingHandler`, server-side `sampling/createMessage` flow
- **examples/secret_provider**: Secret Provider — custom `SecretProvider` impl, `CachedSecretProvider` with TTL, `SecretServiceAdapter` bridge, error categories
- **examples/slack_toolset**: Slack Toolset — `SlackToolset::new(token)`, dry-run mode, `slack_send_message`/`slack_read_channel`/`slack_add_reaction`
- **examples/bigquery_toolset**: BigQuery Toolset — `BigQueryToolset::with_project()`, dry-run mode, dataset/table/schema discovery, SQL execution
- **examples/spanner_toolset**: Spanner Toolset — `SpannerToolset::new()`, dry-run mode, table listing, schema inspection, SQL execution
- **examples/user_personas**: User Personas — `PersonaRegistry::load_directory()`, `UserSimulator`, multi-turn persona-driven conversations
- **examples/prompt_optimizer**: Prompt Optimizer — `PromptOptimizer`, eval set loading, iterative instruction improvement, early stopping
- **examples/video_avatar**: Video Avatar — `AvatarConfig` builder, `RealtimeAgentBuilder::avatar()`, JSON serialization, graceful fallback (no LLM required)
- **examples/intra_compaction**: Intra-Compaction — `IntraCompactionConfig`, `LlmEventSummarizer`, `estimate_tokens()`, overlap preservation, coherence after compaction

#### OS Sandbox Profiles (`adk-sandbox`)

Platform-native OS-level sandbox enforcement for child processes. Restricts filesystem access, blocks network, and controls process spawning at the kernel level.

- **adk-sandbox**: Added `SandboxPolicy`, `SandboxPolicyBuilder`, `SandboxEnforcer` trait, `WrappedCommand`, and `get_enforcer()` registry function
- **adk-sandbox**: Added `MacOsEnforcer` — Seatbelt enforcement via `sandbox-exec` with "allow default, deny dangerous" strategy
- **adk-sandbox**: Added `LinuxEnforcer` — bubblewrap enforcement via `bwrap` with namespace-based isolation
- **adk-sandbox**: Added `WindowsEnforcer` — AppContainer stub (Win32 API implementation deferred)
- **adk-sandbox**: Extended `ProcessBackend` with `with_sandbox(config, enforcer, policy)` for optional OS-level enforcement
- **adk-sandbox**: Added 3 new `SandboxError` variants: `EnforcerFailed`, `EnforcerUnavailable`, `PolicyViolation`
- **adk-sandbox**: Added feature flags: `sandbox-macos`, `sandbox-linux`, `sandbox-windows`, `sandbox-native`
- **adk-gemini**: Added `Gemini31FlashLitePreview` model variant (`gemini-3.1-flash-lite-preview`)
- **examples/sandbox_agent**: New LLM-agent-driven example demonstrating sandboxed Python code execution with network blocking

#### Text-Based Tool Call Parser (`adk-model`)

Automatic detection and parsing of tool calls embedded in text responses from models that don't use native function calling. Enables tool use with open-weight models served via Ollama, vLLM, and other OpenAI-compatible endpoints.

- **`parse_text_tool_calls()`** (`adk-model`): Parses text containing tool call markup into `Part::FunctionCall` items. Supports 7 model-family formats:
  - **Qwen/Hermes**: `<tool_call>{"name":"...","arguments":{...}}</tool_call>`
  - **Qwen-Coder**: `<tool_call><function=NAME>ARGS</function></tool_call>`
  - **Llama**: `<|python_tag|>{"name":"...","parameters":{...}}`
  - **Mistral Nemo**: `[TOOL_CALLS][{"name":"...","arguments":{...}}]`
  - **DeepSeek**: JSON fences with `<｜tool▁call▁end｜>` (full-width Unicode delimiters)
  - **Gemma 4**: `<|tool_call>call:NAME{...}<tool_call|>` (non-JSON custom escaping)
  - **Action tags**: `<|action_start|>{"name":"...","arguments":{...}}<|action_end|>`
- **`ToolCallBuffer`** (`adk-model`): Streaming token buffer that detects tool call prefixes mid-stream, accumulates tokens until the closing tag, then parses and emits `Part::FunctionCall`. Falls back to `Part::Text` on parse failure.
- **`contains_tool_call_tag()`** (`adk-model`): Quick check for tool call markers in text without full parsing.
- **OpenAI-compatible streaming integration**: `ToolCallBuffer` wired into the OpenAI-compatible streaming path, automatically converting text-embedded tool calls to native function calls during streaming.
- **OpenAI non-streaming integration**: `parse_text_tool_calls()` applied to non-streaming responses from OpenAI-compatible endpoints.
- **Ollama non-streaming integration**: `parse_text_tool_calls()` applied to Ollama text responses.
- **Ollama streaming with tools**: Enabled streaming for Ollama tool-calling requests (previously forced non-streaming). Ollama's API now supports streaming with tool calls natively.
- **22 unit tests** covering all 7 formats, mixed text+tool content, multiple tool calls, malformed input, and streaming buffer behavior.

#### Ollama Qwen Example (`examples/ollama_qwen/`)

- Standalone example crate demonstrating three scenarios with Qwen 3.6 / 3.5 / Qwen3-Coder on Ollama:
  1. **Thinking/reasoning**: Extended thinking with `<think>` blocks
  2. **Native Ollama tool calling**: Ollama's built-in tool call API
  3. **OpenAI-compat tool calling**: Text-based tool call parsing via the OpenAI-compatible endpoint
- Model configurable via `OLLAMA_MODEL` env var (default: `qwen3.5`)
- README documents all 7 supported text-based tool call formats

#### Claude Opus 4.7 Support (`adk-anthropic`, `adk-model`)

Day-one support for Anthropic's Claude Opus 4.7 (released April 16, 2026):

- **`KnownModel::ClaudeOpus47`** (`adk-anthropic`): New variant for `claude-opus-4-7` wire format with serde round-trip support.
- **`EffortLevel::XHigh`** (`adk-anthropic`): New effort level between `High` and `Max` — recommended for coding and agentic workflows on Opus 4.7.
- **`ModelPricing::OPUS_47`** (`adk-anthropic`): Pricing constant at $5/$25 per MTok (same as Opus 4.6).
- **`Effort::XHigh`** (`adk-model`): New variant in the adk-model Anthropic config, mapped to `adk_anthropic::EffortLevel::XHigh`.
- **Documentation updates**: ThinkingConfig, OutputConfig, and README updated to document Opus 4.7 breaking changes (adaptive thinking only, no `budget_tokens`/`temperature`/`top_p`, updated tokenizer).

### Fixed

- **`cargo fmt` compliance** (`adk-model`): Fixed formatting in `tool_call_parser.rs` and module ordering in `lib.rs`.

### Security

- **thin-vec CVE fix**: Upgraded `thin-vec` 0.2.14 → 0.2.16 (use-after-free in `IntoIter::drop` when element drop panics).

## [0.6.0] - 2026-04-12

### Breaking Changes

- **`build_v1_agent_card()`** now requires an `AgentCapabilities` parameter (was hardcoded to default). Pass `AgentCapabilities::none()` for previous behavior.
- **`TaskStore` trait** gains `find_task_by_context()` method. Custom implementors must add this method.
- **`PushNotificationSender` trait** methods gain `config: &TaskPushNotificationConfig` parameter.
- **`message_stream()` and `tasks_subscribe()` return type** changed from `BoxStream<Result<TaskStatusUpdateEvent>>` to `BoxStream<Result<StreamResponse>>`.
- **`CallbackContext` trait** gains `shared_state()` default method (returns `None` — no action needed for existing implementors).

### Added

#### A2A v1.0.0 Protocol Compliance (`adk-server`)

Nine compliance fixes bringing the A2A implementation to full conformance with the A2A Protocol v1.0.0 specification:

- **RFC 3339 timestamps** (`executor.rs`): All `TaskStatus` objects now include ISO 8601 timestamps via `TaskStatus::with_timestamp()`.
- **Agent capabilities declaration** (`card.rs`): `build_v1_agent_card()` accepts an `AgentCapabilities` parameter.
- **Input validation** (`request_handler.rs`): `validate_message()` and `validate_id()` reject malformed inputs.
- **Content-Type header** (`jsonrpc_handler.rs`): `Content-Type: application/a2a+json` on all non-streaming responses.
- **Context-scoped task lookup** (`task_store.rs`): `find_task_by_context()` on `TaskStore` trait.
- **Message ID idempotency** (`request_handler.rs`): Duplicate requests return previously created task.
- **Push notification authentication** (`push.rs`): Bearer and token headers on webhook deliveries.
- **INPUT_REQUIRED multi-turn flow** (`request_handler.rs`): Resume existing tasks via `contextId`.
- **Streaming first event** (`stream.rs`): Task object as first SSE event per spec §3.1.2.
- **A2A examples**: `a2a-research-agent` and `a2a-writing-agent` with full client validation.
- **Wire types**: Powered by Foundation-verified [`a2a-protocol-types`](https://crates.io/crates/a2a-protocol-types) v0.5 by [@tomtom215](https://github.com/tomtom215).

#### ParallelAgent SharedState (`adk-core`, `adk-agent`, `adk-runner`)

Thread-safe key-value store for parallel agent coordination:

- **`SharedState`** (`adk-core`): Concurrent `HashMap` with `set_shared`, `get_shared`, and `wait_for_key` (timeout-based blocking via `tokio::sync::Notify`).
- **`SharedStateError`** (`adk-core`): Dedicated error type with `EmptyKey`, `KeyTooLong`, `Timeout`, `InvalidTimeout` variants.
- **`shared_state()` on `CallbackContext`** (`adk-core`): Default method returning `None` for backward compatibility.
- **`SharedStateContext`** (`adk-agent`): Context wrapper injecting `SharedState` into the context chain.
- **`ParallelAgent::with_shared_state()`** (`adk-agent`): Opt-in builder method creating fresh `SharedState` per `run()`.
- **`AgentToolContext` delegation** (`adk-agent`): Tools can now access `shared_state()` through the full context chain.
- **`InvocationContext` delegation** (`adk-runner`): Runner context propagates `shared_state()`.
- **Example crate** (`examples/parallel_shared_state/`): Basic and LLM-powered workbook coordination pattern.

#### Tool Authorization Documentation

- **Tool authorization guide** (`docs/official_docs/security/tool-authorization.md`): `ToolConfirmationPolicy` (HITL), `BeforeToolCallback`, RBAC, graph interrupts with CLI and web server examples.

#### Multimodal Function Responses (`adk-core`, `adk-gemini`, `adk-model`, `adk-agent`)

Tools can now return images, audio, PDFs, and file references alongside JSON in function responses to Gemini 3 models:

- **`InlineDataPart` / `FileDataPart`** (`adk-core`): New types for binary data (MIME type + bytes) and file references (MIME type + URI).
- **`FunctionResponseData` multimodal fields** (`adk-core`): `inline_data: Vec<InlineDataPart>` and `file_data: Vec<FileDataPart>` with serde skip-when-empty for backward compatibility.
- **`FunctionResponseData::from_tool_result()`** (`adk-core`): Automatically extracts `inline_data`/`file_data` from a tool's JSON return value.
- **`FunctionResponseData` constructors** (`adk-core`): `with_inline_data()`, `with_file_data()`, `with_multimodal()` for direct construction.
- **`FunctionResponse.parts`** (`adk-gemini`): Nested `parts` array inside the `functionResponse` wire object matching the Gemini 3 API format.
- **`FunctionResponsePart`** (`adk-gemini`): Enum for `InlineData` and `FileData` entries nested inside function responses.
- **`FileDataRef`** (`adk-gemini`): Wire-format struct for file references with camelCase serialization.
- **`Part::FileData`** (`adk-gemini`): New variant in the Gemini Part enum for file data references.
- **`Content::function_response_multimodal()`** (`adk-gemini`): Constructor for multimodal function response content.
- **`ContentBuilder::with_function_response_multimodal()`** (`adk-gemini`): Builder method for multimodal function responses.
- **Conversion layer** (`adk-model`): Base64-encodes inline data and maps file references into nested `FunctionResponse.parts` for the Gemini wire format.
- **Agent pipeline** (`adk-agent`): Uses `from_tool_result()` for tool results and `AfterToolCallbackFull` results, enabling tools to return multimodal data.
- **Example** (`examples/multimodal_function_response/`): Chart tool (PNG + JSON) and document tool (file URI + JSON) with Gemini 3.

#### Gemini 3 Function Calling Compliance (`adk-gemini`, `adk-model`)

Four additions bringing `adk-gemini` to full compliance with the Gemini function calling specification:

- **`VALIDATED` function calling mode** (`adk-gemini`): New `FunctionCallingMode::Validated` variant for schema validation without forced calling (Gemini 3 series).
- **`allowed_function_names`** (`adk-gemini`): `FunctionCallingConfig` now supports restricting which functions the model may call when mode is `Any`.
- **Function call `id` field** (`adk-gemini`): `FunctionCall` struct now includes an optional `id` field for Gemini 3 series models that return unique identifiers per call.
- **`id` propagation** (`adk-model`): Gemini conversion layer propagates function call `id` between `adk-core` and `adk-gemini` types in both directions.

#### Crate Adoption Feedback (GitHub issue #262)

Five adoption fixes reported by a real-world integrator (zavora-cli):

- **SQLx lifetime fix** (`adk-memory`): `SqliteMemoryService` pool cloning for `#[async_trait]` compatibility.
- **Tool context in callbacks** (`adk-core`, `adk-agent`, `adk-realtime`): `tool_name()` and `tool_input()` on `CallbackContext`.
- **Composable telemetry layer** (`adk-telemetry`): `build_otlp_layer()` for custom subscriber stacks.
- **Developer-friendly content filter** (`adk-guardrail`): `harmful_content_strict()` variant.
- **PluginBuilder documentation** (`adk-plugin`): Expanded rustdoc with examples.

#### Realtime Improvements ([@mikefaille](https://github.com/mikefaille))

- **Gemini 3.1 Live API**: Multiple parts support in Gemini Live sessions.
- **Realtime optimizations**: Concurrency improvements, audio hot path documentation.

### Fixed

- **Sandbox dependency discovery** (`adk-code`): Robust rlib discovery for stale build artifacts.

### Changed

- **Dependencies**: `wasmtime` 43.0.0 → 43.0.1, `rubato` 1.0.1 → 2.0.0.

## [0.5.0] - 2026-03-26

### Added

#### Realtime Improvements ([@mikefaille](https://github.com/mikefaille))

- **Gemini 3.1 Live API**: Support for multiple parts in Gemini Live sessions (#122).
- **Realtime optimizations** (#272): Concurrency improvements, audio hot path documentation, AGENTS.md guide for realtime development.
- **clippy fix**: Resolved `result_large_err` in adk-realtime (#121).

### Fixed

- **Sandbox dependency discovery** (`adk-code`): Robust rlib discovery for stale build artifacts.

### Changed

- **Dependencies**: `wasmtime` 43.0.0 → 43.0.1, `rubato` 1.0.1 → 2.0.0.

### Added

#### Realtime — LiveKit Typestate Builder, OpenAI Protocol Centralization ([@mikefaille](https://github.com/mikefaille))

- **`LiveKitConfig`** (`adk-realtime`): Secure LiveKit configuration with `secrecy::SecretString` for API keys. URL validation and empty-credential rejection at construction time.
- **`LiveKitRoomBuilder`** (`adk-realtime`): Typestate builder for LiveKit room connections. `identity` is required at compile time. Supports optional audio track setup, room name, and custom video grants.
- **`LiveKitError`** (`adk-realtime`): Dedicated error type for LiveKit operations (config, token generation, connection).
- **`OpenAIProtocolHandler<T>`** (`adk-realtime`): Generic protocol handler wrapping any `OpenAITransportLink` transport. Implements `RealtimeSession` for both WebSocket and WebRTC.
- **`OpenAITransportLink` trait** (`adk-realtime`): Transport abstraction for OpenAI Realtime API. Default implementations for audio encoding and session configuration. WebRTC overrides for direct media track access.
- **Centralized OpenAI protocol** (`adk-realtime`): Shared `convert_config_to_openai()` and `translate_client_message()` functions used by both WebSocket and WebRTC transports.
- **New examples**: `debug_gemini`, `debug_livekit_auth`, `livekit_gemini_bridge`.

#### Developer Ergonomics — Parallel Dispatch, Builder, Tool Metadata, Macro Attributes

- **`ToolExecutionStrategy`** (`adk-core`): New enum with `Sequential` (default), `Parallel`, and `Auto` variants controlling how multiple tool calls from a single LLM response are dispatched.
- **Tool metadata** (`adk-core`): `is_read_only()` and `is_concurrency_safe()` default methods on the `Tool` trait. Both return `false` by default. Used by `Auto` strategy to partition tools for concurrent execution.
- **`FunctionTool` extensions** (`adk-tool`): `with_read_only(bool)` and `with_concurrency_safe(bool)` builder methods.
- **`SimpleToolContext`** (`adk-tool`): Lightweight `ToolContext` implementation for non-agent callers (testing, MCP servers, sub-agent delegation). Construct with `SimpleToolContext::new("caller-name")`.
- **`StatefulTool<S>`** (`adk-tool`): Generic wrapper managing `Arc<S>` lifetime for stateful tool closures. Clones the `Arc` per invocation. Mirrors all `FunctionTool` builder methods.
- **`RunnerConfigBuilder`** (`adk-runner`): Typestate builder for `Runner` construction. Enforces required fields (`app_name`, `agent`, `session_service`) at compile time. Access via `Runner::builder()`.
- **`Runner::run_str()`** (`adk-runner`): Convenience method accepting `&str` for `user_id` and `session_id`. Validates and converts internally; returns error before agent loop on invalid input.
- **`LlmAgentBuilder::tool_execution_strategy()`** (`adk-agent`): Per-agent strategy override. Defaults to `Sequential` when not set.
- **Parallel tool dispatch** (`adk-agent`): Refactored `LlmAgent` dispatch loop supporting `Sequential`, `Parallel`, and `Auto` modes. Error isolation — failed tools produce JSON error responses without aborting the batch.
- **`#[tool]` macro attributes** (`adk-rust-macros`): `#[tool(read_only)]`, `#[tool(concurrency_safe)]`, `#[tool(long_running)]` — set tool metadata directly in the macro. Plain `#[tool]` unchanged.
- **Non-breaking field addition policy** (`STABILITY.md`): Documented policy requiring `Option<T>` with defaults for new fields on public structs in Stable-tier crates.

#### Competitive Improvements — Stability, Ergonomics, Encryption, Graph Resume, Tool Search

- **STABILITY.md**: New stability roadmap at the repository root defining three tiers (Stable, Beta, Experimental) with contracts, a crate-tier mapping table for every public `adk-*` crate, deprecation lifecycle policy (N+2 minor releases with `#[deprecated(since, note)]`), and 1.0 milestone criteria with GitHub milestone link.
- **Semver CI enforcement**: New `.github/workflows/semver.yml` runs `cargo semver-checks check-release` on every PR — fails for Stable-tier crates, warns for Beta/Experimental.
- **`provider_from_env()`** (`adk-rust`): Auto-detect LLM provider from environment variables. Checks `ANTHROPIC_API_KEY` → `OPENAI_API_KEY` → `GOOGLE_API_KEY` in precedence order, returns `Arc<dyn Llm>`. Feature-gated per provider.
- **`adk::run()`** (`adk-rust`): Single-function agent invocation — `run("instructions", "input").await` handles provider detection, session creation, agent building, and execution. Returns `Result<String>`.
- **MCP Resource API** (`adk-tool`): `McpToolset::list_resources()`, `list_resource_templates()`, and `read_resource(uri)` methods delegating to rmcp's `resources/list`, `resourceTemplates/list`, and `resources/read` protocol methods. Returns empty vec when server doesn't support resources. Re-exports `Resource`, `ResourceTemplate`, `ResourceContents` from `rmcp::model`.
- **Graph durable resume** (`adk-graph`): `PregelExecutor` now checks for existing checkpoints before starting execution. If a checkpoint exists for the thread ID, state, pending nodes, and step are restored from it — skipping already-completed nodes. Both `run()` and `run_stream()` support resume. New `StreamEvent::Resumed` variant emitted when execution resumes from a checkpoint.
- **Deepgram streaming STT** (`adk-audio`): Full WebSocket streaming implementation for `DeepgramStt::transcribe_stream()` — connects to `wss://api.deepgram.com/v1/listen`, forwards audio frames as binary messages, yields interim and final `Transcript` values. Supports diarization, language detection, and model selection.
- **Structured tool output fix** (`adk-model`): Shared `serialize_tool_result()` helper prevents double-encoding of JSON objects in tool results across all 7 provider convert modules (OpenAI, Anthropic, Groq, DeepSeek, Azure AI, Bedrock, Ollama).
- **`InterruptionDetection` enum** (`adk-realtime`): `Manual` (default) and `Automatic` variants controlling how voice activity detection handles user interruptions. Added to `RealtimeConfig` with `with_interruption_detection()` builder method.
- **`EncryptionKey`** (`adk-session`): AES-256-GCM key management behind `encrypted-session` feature flag. `generate()`, `from_env(var_name)`, `from_bytes(&[u8])` constructors. Debug impl redacts key bytes.
- **`EncryptedSession<S>`** (`adk-session`): Transparent encryption wrapper for any `SessionService`. Encrypts state with AES-256-GCM (random 96-bit nonce, stored as `[nonce || ciphertext]`). Supports key rotation — tries current key first, falls back to previous keys, re-encrypts with current key on successful fallback.
- **`ToolSearchConfig`** (`adk-anthropic`): Regex-based tool name filtering. `matches(tool_name)` method compiles pattern and checks match.
- **`AnthropicConfig::with_tool_search()`** (`adk-model`): Optional `ToolSearchConfig` on the Anthropic provider — when set, only tools matching the regex pattern are sent to the API.
- **Validation examples**: Three standalone example crates (`competitive_ergonomics`, `competitive_graph_resume`, `competitive_tool_search`) exercising all new APIs with 37 runtime assertions.

#### Realtime Context Mutation & LiveKit Performance ([@mikefaille](https://github.com/mikefaille))

- **Provider-agnostic context mutation** (`adk-realtime`, #232): Mid-session instruction and tool swapping without dropping the call. `SessionUpdateConfig` newtype for safe partial session updates. `ContextMutationOutcome` enum — `Applied` (OpenAI: in-place `session.update`) or `RequiresResumption` (Gemini: session resumption with `SessionResumptionConfig`). `RealtimeRunner::update_session()` and `update_session_with_bridge()` orchestrate the provider-appropriate path. Includes `SESSION_MANAGEMENT.md` architecture documentation.
- **`RealtimeRunner` session management** (`adk-realtime`, #105/#232): `update_session()`, `next_event()`, and `send_tool_response()` methods for dynamic FSM IVR state transitions. `SessionUpdateConfig` uses the Newtype pattern wrapping `RealtimeConfig` with `Deref`/`DerefMut` for ergonomic field access.
- **Gemini session resumption** (`adk-realtime`, #232): `SessionResumptionConfig` with handle-based reconnection. `GeminiLiveSession` enables session resumption in setup, receives `SessionResumptionUpdate` messages, and reconnects with the handle for context changes.
- **Two realtime examples** (`adk-realtime`, #232): `openai_session_update` (mid-session persona switch with tool swap) and `gemini_context_mutation` (session resumption for context changes).
- **Zero-allocation LiveKit audio output** (`adk-realtime`, #236): Replaced manual `Vec::push` loops with `bytemuck::try_cast_slice` for O(0) copy. `Cow::Borrowed` passes aligned slices directly to WebRTC FFI. Vectorized iterator fallback for unaligned WebSocket chunks. Safety guards skip invalid audio frames. Includes `livekit_pcm_bench` benchmark.

#### devenv & CI ([@mikefaille](https://github.com/mikefaille))

- **devenv v2.0.6 upgrade** (#230): Updated `setup.sh` with v2 experimental features. Added `dbus` and `pkgs.dbus.dev` dependencies for `keyring` crate (adk-cli secure credential storage). Conditional `~/.bashrc` modification (CI-only) to avoid duplicate entries for local devs. Fixed `adk-rag` missing `gemini` feature in examples config.

#### adk-anthropic — Dedicated Anthropic API Client (NEW CRATE)
- **Standalone crate** replacing the `claudius` dependency in `adk-model`. Follows the same pattern as `adk-gemini` — a dedicated, publishable client crate.
- **Full Anthropic API parity** (March 2026): Messages, Batches, Files, Skills, Models, Token Counting APIs.
- **Current model support**: Claude Opus 4.6, Sonnet 4.6, Haiku 4.5, plus legacy 4.5/4.0/4.1 models. `KnownModel` enum with `Model::Custom(String)` fallback.
- **Adaptive thinking**: `ThinkingConfig::adaptive()` for 4.6 models. Effort controlled via `OutputConfig::with_effort()` (supports `Low`, `Medium`, `High`, `Max`).
- **Budget-based thinking**: `ThinkingConfig::enabled(budget_tokens)` for older models (deprecated on 4.6).
- **Structured outputs**: `OutputConfig` with `OutputFormat::Json` and `OutputFormat::JsonSchema`.
- **Prompt caching**: Top-level `cache_control: CacheControlEphemeral` for automatic caching, plus block-level `cache_control` on system prompts, tools, and content blocks.
- **Context management** (beta): `ContextManagement` with `ClearToolUses` and `ClearThinking` strategies. Auto-injects `context-management-2025-06-27` beta header.
- **Fast mode** (beta): `SpeedMode::Fast` for Opus 4.6. Auto-injects `fast-mode-2026-02-01` beta header.
- **Citations**: `CitationsConfig` on documents with `TextCitation` variants (char location, page location, content block location, web search result).
- **Vision**: URL and base64 image analysis via `ImageBlock`.
- **PDF processing**: URL, base64, and Files API PDF analysis via `DocumentBlock`.
- **SSE streaming**: Full event set including `ToolInputStart`, `ToolInputDelta`, `CompactionEvent`, `StreamError`.
- **Token counting**: `count_tokens()` method for pre-send estimation.
- **Token pricing**: `pricing` module with `ModelPricing` constants for all current models and `estimate_cost()` / `estimate_cost_1h()` calculators.
- **Stop reasons**: `StopReason` enum with `EndTurn`, `MaxTokens`, `StopSequence`, `ToolUse`, `PauseTurn`, `Refusal`, `PauseRun`, `ModelContextWindowExceeded`.
- **Container support**: `container` field on `MessageCreateParams` and `ContainerInfo` on `Message` response.
- **Service tier**: `service_tier` field for priority capacity.
- **14 examples**: `basic`, `streaming`, `thinking`, `tools`, `structured_output`, `caching`, `context_editing`, `compaction`, `token_counting`, `stop_reasons`, `fast_mode`, `citations`, `pdf_processing`, `vision`.
- **373 unit tests** covering all types, serialization round-trips, client logic, and SSE parsing.

#### adk-model — Anthropic Migration
- Replaced `claudius` dependency with `adk-anthropic` in `adk-model`. Import paths changed from `use claudius::` to `use adk_anthropic::`.
- Renamed `convert_claudius_error` to `convert_anthropic_error` across all Anthropic adapter modules.
- All 72 adk-model lib tests pass with the new dependency.

#### MCP Elicitation Support (adk-tool)
- **`ElicitationHandler` trait**: User-implementable trait for handling MCP elicitation requests from servers. Supports form-based elicitation (structured schemas) and URL-based elicitation. Requires `Send + Sync` for async safety.
- **`AutoDeclineElicitationHandler`**: Built-in zero-size handler that declines all elicitation requests, preserving backward-compatible behavior identical to rmcp's `()` ClientHandler default.
- **`AdkClientHandler`**: Bridge struct implementing rmcp's `ClientHandler` trait, advertising elicitation capabilities and delegating requests to the user's `ElicitationHandler`. Catches panics and errors gracefully, falling back to Decline.
- **`McpToolset::with_elicitation_handler()`**: Async factory method that creates an MCP client connection with elicitation support from any transport and an `Arc<dyn ElicitationHandler>`.
- **`McpToolset::with_client_handler()`**: Factory method for using a custom `ClientHandler` type with `McpToolset`.
- **`McpHttpClientBuilder::with_elicitation_handler()` / `connect_with_elicitation()`**: Builder methods for HTTP-based MCP connections with elicitation support.
- **Capability advertisement**: `AdkClientHandler` advertises form and URL elicitation capabilities to MCP servers during initialization.
- **Elicitation example**: `examples/mcp_elicitation/` — standalone crate with a real MCP server using `peer.elicit::<T>()` and an LLM-powered agent client with interactive stdin-based `ElicitationHandler`.
- Full backward compatibility: `McpToolset::new()` with `()` handler continues to work unchanged.

#### Gemini built-in tool tracing example (examples)
- **`gemini_search_bug`**: Standalone example reproducing GitHub Issue #224 — demonstrates Google Search + URL Context + function tool coexistence through the ADK runner with full `ServerToolCall`/`ServerToolResponse` tracing, thought signature propagation, and grounding metadata display. Uses `gemini-3-pro-preview` with `include_server_side_tool_invocations` to surface the complete tool call chain.

#### Action Node Graph Standardization (adk-action, adk-graph, adk-rust)
- **`adk-action` crate**: New shared crate containing all 14 action node type definitions, `StandardProperties`, `ActionError` enum, and variable interpolation utilities. Zero runtime dependencies beyond `serde`, `serde_json`, `thiserror`, and `regex`.
- **`ActionNodeExecutor`** in `adk-graph`: Implements the `Node` trait for any `ActionNodeConfig`, applying error handling (stop/continue/retry/fallback), timeout enforcement, and skip conditions uniformly across all node types.
- **14 action node executors**: Set, Transform, Switch, Loop, Merge, Wait, File, Code (Rust), Manual Trigger, HTTP, Code (JS/TS), Database (SQL/MongoDB/Redis), Email (IMAP/SMTP), Notification (Slack/Discord/Teams/webhook), RSS/Feed.
- **`TriggerRuntime`**: Background infrastructure for webhook routes (Axum), cron scheduling (`tokio-cron-scheduler`), and event subscriptions (`tokio::sync::mpsc`).
- **`WorkflowSchema`**: Serializable interchange format for graph workflows with `from_json()` and `build_graph()` methods, enabling adk-studio projects to be loaded and executed by adk-graph.
- **`GraphAgentBuilder` extensions**: `action_node()` and `from_workflow_schema()` methods for convenient action node integration.
- **Feature flags**: `action` (core nodes, no extra deps), `action-trigger`, `action-http`, `action-db`, `action-db-mongo`, `action-db-redis`, `action-code`, `action-email`, `action-rss`, `action-full`. Forwarded through `adk-rust` umbrella crate.
- **10 correctness properties**: Property-based tests across both crates covering round-trip serialization, error mode retry counts, switch condition determinism, interpolation idempotence, backward compatibility, and notification payload formats.

### Fixed

#### adk-gemini
- Fixed Gemini 3 built-in tools (Google Search, URL Context) causing truncated responses (#224). `ContentBuilder::build()` now auto-sets `includeServerSideToolInvocations: true` when server-side tools are present, enabling Gemini 3 to return `toolCall`/`toolResponse` parts on AI Studio instead of silently truncating.
- Fixed Vertex AI 400 error when `includeServerSideToolInvocations` was sent. Vertex AI rejects this field — it handles built-in tools natively. Both the Vertex backend and the Studio backend (when `with_base_url` points at `aiplatform.googleapis.com`) now strip the field before sending.

#### adk-model
- Fixed `test_server_tool_response_round_trip_as_openai_items` test — JSON fixture had `outcome` fields flattened instead of nested, causing deserialization mismatch with `async-openai` 0.33 structs.
- Fixed Anthropic system prompt tests (`test_heuristic_skipped_when_explicit_system_exists`, `test_instruction_rerouting_to_system`, `test_multiple_system_entries_concatenated`) that expected `SystemPrompt::String` but received `SystemPrompt::Blocks` after `prompt_caching` default changed to `true`.
- Fixed `prop_default_config_backward_compatible` property test asserting `prompt_caching` should be `false` — updated to match the actual default of `true`.
- Removed unused `OutputStatus` import in `responses_convert.rs`.
- Replaced `drain(..).collect()` with `std::mem::take()` in Anthropic streaming client per clippy `drain_collect` lint.

### Changed

#### Dependency upgrade (adk-gemini)
- **google-cloud-aiplatform-v1 1.8.0 → 1.9.0**: Migrated `EmbedContentRequest` from deprecated top-level `title`, `task_type`, and `output_dimensionality` fields to the new `EmbedContentConfig` struct. Eliminates 3 deprecation warnings on every build.

#### Provider-native built-in tool support (adk-tool, adk-model, adk-gemini, examples)
- Added typed built-in tool wrappers for Gemini (`GoogleMapsTool`, `GeminiCodeExecutionTool`, `GeminiFileSearchTool`, `GeminiComputerUseTool`), OpenAI Responses (`OpenAIWebSearchTool`, `OpenAIFileSearchTool`, `OpenAICodeInterpreterTool`, `OpenAIImageGenerationTool`, `OpenAIComputerUseTool`, `OpenAIMcpTool`, `OpenAILocalShellTool`, `OpenAIShellTool`, `OpenAIApplyPatchTool`), and Anthropic (`WebSearchTool`, native bash, native text editor variants).
- Added a provider-native declaration path to the shared `Tool` API so agents can mix built-in tools with ordinary `FunctionTool`s without relying on opaque `GenerateContentConfig.extensions` blobs.
- Expanded Gemini wire models to understand additional native tool declarations and code-execution parts, and updated the OpenAI/Anthropic examples to use the new typed wrappers directly.

### Changed

#### Built-in tool adapters
- **adk-model (OpenAI Responses)**: Native OpenAI tool declarations now deserialize from tool metadata instead of only `extensions["openai"]["built_in_tools"]`. Server-side tool outputs are preserved as typed Responses `Item` payloads so they survive streaming finalization and stateless round-trips.
- **adk-model (Anthropic)**: Native Anthropic tool declarations now deserialize from tool metadata, streamed `server_tool_use` / `web_search_tool_result` blocks are preserved in final streamed responses, and string tool results are no longer double-JSON-encoded.
- **adk-model (Gemini)**: Gemini native tools are now metadata-driven instead of name-driven, mixed built-in/function tool detection works for the broader Gemini tool surface, and native tool config such as `retrievalConfig` is forwarded correctly.

#### AP2 alpha adapter (adk-payments)
- Added typed AP2 alpha mandate, payment request, payment response, and payment receipt models plus an `Ap2Adapter` that routes human-present and human-not-present flows through the shared checkout, payment, intervention, journal, and evidence services.
- Added `ap2-a2a` AgentCard and A2A container helpers, `ap2-mcp` safe MCP-facing mandate and receipt views, AP2 fixture coverage, and end-to-end AP2 integration tests.

#### Agentic commerce validation and docs (adk-payments)
- Added a shared multi-actor integration harness for shopper, merchant, credentials-provider, payment-processor, and webhook actors, and rewired ACP/AP2 end-to-end tests to use the shared journal, memory, and evidence plumbing.
- Added payments documentation updates in `adk-payments/README.md`, `docs/official_docs/security/payments.md`, and `examples/payments/README.md`, plus a local `examples/payments` scenario index for the supported commerce journeys.

#### OpenAI Responses API client (adk-model)
- **`OpenAIResponsesClient`**: Dedicated client for OpenAI's `/v1/responses` endpoint — the successor to Chat Completions. Implements `adk_core::Llm` with full streaming, tool calling, and multi-turn support.
- **`OpenAIResponsesConfig`**: Configuration type with `with_reasoning_effort()`, `with_reasoning_summary()`, `with_organization()`, `with_project()`, `with_base_url()`.
- **`ReasoningSummary`** enum (`Auto`, `Concise`, `Detailed`): Controls reasoning summary generation for o-series models. Summaries appear as `Part::Thinking` in the response stream.
- **Streaming deduplication**: `ResponseCompleted` events extract only function calls and usage metadata — text/thinking content already streamed via delta events is not re-emitted.
- **Provider metadata**: Every response includes `provider_metadata.openai.response_id` for server-side state and debugging.
- **Documentation**: Full docs page at `docs/official_docs/models/openai-responses.md`, updated `providers.md`, `adk-model/README.md`, and root `README.md`.
- **Example**: `examples/openai_responses/` — standalone crate with 7 scenarios (basic, streaming, reasoning, tools, multi-turn, system instructions, generation config).

#### OpenRouter deep integration (adk-model, adk-rust, examples)
- **`openrouter` feature on `adk-rust`**: The umbrella crate now re-exports `OpenRouterClient`, `OpenRouterConfig`, `OpenRouterApiMode`, `OpenRouterRequestOptions`, and related types behind a dedicated `openrouter` feature.
- **Native OpenRouter examples**: Added `adk-model/examples/openrouter_chat.rs`, `openrouter_responses.rs`, `openrouter_adapter.rs`, and `openrouter_discovery.rs` plus shared support modules for live provider validation.
- **Agentic OpenRouter validation crate**: Added `examples/openrouter` as a standalone example crate mirroring the `examples/openai_responses` style and covering chat, streaming, tools, responses mode, multimodal input, routing, discovery, and sessioned runner flows.
- **Ignored live contracts**: Added `adk-model/tests/openrouter_contract_tests.rs` and wired OpenRouter into the shared provider contract harness for ignored live validation.

#### Config validation (adk-gemini)
- **`ThinkingConfig::validate()`**: Pre-send validation that rejects mutually exclusive `thinking_budget` + `thinking_level` combinations before the request reaches the Gemini API.
- **`GenerationConfig::validate()`**: Pre-send validation for `temperature` (0.0–2.0), `top_p` (0.0–1.0), `top_k` (> 0), `max_output_tokens` (> 0), and delegates to `ThinkingConfig::validate()` when present. Validation is wired at the request boundary — invalid configs return `AdkError` instead of sending malformed requests.

#### Audio codec capability queries (adk-audio)
- **`AudioFormat::supports_encode()`**: Returns `true` for formats with working `encode()` implementations (`Pcm16`, `Wav`), `false` for all others. Uses exhaustive `match` so new variants force a decision.
- **`AudioFormat::supports_decode()`**: Returns `true` for formats with working `decode()` implementations (`Pcm16`, `Wav`), `false` for all others.

#### Feature presets (adk-rust)
- **`labs` feature preset**: New preset for experimental crates (`code`, `sandbox`, `audio`). Use `features = ["labs"]` to opt in to experimental functionality.

### Changed

#### OpenRouter production hardening
- **Streaming finalization**: The OpenRouter chat adapter now emits exactly one final `LlmResponse` chunk even when OpenRouter streams `finish_reason` and usage metadata in separate SSE frames.
- **Tool-call mapping**: Chat-mode tool responses now round-trip as `role="tool"` messages with `tool_call_id`, and streamed tool-call deltas tolerate missing `role`, `type`, and function `name` fields that appear in real OpenRouter streams.
- **Documentation**: Updated `adk-model` crate docs and README to document native OpenRouter APIs, the generic `Llm` adapter boundary, and the local example entry points.

#### Feature presets (adk-rust)
- **`full` feature preset no longer includes experimental crates**: `full` now compiles only stable specialist crates (`graph`, `realtime`, `browser`, `eval`, `rag`). Experimental crates (`code`, `sandbox`, `audio`) moved to the new `labs` preset. Use `features = ["full", "labs"]` to get everything.

#### Debug endpoint honesty (adk-server)
- **`get_graph` returns 501 Not Implemented**: Previously returned HTTP 200 with a hardcoded fake DOT graph string. Now returns HTTP 501 with `{ "error": "graph generation is not yet implemented" }`.
- **`get_eval_sets` returns 501 Not Implemented**: Previously returned HTTP 200 with an empty array stub. Now returns HTTP 501 with `{ "error": "eval sets are not yet implemented" }`.
- **`get_event` returns 404 when event not found**: Previously returned HTTP 200 with a stub JSON body containing an empty `invocationId`. Now returns HTTP 404 Not Found. Existing successful path (event found in span exporter) is unchanged.

#### Production hardening
- **adk-core**: Added validated `new()` constructors for `AppName`, `UserId`, `SessionId`, and `InvocationId` so trust-boundary code can use an explicit safe constructor instead of relying on `TryFrom`.
- **adk-runner**: `Runner::run()` now accepts typed `UserId` and `SessionId` parameters. Migration: `runner.run("user".to_string(), "session".to_string(), content)` becomes `runner.run(UserId::new("user")?, SessionId::new("session")?, content)`.
- **adk-runner**: Added `MutableSession::events_len()` and updated compaction checks to avoid cloning the full event list for count-only access.
- **adk-audio**: AssemblyAI, Deepgram, and MLX `transcribe_stream()` stubs now return explicit `AudioError::Stt` errors instead of silently succeeding with empty streams.
- **adk-audio**: MLX STT placeholder errors now clearly state that local Whisper inference is not yet implemented and recommend using a cloud STT provider.

#### Structured Error Envelope (Breaking)
- **adk-core**: Replaced flat `AdkError` enum with a multi-axis struct separating component (where), category (what kind), code (machine key), message (human text), retry hint, and error details. This is a deliberate breaking change targeting pre-1.0.
- **adk-core**: Added `ErrorComponent` (14 variants) and `ErrorCategory` (10 variants) enums for structured error classification.
- **adk-core**: Added `RetryHint` with `should_retry`, `retry_after_ms`, and `max_attempts` fields for structured retry guidance.
- **adk-core**: Added `http_status_code()` and `to_problem_json()` methods on `AdkError` for HTTP error response generation.
- **adk-core**: Backward-compatible constructors (`agent()`, `model()`, `tool()`, `session()`, etc.) preserved with `.legacy` code suffix.
- **adk-model**: All providers (Gemini, OpenAI, Anthropic, DeepSeek, Groq, Azure AI, Ollama) now emit structured errors with proper `ErrorCategory` based on HTTP status codes (429→RateLimited, 503→Unavailable, etc.).
- **adk-model**: `is_retryable_model_error()` now checks `error.retry.should_retry` as single source of truth, with fallback to message parsing for legacy errors.
- **adk-model**: `execute_with_retry_hint()` extracts `retry_after` from structured `AdkError` fields.
- **adk-server**: Runtime controller uses `AdkError::http_status_code()` and `to_problem_json()` for error responses instead of hardcoded 500s.
- **All crates**: Migrated from `AdkError::Variant("msg".into())` to `AdkError::variant("msg")` method syntax.
- **Boundary crates**: Added `From<CrateLocalError> for AdkError` impls in adk-realtime, adk-graph, adk-guardrail, adk-auth, adk-code, adk-skill, adk-sandbox, adk-eval, adk-rag.

### Changed (from 0.4.1)

#### Examples
- **Moved to adk-playground**: All examples removed from this workspace and consolidated in the [adk-playground](https://github.com/zavora-ai/adk-playground) repo (120+ examples). The `examples/` directory now contains only a README pointing there.

#### Error Handling Hardening
- **adk-runner**: Replaced all `RwLock::unwrap()` calls in `MutableSession` with graceful error handling. Poisoned locks now log via `tracing::error` and return safe defaults (empty `Vec`, empty `HashMap`, `None`) instead of panicking. Affects `apply_state_delta`, `append_event`, `events_snapshot`, `conversation_history`, and `State` trait methods.
- **adk-telemetry**: Replaced `expect()` calls in `init_with_otlp()` with proper error propagation — OTLP exporter build failures now return `TelemetryError::Init` instead of panicking. Replaced `expect()` with `unwrap_or_else` fallback for `EnvFilter` in all init functions. Replaced `expect()` with `let-else` early return in span `Layer` callbacks. Replaced `unwrap()` with `unwrap_or_else(into_inner)` for `RwLock` in `AdkSpanExporter`.
- **adk-code**: `DockerExecutor::new()` now returns `Result<Self, ExecutionError>` instead of panicking when the Docker daemon is unreachable.
- **adk-agent**: Replaced `Arc::get_mut().expect()` with `if let Some` in builder methods for `LoopAgent`, `ConditionalAgent`, and `ParallelAgent`.

#### Dependency Cleanup
- **adk-agent**: Removed unused `adk-model` direct dependency and `gemini` feature forwarding. `adk-agent` source code had zero imports from `adk_model`; the dependency only existed to forward the `gemini` feature flag. No crate in the workspace referenced `adk-agent/gemini`. `adk-model` remains as a dev-dependency for tests.
- **adk-guardrail**: Set `jsonschema = { version = "0.43", optional = true, default-features = false }` to eliminate `reqwest 0.13` from the dependency tree. ADK does not use remote JSON Schema `$ref` resolution, so the network features are unnecessary.
- **adk-model (anthropic)**: Upgraded `claudius` from 0.16 to 0.19, eliminating `reqwest 0.11` from the dependency tree. The claudius 0.19 API takes `&params` instead of `params` in `.stream()`. Note: claudius 0.19 uses `reqwest 0.13` internally, so there is still a reqwest version duplicate with the workspace's `reqwest 0.12`, but the older `reqwest 0.11` is gone.
- **adk-telemetry**: Upgraded OpenTelemetry stack from 0.21 to 0.28 (`opentelemetry 0.28`, `opentelemetry_sdk 0.28`, `opentelemetry-otlp 0.28`, `tracing-opentelemetry 0.29`). This eliminates duplicate `axum`, `hyper`, `http`, `h2`, and `tower` crates — the old OTel stack pulled `tonic 0.9` → `axum 0.6` → `hyper 0.14` → `http 0.2`, while `adk-server` uses `axum 0.8` → `hyper 1.x` → `http 1.x`. Updated `init_with_otlp()` to use new 0.28 builder APIs (`SdkTracerProvider`, `SpanExporter::builder`, `MetricExporter::builder`, `SdkMeterProvider`). Updated `shutdown_telemetry()` to replace the global provider with a no-op (the `shutdown_tracer_provider()` global function was removed in OTel 0.28).

#### Examples
- **telemetry_demo**: Updated to use OTel 0.28 APIs — `.build()` instead of `.init()` for metrics instruments. Replaced mock/simulated LLM calls with real Gemini API calls. The demo now requires `GOOGLE_API_KEY` and demonstrates actual token usage recording via `with_usage_tracking` for both non-streaming and streaming responses.

### Fixed

#### adk-gemini
- **Gemini 3.x thought_signature serialization**: Changed `#[serde(skip_serializing)]` to `#[serde(skip_serializing_if = "Option::is_none")]` on `thought_signature` fields in `Part::Text`, `Part::FunctionCall`, and the tools `FunctionCall` struct. Gemini 3.x models require `thought_signature` to be echoed back in multi-turn function calling; the previous behavior silently dropped it, causing 400 errors on the second LLM call after tool execution. Backward compatible — field is omitted when `None`.

#### adk-tool
- **AgentTool infinite loop on empty sub-agent responses**: `AgentToolInvocationContext::run_config()` now returns `StreamingMode::None` instead of `StreamingMode::SSE`. In SSE mode, the sub-agent's final event often contained empty text (actual content was spread across earlier partial chunks), causing the coordinator to re-call the same tool indefinitely. Non-streaming mode accumulates the full response before yielding a single complete event. Additionally, `extract_response` now skips empty text parts and falls back to collecting text from all events.

#### adk-session
- **MongoDB standalone deployment support**: `MongoSessionService` now auto-detects whether the connected MongoDB instance supports multi-document transactions (replica set / sharded cluster) or is running standalone. On standalone deployments, all write operations execute sequentially without transactions instead of failing with `IllegalOperation: Transaction numbers are only allowed on a replica set member or mongos`. Detection uses the `hello` command at connection time to check for `setName` in the response. New `supports_transactions()` method exposes the detected mode. The `retryWrites=false` connection string workaround is no longer required.
- **PostgreSQL migration INT4/INT8 type mismatch**: Fixed `COALESCE(MAX(version), 0)` in the migration registry query to use `CAST(... AS BIGINT)`. PostgreSQL creates the `version` column as `INTEGER` (INT4) but the Rust code reads it as `i64` (INT8), causing a type mismatch error on migration. The cast ensures the return type matches the expected Rust type.
- **PostgreSQL migration registry DDL**: Parameterized the migration runner macro to use `BIGINT PRIMARY KEY` for PostgreSQL and `INTEGER PRIMARY KEY` for SQLite, matching the Rust `i64` type natively. Removed the `CAST(... AS BIGINT)` workaround from SELECT queries since the column type is now correct. Applied to both `adk-session` and `adk-memory` migration runners.
- **examples**: Added `required-features = ["rag-gemini"]` to the `rag_gemini` example entry, fixing `cargo test --workspace` compilation failure when the optional `adk-rag` dependency is not enabled.


## [0.4.0] - 2026-03-16

### Added

#### `cargo-adk` Scaffolding CLI
- **Project scaffolding**: New `cargo-adk` binary for generating agent projects from templates. `cargo adk new my-agent` scaffolds a working project with the right dependencies, feature flags, and boilerplate. Templates: `basic`, `tools` (#[tool] macro), `rag` (vector search), `api` (REST server), `openai`. Supports `--provider` flag for OpenAI/Anthropic/Gemini.

#### `#[tool]` Proc Macro (adk-rust-macros)
- **Zero-boilerplate tool registration**: New `#[tool]` attribute macro turns an async function into a full `Tool` implementation. Doc comments become the description, argument types derive JSON schemas via schemars, and a PascalCase struct is generated implementing `adk_core::Tool`. Supports both standalone functions and functions with `Arc<dyn ToolContext>` parameter. Schema output is automatically cleaned for LLM API compatibility (strips `$schema`, simplifies nullable types).

#### Development Infrastructure
- **cargo-nextest integration**: Switched from `cargo test` to `cargo nextest run` for workspace test execution. Parallel test binary execution reduces test wall-clock time from ~1m47s to ~9s (~11x speedup). Added `.config/nextest.toml` with default and CI profiles (CI profile includes retry-on-flaky and slow-test warnings). `devenv.nix` updated with `ws-test` (nextest), `ws-test-ci` (nextest CI profile), and `ws-test-slow` (fallback `cargo test` for doctests) scripts.

#### Vision / Multimodal Support (adk-model)
- **Bedrock**: `InlineData` with image MIME types (jpeg/png/gif/webp) now maps to `ContentBlock::Image`; document MIME types (pdf/csv/html/md/txt/doc/docx) map to `ContentBlock::Document`. Response-side `ContentBlock::Image` converts back to `Part::InlineData`. `FileData` with image/document URLs becomes a text reference (Bedrock only supports S3 URIs natively).
- **OpenAI**: `FileData` with `image/*` MIME types now maps to `ImageUrl` content part instead of falling back to text, enabling direct image URL vision.
- **Anthropic**: `FileData` with image MIME types (jpeg/png/gif/webp) now maps to `ImageBlock` with `UrlImageSource` instead of text fallback, enabling direct image URL vision.

#### OpenAI Reasoning Model Support (adk-model)
- **Reasoning content extraction**: OpenAI-compatible client now uses direct reqwest calls instead of async-openai's HTTP client, enabling extraction of `reasoning_content` from reasoning models (o3, o4-mini, gpt-5-mini) that async-openai 0.33 silently drops. Reasoning content maps to `Part::Thinking`.
- **Empty text filtering**: `from_openai_response` and new `from_raw_openai_response` now filter empty text parts produced by reasoning models when all tokens go to internal chain-of-thought.

### Changed

#### adk-rust (umbrella crate)
- **Tiered feature presets**: Default changed from `full` to `standard`. Three presets: `minimal` (agents + Gemini + runner, ~30s build), `standard` (+ tools, sessions, memory, telemetry, guardrail, auth, plugin, ~51s build), `full` (+ server, CLI, graph, browser, eval, realtime, RAG, audio, ~2min build). Users who need server/CLI/specialist crates add `features = ["full"]`.
- **Minimal tokio features**: `adk-rust` umbrella crate now declares explicit tokio features (`rt`, `rt-multi-thread`, `sync`, `time`, `macros`, `net`, `signal`, `fs`, `process`, `io-util`) instead of `"full"`. Binary crates (`adk-cli`, examples) retain `"full"`. This follows the Rust convention that library crates should never use `tokio = { features = ["full"] }`.

#### adk-core
- **AdkError documentation**: All 9 error variants now have doc comments describing their use (Agent, Model, Tool, Session, Artifact, Memory, Config, Io, Serde).

#### Examples
- **openai_basic**: Default model changed from `gpt-5-mini` to `gpt-4o-mini`, `max_output_tokens` increased from 64 to 256 (reasoning models need headroom). Supports `OPENAI_MODEL` env var override.
- **vision_test**: OpenAI model changed from `gpt-5-mini` to `gpt-4o-mini`.
- **Cleanup**: Removed 20 non-essential `openai_*` example directories (full collection in adk-playground repo).

#### adk-model
- **Consolidated OpenAI-compatible providers**: Replaced 7 near-identical provider modules (fireworks, together, mistral, perplexity, cerebras, sambanova, xai) with `OpenAICompatibleConfig` presets. Each was ~150 lines wrapping the same `OpenAICompatible` client — now 7 preset constructors totaling 63 lines. Usage: `OpenAICompatible::new(OpenAICompatibleConfig::fireworks(key, model))`. Feature flags preserved as backward-compatible aliases (`fireworks = ["openai"]`). `all-providers` simplified from 15 to 8 flags.

#### adk-telemetry
- **Standardized LLM token usage telemetry**: New `llm_generate_span(provider, model, stream)` creates spans with pre-declared `gen_ai.usage.*` fields following OpenTelemetry GenAI semantic conventions. New `LlmUsage` struct and `record_llm_usage(&usage)` record token counts (input, output, total, cache read/creation, thinking, audio input/output) on the current span. All 8 fields are optional-aware — only non-None values are recorded.
- **Proper error type**: Replaced `Box<dyn std::error::Error>` with `TelemetryError` (thiserror) in all init functions. Convention-compliant typed errors.

#### adk-model
- **Unified token usage tracking across all providers**: New `usage_tracking::with_usage_tracking(stream, span)` wraps any `LlmResponseStream` to automatically record `gen_ai.usage.*` fields on the tracing span. Applied to all 10 providers: Gemini, OpenAI, OpenAI-compatible (Fireworks, Together, Mistral, Perplexity, Cerebras, SambaNova, xAI), Anthropic, Ollama, Bedrock, DeepSeek, Groq, Azure AI, Azure OpenAI. Previously only Anthropic recorded token counts; now all providers emit standardized telemetry including cache, thinking, and audio token counts.

#### adk-plugin
- **Removed unused dependencies**: `async-trait` and `serde` removed from Cargo.toml (never imported).

#### adk-memory
- **Shared text utilities**: Extracted `extract_text()`, `extract_words()`, and `extract_words_from_content()` into `adk_memory::text` module. Removed duplicate implementations from 5 backends (postgres, sqlite, mongodb, neo4j, redis) and inmemory.

#### Documentation (Tier 2 crates)
- **adk-artifact**: Documented all request/response structs, `ArtifactService` trait methods, `InMemoryArtifactService`.
- **adk-guardrail**: Documented `GuardrailError` variants, `GuardrailSet` methods, `ExecutionResult` fields.
- **adk-skill**: Documented 8 public functions (`select_skills`, `apply_skill_injection`, `discover_skill_files`, `parse_skill_markdown`, `load_skill_index`, etc.).
- **adk-gemini**: Removed `println!` debug statements from tests.
- **README versions**: Bumped 0.3→0.4 in adk-telemetry, adk-memory, adk-artifact, adk-plugin, adk-guardrail, adk-gemini.

#### adk-mistralrs
- **Minimal tokio features**: Changed from `tokio = { features = ["full"] }` to `tokio = { features = ["rt", "sync", "macros"] }` — the minimal set actually used by the crate.

#### CI
- **nextest in CI**: GitHub Actions workflow now uses `ws-test-ci` (cargo-nextest with CI profile) instead of `cargo test --workspace`. Summary parser updated to handle nextest output format with fallback for `cargo test` format.

#### adk-model (OpenAI / OpenAI-compatible providers)
- **async-openai 0.33**: Upgraded from 0.27 to 0.33. Breaking API changes adapted: types moved to `types::chat::*`, `ChatCompletionToolType` removed, `FunctionObject.parameters` changed to `Option<serde_json::Value>`, `max_tokens` replaced with `max_completion_tokens`.
- **Non-streaming workaround**: OpenAI and Azure OpenAI providers temporarily use non-streaming `create()` instead of `create_stream()` due to a `reqwest-eventsource` compatibility bug in async-openai 0.33 that causes "Invalid header value" errors on SSE connections. Responses arrive as a single chunk. Streaming will be restored when the upstream bug is fixed.
- **reqwest default features restored**: Root workspace `reqwest` dependency no longer sets `default-features = false`, fixing transitive feature resolution issues.

### Added

#### adk-sandbox (NEW CRATE)
- New `adk-sandbox` crate: isolated code execution runtime for ADK agents
- `SandboxBackend` trait with `execute(ExecRequest) -> Result<ExecResult, SandboxError>` and `capabilities()` methods
- `ProcessBackend`: subprocess execution via `tokio::process::Command` with timeout enforcement, environment isolation (`env_clear()`), output truncation (1 MB, UTF-8 safe), and `kill_on_drop(true)`. Supports Rust, Python, JavaScript, TypeScript, and shell commands
- `WasmBackend`: in-process WASM execution via `wasmtime` with epoch-based timeout, memory limits via `StoreLimitsBuilder`, WASI stdin/stdout/stderr capture, and no filesystem or network access (behind `wasm` feature)
- `SandboxTool`: `adk_core::Tool` implementation delegating to any `SandboxBackend`, with error-as-information pattern (errors returned as structured JSON, never `ToolError`)
- `ExecRequest` and `ExecResult` types with explicit timeout (no `Default` impl), `Language` enum, and `SandboxError` enum
- `BackendCapabilities` with honest `EnforcedLimits` reporting what each backend actually enforces
- Feature flags: `process` (default), `wasm` (optional, requires `wasmtime`)

### Changed

#### Repository structure
- `adk-deploy-server` and `adk-deploy-console` have been hard-migrated out of the `adk-rust` workspace into the sibling `adk-platform` repo, while `adk-deploy` remains in `adk-rust` as the shared deployment manifest and bundling utility crate

#### adk-code
- Redesigned with `RustExecutor`: check → build → execute pipeline delegating to `SandboxBackend` from `adk-sandbox`
- New `CodeTool` implementing `adk_core::Tool` with structured diagnostic passthrough (compile errors as JSON, not `ToolError`)
- New `CodeError` enum with `CompileError` (structured `Vec<RustDiagnostic>`), `DependencyNotFound`, `Sandbox`, `InvalidCode` variants
- Extracted `harness.rs` (harness template, source validation) and `diagnostics.rs` (rustc JSON diagnostic parser) as shared modules
- `EmbeddedJsExecutor` capabilities fixed: now honestly reports `true` for network/filesystem/environment enforcement (isolation by omission via `boa_engine`)
- `DockerExecutor` Drop safety fixed: uses `Handle::try_current()` before spawning cleanup, logs warning when no runtime is available
- Migration compatibility layer in `compat` module with deprecated type aliases for one release cycle

### Deprecated

#### adk-tool
- `RustCodeTool` is deprecated in favor of `adk_code::CodeTool`

#### adk-code
- `CodeExecutor`, `ExecutionRequest`, `ExecutionResult`, `RustSandboxExecutor`, `RustSandboxConfig` type aliases deprecated (use `adk-sandbox` and new `adk-code` types instead). Will be removed in v0.6.0

## [0.4.0] - 2026-03-12

### Added

#### adk-code (NEW CRATE)
- New `adk-code` crate: first-class code execution substrate for ADK-Rust
- Core types: `CodeExecutor` trait, `ExecutionRequest`, `ExecutionResult`, `ExecutionLanguage`, `SandboxPolicy`, `BackendCapabilities`, `ExecutionIsolation`
- `CodeExecutor` lifecycle methods: `start()`, `stop()`, `restart()`, `is_running()` for persistent execution environments (default no-ops for simple backends)
- `RustSandboxExecutor`: flagship Rust-authored code execution with host-local process isolation and strict defaults (30s timeout, 1MB output limits)
- `EmbeddedJsExecutor`: secondary in-process JavaScript backend via `boa_engine` for lightweight transforms (behind `embedded-js` feature)
- `DockerExecutor`: persistent Docker container executor using `bollard` SDK (behind `docker` feature) — matches AutoGen's `DockerCommandLineCodeExecutor` lifecycle model (start once, execute many, stop when done)
- `DockerConfig` presets: `python()`, `node()`, `custom(image)` with builder methods `.pip_install()`, `.npm_install()`, `.with_network()`, `.env()`, `.bind_mount()`
- `ContainerCommandExecutor`: CLI-based ephemeral container executor for Python, JavaScript, and command execution
- `WasmGuestExecutor`: guest-module backend for precompiled `.wasm` modules with explicit boundary validation
- `Workspace` and `CollaborationEvent`: shared project context for multi-agent code generation with typed collaboration events (NeedWork, WorkClaimed, WorkPublished, FeedbackRequested, FeedbackProvided, Blocked, Completed)
- A2A-compatible collaboration event mapping for future remote specialist execution
- `ExecutionMetadata` and `ArtifactRef` for telemetry correlation and artifact storage references
- Fail-closed sandbox policy validation: backends reject unsupported controls before executing user code
- 10 correctness properties validated by proptest (100+ iterations each)

#### adk-tool
- `RustCodeTool`: primary Rust-first code execution tool with `code:execute` and `code:execute:rust` scopes
- `JavaScriptCodeTool`: secondary JavaScript execution tool — uses real `EmbeddedJsExecutor` when `code-embedded-js` feature is enabled, returns descriptive error otherwise
- `PythonCodeTool`: container-backed Python execution tool, supports custom executors via `with_executor()` (e.g., `DockerExecutor` for persistent containers)
- `FrontendCodeTool`: container-backed Node.js execution tool for React/frontend code, supports custom executors via `with_executor()`
- New feature flags: `code-embedded-js` (enables boa_engine JS backend), `code-docker` (enables Docker SDK persistent containers)
- Workspace-friendly presets: `RustCodeTool::backend()`, `FrontendCodeTool::react()` for collaborative project builds

#### adk-studio
- Rust-first code execution: Studio live runner executes authored Rust through `adk-code` `RustSandboxExecutor` instead of returning placeholder errors
- Generated Studio projects reuse the same authored Rust body for code nodes
- Rust is the primary code authoring mode; JavaScript/TypeScript available as secondary scripting
- Sandbox settings map to backend-enforceable capabilities with incompatibility surfacing

#### adk-deploy
- `adk-deploy` manifest coverage now includes telemetry, auth, guardrails, realtime, A2A, graph/HITL, plugins, skills, and richer service binding validation for self-hosted deployment workflows
- Bundle creation now rejects asset paths that escape the project root
- `adk-cli` deploy login now validates operator-provided bearer tokens against the external platform API and stores them in the OS credential store instead of plaintext config
- Deployment manifests can now publish operator interaction metadata for manual, webhook, schedule, and event triggers, and Studio carries trigger configuration into that manifest for external platform consumers

### Fixed

#### adk-gemini
- **Citation metadata deserialization**: `CitationMetadata` now deserializes correctly when Gemini returns `citationMetadata` without a `citationSources` field. Previously this caused a deserialization error for grounded responses using Google Search or URL context tools. ([#178](https://github.com/zavora-ai/adk-rust/issues/178))
- **Vertex AI global endpoint**: The Vertex endpoint builder now correctly produces `https://aiplatform.googleapis.com` when `location` is `"global"`, instead of the invalid `https://global-aiplatform.googleapis.com`. No custom base URL workaround is needed for Gemini 3 models on the global endpoint. ([#179](https://github.com/zavora-ai/adk-rust/issues/179))
- **Feature-gated Google Cloud dependencies**: `google-cloud-aiplatform-v1`, `google-cloud-auth`, and `google-cloud-gax` are now optional dependencies behind the `vertex` feature flag. Users who only need the Gemini Developer API (AI Studio) can compile with `--no-default-features --features studio` to avoid pulling in heavy Google Cloud crates. Default features include `vertex` for backward compatibility. ([#181](https://github.com/zavora-ai/adk-rust/issues/181))

### Added

#### adk-gemini
- **Gemini 3 thinking level**: `ThinkingLevel` enum (`Minimal`, `Low`, `Medium`, `High`) and `thinking_level` field on `ThinkingConfig` for native Gemini 3 level-based reasoning control. Builder method `with_thinking_level()` available on both `ThinkingConfig` and `ContentBuilder`. Existing Gemini 2.5 budget-based APIs (`with_thinking_budget`, `with_dynamic_thinking`) are unchanged. ([#177](https://github.com/zavora-ai/adk-rust/issues/177))

#### adk-model
- **OpenAI reasoning effort**: `ReasoningEffort` enum (`Low`, `Medium`, `High`) and `reasoning_effort` field on `OpenAIConfig` for OpenAI reasoning models (o1, o3, etc.). Builder method `with_reasoning_effort()` wires through to the `reasoning_effort` API field. Also available on `OpenAICompatibleConfig` for compatible providers. ([#177](https://github.com/zavora-ai/adk-rust/issues/177))

#### adk-core
- **Typed identity module**: New `adk_core::identity` module with `AppName`, `UserId`, `SessionId`, `InvocationId` newtypes, `AdkIdentity` (session-scoped triple), `ExecutionIdentity` (per-invocation capsule), and `IdentityError`. All leaf types implement `Clone`, `Debug`, `Eq`, `Hash`, `Ord`, `Display`, `AsRef<str>`, `Borrow<str>`, `FromStr`, `TryFrom<&str>`, `TryFrom<String>`, `Serialize`, `Deserialize` with `#[serde(transparent)]`. Validation rejects empty values, null bytes, and strings exceeding 512 bytes. `SessionId::generate()` and `InvocationId::generate()` produce UUID-based identifiers.
- **Typed context helpers on `ReadonlyContext`**: Additive default methods `try_app_name()`, `try_user_id()`, `try_session_id()`, `try_invocation_id()`, `try_identity()`, and `try_execution_identity()` parse existing string fields into typed identifiers, returning `IdentityError` on invalid values instead of panicking.
- **Typed session helpers on `Session`**: Additive default methods `try_app_name()`, `try_user_id()`, `try_session_id()`, and `try_identity()` on the `Session` trait.
- **`ToolOutcome` struct**: Structured metadata for tool execution results — carries tool name, arguments, success/failure, execution duration, optional error message, and retry attempt number. Available via `CallbackContext::tool_outcome()` in after-tool callbacks.
- **`tool_outcome()` default method on `CallbackContext`**: Returns `Option<ToolOutcome>`, defaulting to `None` for full backward compatibility with existing implementors.
- **`RetryBudget` struct**: Configurable retry policy with `max_retries` and `delay` for automatic tool retry on transient failures.
- **`OnToolErrorCallback` type**: Promoted to `adk-core` as the canonical, framework-level tool-error callback type. Previously defined locally in `adk-agent` and `adk-plugin`.
- **`AfterToolCallbackFull` type**: V2 rich after-tool callback aligned with Python/Go ADK model. Receives `(CallbackContext, Tool, args, response)` and can inspect or replace the tool response sent to the LLM.

#### adk-auth
- **Typed auth-boundary user validation**: `JwtRequestContextExtractor` now validates the mapped auth user against `UserId` before returning `RequestContext`. Invalid mapped user IDs now fail with `RequestContextError::ExtractionFailed` instead of slipping deeper into the runtime. `ClaimsMapper` remains responsible only for claim selection.

#### adk-agent
- **`.toolset()` builder method**: `LlmAgentBuilder` now accepts `Arc<dyn Toolset>` for dynamic per-invocation tool resolution. Toolsets are resolved at the start of each `run()` call using the current `ReadonlyContext`, enabling context-dependent tools (e.g., per-user browser sessions). Static `.tool()` and dynamic `.toolset()` can be mixed freely.
- **`.default_retry_budget()` and `.tool_retry_budget()`**: Configure automatic retry for transient tool failures. Per-tool budgets override the default. When retries are exhausted, the final failure is reported to the LLM.
- **`.circuit_breaker_threshold()`**: Tracks consecutive tool failures per tool name within an invocation. After the configured threshold, the tool is temporarily disabled with an immediate error response to the LLM. Resets at the start of each new invocation.
- **`.on_tool_error()` callback**: Register fallback handlers invoked when a tool fails (after retries are exhausted). Callbacks can return a substitute `Value` used as the function response, or `None` to pass through to the next handler. If no handler provides a fallback, the original error is reported to the LLM.
- **`ToolOutcome` in after-tool callbacks**: `CallbackContext::tool_outcome()` returns structured execution metadata (success, duration, error, attempt number) without requiring JSON error parsing.
- **`.after_tool_callback_full()` builder method**: V2 rich after-tool callback that receives the tool, arguments, and response. Runs after the legacy `AfterToolCallback` chain. Aligned with Python/Go ADK model for first-class tool result handling.

#### adk-realtime
- **`.toolset()` builder method on `RealtimeAgentBuilder`**: Dynamic per-invocation tool resolution for realtime voice agents, matching `LlmAgentBuilder` parity. Toolsets are resolved before the realtime session connects, with the same duplicate detection (static-vs-toolset, toolset-vs-toolset) as `LlmAgent`. Static `.tool()` and dynamic `.toolset()` can be mixed freely. Fully backward compatible.

#### adk-tool
- **Toolset composition utilities**: Three reusable toolset wrappers for complex agent configurations:
  - `FilteredToolset` — wraps any toolset and filters tools by predicate (allow-list via `string_predicate()` or custom `ToolPredicate`)
  - `MergedToolset` — combines multiple toolsets into one with first-wins deduplication and `tracing::warn` on name conflicts
  - `PrefixedToolset` — namespaces all tool names with a configurable prefix to avoid collisions across toolsets
  All three implement `Toolset` and compose with any toolset implementation including `McpToolset` and `BrowserToolset`.

#### adk-browser
- **Pool-backed `BrowserToolset`**: `BrowserToolset::with_pool()` and `BrowserToolset::with_pool_and_profile()` constructors resolve per-user browser sessions from `BrowserSessionPool` using the invocation's `user_id`. This is the production path for multi-tenant browser agents. Existing `new()` and `with_profile()` constructors are unchanged.
- **`try_all_tools()`**: Explicit error handling for pool-backed toolsets where `all_tools()` cannot resolve without context.
- **`ensure_started()` auto-recovery**: All public `BrowserSession` methods that access the WebDriver now go through a centralized lifecycle-safe path that auto-starts or reconnects stale sessions. Tools no longer fail with "Browser session not started" errors. Explicit `start()` and `stop()` remain for manual lifecycle control.
- **Navigation tool page context**: `NavigateTool`, `BackTool`, `ForwardTool`, and `RefreshTool` now include a `"page"` field in responses with the current page context (URL, title, truncated text), matching the format used by interaction tools. If page context capture fails, a `"page_context_error"` field is included instead.

#### Examples
- **`browser_pool`**: Multi-tenant pool-backed `BrowserToolset` with per-user session isolation, `.toolset()` API, and `ensure_started()` auto-recovery. Requires `--features browser`.
- **`resilient_agent`**: Retry budgets, circuit breakers, `on_tool_error` fallback callbacks, and `ToolOutcome` metadata in after-tool callbacks. Uses mock flaky/broken/reliable tools.
- **`toolset_composition`**: `FilteredToolset`, `MergedToolset`, `PrefixedToolset`, `BasicToolset`, `string_predicate`, and full composition chains.
- **`server_compaction`**: `ServerConfig::with_compaction()`, `EventsCompactionConfig`, and custom `BaseEventsSummarizer`.

#### adk-session
- **Typed identity session APIs**: `AppendEventRequest` struct and `SessionService::append_event_for_identity()` default method accept `AdkIdentity` for unambiguous session-scoped event appending. Additive `get_for_identity()` and `delete_for_identity()` default methods for typed get/delete. All 8 backends (InMemory, SQLite, PostgreSQL, Redis, MongoDB, Firestore, Neo4j, Vertex) override `append_event_for_identity()`. `InMemorySessionService` uses `AdkIdentity` as its internal HashMap key instead of delimiter-concatenated strings. Typed request helpers on `GetRequest`, `DeleteRequest`, `ListRequest`, and `CreateRequest`.
- **Legacy append guidance**: The typed `AdkIdentity` path is now the preferred session API for new code. The legacy `append_event(&str, ...)` method remains for migration only and is the first legacy identity API slated for deprecation after internal callers complete their move to typed identity.
- **Schema migrations**: Versioned, forward-only migration system for all database backends (SQLite, PostgreSQL, MongoDB, Neo4j). Each backend tracks applied migrations in a `_schema_migrations` registry table with checksums and timestamps. Migrations are idempotent — calling `migrate()` on an already-current database is a no-op.
- **Baseline detection**: `migrate()` detects pre-existing tables created before the migration system and registers them as already applied, avoiding destructive re-creation.
- **`schema_version()` method**: All database backends expose `schema_version()` returning the current migration version (0 if no migrations applied).
- **`from_pool()` / `pool()` methods on `SqliteSessionService`**: Parity with other backends for constructing from an existing connection pool and accessing the inner pool.

#### adk-memory
- **Schema migrations**: Same versioned migration system as `adk-session`, applied to all `adk-memory` database backends (SQLite, PostgreSQL, MongoDB, Neo4j). Each backend has its own migration registry and version tracking.
- **`schema_version()` method**: All database backends expose `schema_version()`.

#### adk-cli / adk-server
- **Production app builder path**: `Launcher` now exposes `build_app()` and `build_app_with_a2a(...)`, making it possible to reuse ADK server wiring while still owning the Axum router, middleware stack, and serve loop in production applications.
- **Launcher A2A and telemetry configuration**: `Launcher` now supports `with_a2a_base_url(...)` and `with_telemetry(...)`, so A2A routes and telemetry initialization are configurable instead of hardcoded in serve mode.
- **Server runtime passthrough**: `ServerConfig` now exposes `with_compaction(...)` and `with_context_cache(...)`, and the SSE + A2A runtime controllers now forward those settings into `RunnerConfig`.

#### adk-runner
- **Typed execution identity**: `InvocationContext` stores `ExecutionIdentity` internally, providing validated typed identity throughout the agent execution lifecycle. Event creation and agent transfers use typed invocation identity after boundary parsing.

#### adk-server / adk-studio
- **Boundary identity parsing**: HTTP and Studio ingress handlers parse user-controlled `app_name`, `user_id`, and `session_id` values into typed identifiers at the boundary, returning `400 Bad Request` on invalid input instead of panicking downstream.

### Changed

#### adk-session
- **`DatabaseSessionService` renamed to `SqliteSessionService`**: The struct, source file (`database.rs` → `sqlite.rs`), and test file (`database_tests.rs` → `sqlite_tests.rs`) have been renamed to accurately reflect the SQLite-only backend. A deprecated type alias `DatabaseSessionService` is provided for backward compatibility. The `database` feature flag remains as an alias for `sqlite`.

#### adk-realtime
- **LiveKit re-exports**: Replaced glob `pub use livekit::prelude::*` with explicit type re-exports in `adk_realtime::livekit` module, eliminating semver hazard from upstream prelude changes
- **Breaking**: Removed crate-level `pub use ::livekit` and `pub use ::livekit_api` re-exports that collided with the `livekit` module namespace — use `adk_realtime::livekit::{AccessToken, VideoGrants}` instead of `adk_realtime::livekit_api::access_token::{AccessToken, VideoGrants}`
- Added `AudioFrame` re-export to `adk_realtime::livekit` for downstream audio processing

#### adk-core
- **`ToolOutcome` struct**: Structured metadata for tool execution results — carries tool name, arguments, success/failure, execution duration, optional error message, and retry attempt number. Available via `CallbackContext::tool_outcome()` in after-tool callbacks.
- **`tool_outcome()` default method on `CallbackContext`**: Returns `Option<ToolOutcome>`, defaulting to `None` for full backward compatibility with existing implementors.
- **`RetryBudget` struct**: Configurable retry policy with `max_retries` and `delay` for automatic tool retry on transient failures.
- **`OnToolErrorCallback` type**: Promoted to `adk-core` as the canonical, framework-level tool-error callback type shared by `adk-agent` and `adk-plugin`.
- **`AfterToolCallbackFull` type**: V2 rich after-tool callback aligned with Python/Go ADK model. Receives `(CallbackContext, Tool, args, response)` and can inspect or replace the tool response sent to the LLM.

#### adk-agent
- **`.toolset()` builder method**: `LlmAgentBuilder` now accepts `Arc<dyn Toolset>` for dynamic per-invocation tool resolution. Toolsets are resolved at the start of each `run()` call using the current `ReadonlyContext`, enabling context-dependent tools (e.g., per-user browser sessions). Static `.tool()` and dynamic `.toolset()` can be mixed freely.
- **`.default_retry_budget()` and `.tool_retry_budget()`**: Configure automatic retry for transient tool failures. Per-tool budgets override the default. When retries are exhausted, the final failure is reported to the LLM.
- **`.circuit_breaker_threshold()`**: Tracks consecutive tool failures per tool name within an invocation. After the configured threshold, the tool is temporarily disabled with an immediate error response to the LLM. Resets at the start of each new invocation.
- **`.on_tool_error()` callback**: Register fallback handlers invoked when a tool fails (after retries are exhausted). Callbacks can return a substitute `Value` used as the function response, or `None` to pass through to the next handler.
- **`.after_tool_callback_full()` builder method**: V2 rich after-tool callback that receives the tool, arguments, and response. Aligned with Python/Go ADK model for first-class tool result handling.

#### adk-browser
- **`BrowserSessionPool`**: Multi-tenant session pool for managing browser sessions across concurrent agent invocations. Supports configurable pool size and session lifecycle management.
- **`BrowserProfile` enum**: Pool-aware toolset creation with `Shared` (pooled) and `Dedicated` (single-session) profiles.
- **JS string escaping**: `escape_js_string()` utility for safe JavaScript injection in evaluate tool.

#### adk-tool
- **Toolset composition**: `adk-tool/src/toolset/` module with composable toolset support for combining multiple tool sources.

#### adk-cli
- **Global provider flags**: `--provider`, `--model`, `--api-key` flags available on all subcommands.
- **First-run setup wizard**: Interactive provider selection and API key configuration on first launch.
- **Default to REPL**: Running `adk-rust` with no subcommand starts an interactive session.

### Fixed

#### adk-auth
- **Cross-role deny precedence**: `AccessControl` and `SsoAccessControl` now evaluate deny rules across all assigned roles before allowing access. Previously, the first allowing role could bypass a deny from another role, making authorization depend on role assignment order.
- **Verified email identity mapping**: `ClaimsMapper::user_id_from_email()` and `TokenClaims::user_id()` now require `email_verified == true` before using an email claim as the effective identity. Unverified emails fall back to `sub`.
- **SSO validation hardening**: OIDC discovery now rejects issuer mismatches, provider validators now enforce `nbf`, JWKS refreshes are single-flight with a cache key cap, and Azure multi-tenant validation can be restricted with `with_allowed_tenants(...)`.
- **Auth bridge implementation**: The `auth-bridge` feature now provides `JwtRequestContextExtractor` for `adk-server`, mapping Bearer tokens into `RequestContext` with validated user IDs and JWT scopes.
- **FileAuditSink mutex poisoning**: `FileAuditSink` now recovers from poisoned mutex instead of panicking, using `unwrap_or_else` to reclaim the lock guard.
- **TokenError placeholder**: `TokenError::placeholder()` now returns a proper error variant instead of a debug-only stub that could mask real token validation failures.
- **ScopedTool/ProtectedTool macro consolidation**: Eliminated duplicated trait implementations between `ScopedTool` and `ProtectedTool` by extracting shared logic into macros, reducing maintenance surface.

#### adk-gemini
- **FunctionCall serialization**: Fixed `thought_signature` leaking inside the `functionCall` JSON object when serializing `Part::FunctionCall`. The Gemini API expects `thoughtSignature` at the Part level only, not inside `functionCall`. The conversion layer in `adk-model` now correctly places the signature at the Part level and omits it from the inner `FunctionCall` struct.
- **Broken serde attributes**: Restored missing `#[serde(skip_serializing_if = "Option::is_none")]` attributes on `FunctionDeclaration`, `FunctionCall`, `FunctionResponse`, and `ToolConfig` fields that had been replaced with invalid placeholder text, causing compilation failures.
- **Non-object tool responses**: Gemini-backed agents now normalize array/scalar tool outputs into a valid object payload before sending `functionResponse.response`. This fixes Gemini tool-calling flows for tools like `RagTool` that naturally return lists of results.

#### adk-agent / adk-runner / adk-core
- **Multi-agent transfer round-trip**: Sub-agents can now transfer back to their parent and peer agents. The runner computes valid transfer targets (parent + peers) and passes them via `RunConfig::transfer_targets`. Previously, sub-agents with no children had an empty valid-agents list, making all transfers fail.
- **Transfer chain support**: The runner now loops on transfers (up to 10 hops) instead of handling only a single transfer. This enables coordinator → sub-agent → coordinator round-trip patterns.
- **Sub-agent conversation history isolation**: When a sub-agent is invoked via transfer, it now receives filtered conversation history that excludes other agents' events. Previously, the sub-agent's LLM saw the parent's tool calls mapped as "model" role, causing it to think work was already done and return immediately.
- **Transfer tool schema**: The `transfer_to_agent` tool declaration now includes valid target names as an `enum` in the JSON schema and lists them in the description, so the LLM knows which agents it can transfer to.
- **`disallow_transfer_to_parent` / `disallow_transfer_to_peers`**: These `LlmAgent` builder flags are now wired up and actively filter the transfer targets list. Previously they were stored but never checked.
- **Agent runtime hardening**: `LlmAgent` now enforces configured input/output guardrails at runtime, normalizes XML tool-call markup before tool dispatch, preserves unique `function_call_id` values per tool invocation, and rejects duplicate sub-agent names during builder validation.
- **Workflow agent contract fixes**: `ParallelAgent` and `ConditionalAgent` now execute their registered before/after callbacks, `IncludeContents::None` now keeps only the current user turn plus injected instructions, and `LoopAgent` maintains local conversation history for direct workflow use outside `adk-runner`.
- **Deterministic LLM routing**: `LlmConditionalAgent` now resolves overlapping route labels deterministically, preferring exact matches and then the longest matching label.

#### adk-browser
- **Centralized `ensure_started()`**: All WebDriver-accessing methods now go through a single session initialization path, eliminating race conditions on first use.
- **Navigation tool response alignment**: `navigate` tool returns consistent structured responses across success and error paths.
- **Tool hardening**: `click`, `evaluate`, `extract`, `type_text`, and `wait` tools handle edge cases (stale elements, timeouts, JS errors) with actionable error messages.

#### adk-model
- **DeepSeek reasoning content**: `Part::Thinking` content is now correctly placed in `reasoning_content` field instead of being mixed into the main `content` field.

#### adk-server
- **Compaction config wiring**: `compaction_config` from server config is now passed through to `RunnerConfig` in both runtime and A2A controllers.

#### adk-agent (Added)
- **Regression test suite**: New `review_regression_tests.rs` with 10 targeted tests covering guardrail runtime enforcement, parallel/conditional agent callbacks, function_call_id uniqueness, `IncludeContents::None` filtering, deterministic LLM routing, sub-agent name uniqueness validation, and tool_call_markup normalization.
- **README accuracy**: Updated README to reflect all current builder methods, correct examples, and accurate feature descriptions.
- **Guardrail example update**: Removed outdated caveat from `guardrail_agent` example that incorrectly stated guardrails were builder-only; example now documents that guardrails are enforced at runtime.

## [0.3.2] - 2026-02-17

### ⭐ Highlights
- **9 New LLM Providers**: xAI, Fireworks AI, Together AI, Mistral AI, Perplexity, Cerebras, SambaNova (OpenAI-compatible), Amazon Bedrock (AWS SDK), and Azure AI Inference (reqwest) — all feature-gated with contract tests
- **adk-rag**: New RAG crate with modular pipeline, 6 vector store backends (InMemory, Qdrant, LanceDB, pgvector, SurrealDB), 3 chunking strategies, and agentic retrieval via `RagTool`
- **Generation Config on Agents**: `LlmAgentBuilder` now supports `temperature()`, `top_p()`, `top_k()`, `max_output_tokens()` convenience methods and full `generate_content_config()` for agent-level LLM tuning
- **Gemini Model URL Fix**: `Model::Custom` variant now correctly prefixes `models/` in API URLs, fixing `PerformRequestNew` errors for all Gemini tool-calling examples
- **Gemini Models Discovery API**: New `list_models()` and `get_model()` methods on `Gemini` client for runtime model discovery
- **Expanded Model Enum**: `Model` enum expanded from 5 to 22 variants covering Gemini 3, 2.5, 2.0, and embedding models

### Added

#### adk-rag (NEW CRATE)
- New `adk-rag` crate: modular Retrieval-Augmented Generation for ADK-Rust agents
- Core traits: `EmbeddingProvider`, `VectorStore`, `Chunker`, `Reranker`
- `InMemoryVectorStore` with cosine similarity search (no external deps)
- Three chunking strategies: `FixedSizeChunker`, `RecursiveChunker`, `MarkdownChunker`
- `RagPipeline` orchestrator for ingest (chunk → embed → store) and query (embed → search → rerank → filter) workflows
- `RagPipelineBuilder` with builder-pattern configuration
- `RagTool` implementing `adk_core::Tool` for agentic retrieval — agents call `rag_search` on demand
- Feature-gated embedding providers: `GeminiEmbeddingProvider` (`gemini`), `OpenAIEmbeddingProvider` (`openai`)
- Feature-gated vector stores: `QdrantVectorStore` (`qdrant`), `LanceDBVectorStore` (`lancedb`), `PgVectorStore` (`pgvector`)
- `SurrealVectorStore` (`surrealdb`) with HNSW cosine indexing — supports in-memory, RocksDB, and remote server modes
- `rag` feature flag added to `adk-rust` umbrella crate (included in `full`)
- 7 examples: `rag_basic`, `rag_markdown`, `rag_agent`, `rag_recursive`, `rag_reranker`, `rag_multi_collection`, `rag_surrealdb`
- Official documentation page at `docs/official_docs/tools/rag.md` with validated code samples

#### adk-agent
- `LlmAgentBuilder::generate_content_config()` — set full `GenerateContentConfig` at the agent level
- `LlmAgentBuilder::temperature()` — convenience method for setting default temperature
- `LlmAgentBuilder::top_p()` — convenience method for setting default top-p
- `LlmAgentBuilder::top_k()` — convenience method for setting default top-k
- `LlmAgentBuilder::max_output_tokens()` — convenience method for setting default max output tokens
- Agent-level generation config is merged with `output_schema` in the LLM request loop

#### adk-core
- `GenerateContentConfig` now derives `Default`

#### adk-gemini
- `Model` enum expanded with 17 new variants:
  - Gemini 3: `Gemini3ProPreview`, `Gemini3ProImagePreview`, `Gemini3FlashPreview`
  - Gemini 2.5: `Gemini25Pro`, `Gemini25ProPreviewTts`, `Gemini25FlashPreview092025`, `Gemini25FlashImage`, `Gemini25FlashLive122025`, `Gemini25FlashLive092025`, `Gemini25FlashPreviewTts`, `Gemini25FlashLite`, `Gemini25FlashLitePreview092025`
  - Gemini 2.0 (deprecated): `Gemini20Flash`, `Gemini20Flash001`, `Gemini20FlashExp`, `Gemini20FlashLite`, `Gemini20FlashLite001`
- `Model::Gemini25FlashImagePreview` marked `#[deprecated]` (use `Gemini25FlashImage`)
- `Model::Gemini20Flash*` variants marked `#[deprecated]` (shutting down March 31, 2026)
- `model_info` module with `ModelInfo` and `ListModelsResponse` types for the Models API
- `Gemini::list_models(page_size)` — paginated stream of available model metadata
- `Gemini::get_model(name)` — fetch metadata for a specific model (token limits, supported methods, etc.)
- `GeminiBackend::list_models()` and `GeminiBackend::get_model()` trait methods with default unsupported impls
- `StudioBackend` implementation of `list_models` and `get_model` via REST
- `ModelInfo` and `ListModelsResponse` re-exported from `prelude`

#### adk-studio
- Generation config parameters (`temperature`, `top_p`, `top_k`, `max_output_tokens`) added to `AgentSchema`
- Advanced Settings section in LlmProperties panel for configuring generation parameters
- Code generation emits `.temperature()`, `.top_p()`, `.top_k()`, `.max_output_tokens()` builder calls

#### adk-model — New Providers
- **Fireworks AI** (`fireworks` feature) — OpenAI-compatible provider for fast open-model inference. Default model: `accounts/fireworks/models/llama-v3p1-8b-instruct`. Env: `FIREWORKS_API_KEY`
- **Together AI** (`together` feature) — OpenAI-compatible provider for hosted open models. Default model: `meta-llama/Llama-3.3-70B-Instruct-Turbo`. Env: `TOGETHER_API_KEY`
- **Mistral AI** (`mistral` feature) — OpenAI-compatible provider for Mistral cloud models. Default model: `mistral-small-latest`. Env: `MISTRAL_API_KEY`
- **Perplexity** (`perplexity` feature) — OpenAI-compatible provider for search-augmented LLM. Default model: `sonar`. Env: `PERPLEXITY_API_KEY`
- **Cerebras** (`cerebras` feature) — OpenAI-compatible provider for ultra-fast inference. Default model: `llama-3.3-70b`. Env: `CEREBRAS_API_KEY`
- **SambaNova** (`sambanova` feature) — OpenAI-compatible provider for fast inference. Default model: `Meta-Llama-3.3-70B-Instruct`. Env: `SAMBANOVA_API_KEY`
- **Amazon Bedrock** (`bedrock` feature) — AWS SDK Converse API with IAM/STS authentication, streaming and non-streaming support. Default model: `anthropic.claude-sonnet-4-20250514-v1:0`. Uses AWS credential chain
- **Azure AI Inference** (`azure-ai` feature) — reqwest-based client for Azure AI Inference endpoints with `api-key` header auth, streaming SSE and non-streaming JSON. Env: `AZURE_AI_API_KEY`
- `all-providers` feature now includes all eight new provider feature flags
- Contract tests (`ProviderSpec` + `provider_contract_tests!` macro) for all eight new providers
- Comprehensive rustdoc with quick-start examples for all new provider types

#### Examples
- `gemini_multimodal` — inline image analysis, multi-image comparison, and vision agent pattern using `Part::InlineData` with Gemini
- `anthropic_multimodal` — image analysis with Claude using `Part::InlineData` (requires `--features anthropic`)
- `multi_turn_tool` — inventory management scenario demonstrating multi-turn tool conversations with both Gemini (default) and OpenAI (`--features openai`)
- `rag_surrealdb` — SurrealDB vector store with embedded in-memory mode

### Fixed
- **adk-server**: Runtime endpoints (`run_sse`, `run_sse_compat`) now process attachments and `inlineData` instead of silently dropping them — base64 validation, size limits, and per-provider content conversion (#142, #143)
- **adk-model**: All providers now handle `InlineData` and `FileData` parts — native image/audio/PDF blocks for Anthropic and OpenAI, text fallback for DeepSeek/Groq/Ollama, Gemini response `InlineData` no longer silently dropped (#142, #143)
- **adk-runner**: `conversation_history()` now preserves `function`/`tool` content roles instead of overwriting them to `model`, fixing multi-turn tool conversations (#139)
- **adk-gemini**: `PerformRequestNew` error variant now displays the underlying reqwest error instead of swallowing it
- **adk-gemini**: `From<String> for Model` now correctly maps known model names (e.g. `"gemini-2.5-flash"`) to proper enum variants instead of always creating `Custom`
- **adk-gemini**: `Model::Custom` `Display` impl now adds `models/` prefix when missing, fixing broken API URLs like `gemini-2.5-flash:streamGenerateContent` → `models/gemini-2.5-flash:streamGenerateContent`

### Changed
- CI: sccache stats, test results, and clippy summary now appear in GitHub Actions step summary
- CI: devenv scripts renamed to `ws-*` prefix to avoid collisions with Cargo binaries
- `AGENTS.md` consolidated with crates.io publishing guide and PR template improvements
- Removed broken `.pre-commit-config.yaml` symlink

### Contributors
Thanks to the following people for their contributions to this release:
- **@mikefaille** — major contributions to `adk-realtime` (tokio-tungstenite upgrade, rustls migration), LiveKit WebRTC bridge groundwork, CI improvements (sccache summaries, devenv script fixes), environment sync, documentation consolidation, and PR template (#134, #136, #137)
- **@rohan-panickar** — attachment support for runtime endpoints and multi-provider content conversion (#142, #143), fix for tool context role preservation (#139)
- **@dhruv-pant** — Gemini service account auth and configurable retry logic

## [0.3.1] - 2026-02-14

### ⭐ Highlights
- **Vertex AI Streaming**: `adk-gemini` refactored with `GeminiBackend` trait — pluggable `StudioBackend` (REST) and `VertexBackend` (REST SSE streaming + gRPC fallback)
- **Realtime Stabilization**: `adk-realtime` audio transport rewritten with raw bytes, Gemini Live session corrected, event types renamed for OpenAI SDK alignment
- **Multi-Provider Codegen**: ADK Studio code generation now supports Gemini, OpenAI, Anthropic, DeepSeek, Groq, and Ollama (was hardcoded to Gemini)
- **2026 Model Names**: All docs, examples, and source defaults updated to current model names (gemini-2.5-flash, gpt-5-mini, claude-sonnet-4-5-20250929, etc.)
- **Response Parsing Tests**: 25 rigorous tests covering Gemini response edge cases (safety ratings, streaming chunks, function calls, grounding metadata, citations)
- **Code Health**: Span-based line numbers in doc-audit analyzer, validation refactor in adk-ui, dead code cleanup, CONTRIBUTING.md rewrite

### Added

#### adk-gemini
- `GeminiBackend` trait with `send_request()` and `send_streaming_request()` methods
- `StudioBackend` — AI Studio REST implementation (default)
- `VertexBackend` — Vertex AI REST SSE streaming with gRPC fallback, ADC/service account/WIF auth
- `GeminiBuilder` for constructing clients with explicit backend selection
- `Model::GeminiEmbedding001` variant for `gemini-embedding-001` (3072 dimensions, replaces `text-embedding-004`)
- `Model::TextEmbedding004` marked `#[deprecated]` with compiler warning
- 25 response parsing tests: basic text, multi-candidate, safety ratings (string + numeric), blocked prompts, streaming chunks, function calls, inline data, grounding metadata, citations, usage metadata with thinking tokens, all FinishReason variants, unknown enum graceful degradation, round-trip serialization

#### adk-realtime
- Audio transport changed from `String` (base64) to `Vec<u8>` (raw bytes) with custom serde for base64 wire format
- `BoxedModel` changed from `Box<dyn RealtimeModel>` to `Arc<dyn RealtimeModel>` for thread-safe sharing
- ClientEvent renames: `AudioInput`→`AudioDelta`, `AudioCommit`→`InputAudioBufferCommit`, `AudioClear`→`InputAudioBufferClear`, `ItemCreate`→`ConversationItemCreate`, `CreateResponse`→`ResponseCreate`, `CancelResponse`→`ResponseCancel`
- `EventHandler::on_audio` and `AudioCallback` changed from `&str` (base64) to `&[u8]` (raw bytes)
- Gemini Live session rewrite: `send_text` uses `client_content` (correct Gemini API), handles binary WebSocket messages, `GeminiLiveBackend` enum for backend selection
- `GeminiRealtimeModel` now accepts `GeminiLiveBackend` instead of raw API key string
- `RealtimeError::audio()` convenience constructor
- Added `bytes`, `bytemuck` dependencies; optional `adk-gemini` dep behind `gemini` feature flag
- Feature flags: `openai`, `gemini`, `full`

#### adk-rag (NEW CRATE)
- New `adk-rag` crate: modular Retrieval-Augmented Generation for ADK-Rust agents
- Core traits: `EmbeddingProvider`, `VectorStore`, `Chunker`, `Reranker`
- `InMemoryVectorStore` with cosine similarity search (no external deps)
- Three chunking strategies: `FixedSizeChunker`, `RecursiveChunker`, `MarkdownChunker`
- `RagPipeline` orchestrator for ingest (chunk → embed → store) and query (embed → search → rerank → filter) workflows
- `RagTool` implementing `adk_core::Tool` for agentic retrieval — agents call `rag_search` on demand
- Feature-gated embedding providers: `GeminiEmbeddingProvider` (`gemini`), `OpenAIEmbeddingProvider` (`openai`)
- Feature-gated vector stores: `QdrantVectorStore` (`qdrant`), `LanceDBVectorStore` (`lancedb`), `PgVectorStore` (`pgvector`)
- `rag` feature flag added to `adk-rust` umbrella crate (included in `full`)
- 6 examples: `rag_basic`, `rag_markdown`, `rag_agent`, `rag_recursive`, `rag_reranker`, `rag_multi_collection`
- Official documentation page at `docs/official_docs/tools/rag.md` with validated code samples
- Published to crates.io as `adk-rag v0.3.1`

#### adk-studio
- Multi-provider LLM support in code generation (Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama)
- Provider-specific environment variable detection and validation
- Ollama local model support with configurable base URL

#### Examples
- `verify_backend_selection` — validates Studio backend (default, with_model, builder, streaming, embedding, v1 API)
- `verify_vertex_streaming` — validates Vertex AI backend (non-streaming, REST SSE streaming, embedding)

### Fixed
- **adk-model**: `GeminiModel::new()` now uses `Gemini::with_model(api_key, model_name)` instead of ignoring the provided model name (bug #77)
- **adk-studio**: CORS restricted to localhost origins only (was allowing all origins)
- **adk-ui**: `NumberInput` validation no longer false-fails when only `min` is set (`Some(min) > None` was always true)
- **adk-graph**: Replaced `eprintln!("DEBUG: ...")` with `tracing::debug!()` in `AgentNode::execute_stream` and `CompiledGraph::stream` (stderr leakage in library code)
- **adk-ui**: Validation refactored from monolithic match into per-type `Validate` trait impls (Text, Button, TextInput, NumberInput, Select, Table, Chart, Card, Modal, Stack, Grid, Tabs)

### Changed
- All model name defaults updated to 2026 versions across 95+ files:
  - `gemini-2.0-flash` → `gemini-2.5-flash`
  - `gpt-4o` / `gpt-4o-mini` → `gpt-5-mini`
  - `claude-sonnet-4-20250514` → `claude-sonnet-4-5-20250929`
  - `gemini-2.0-flash-live-preview-04-09` → `gemini-live-2.5-flash-native-audio`
- `CONTRIBUTING.md` rewritten with full 25+ crate inventory, build commands, architecture notes
- `.kiro/` and `.vite/` excluded from git tracking
- `.gitignore` cleaned up (removed absolute paths, duplicate entries)
- Added `.skills/` with Kiro skill definitions for agent workflows

### Documentation
- Updated all example model names to 2026 versions (PRs #79-#82)
- Updated source code default model names across all provider crates

## [0.3.0] - 2026-02-08

### ⭐ Highlights
- **Context Compaction**: Sliding-window summarization of older events to reduce LLM context size (ADK Python parity)
- **Workflow Agent Hardening**: ConditionalAgent, LlmConditionalAgent, and ParallelAgent production fixes
- **adk-core Production Hardening**: Security limits, validation, provider-agnostic Event, hand-written template parser
- **Action Node Code Generation**: Full Rust codegen for HTTP, Database, Email, and Code action nodes
- **Workflow Triggers**: Complete trigger system with webhook, schedule, and event triggers
- **rmcp 0.14 Upgrade**: Updated MCP integration with HTTP transport, authentication, and auto-reconnect
- **Plugin System**: Extensible callback architecture for agent lifecycle hooks (adk-go parity)
- **OpenAI Structured Output**: `output_schema` now works with OpenAI/Azure via `response_format` API

### Added

#### adk-core
- `EventCompaction` struct for compacted event metadata (start/end timestamps, summary content)
- `EventActions.compaction` field for marking events as compaction summaries
- `BaseEventsSummarizer` trait for custom summarization strategies
- `EventsCompactionConfig` struct (compaction_interval, overlap_size, summarizer)
- `validate_state_key()` and `MAX_STATE_KEY_LEN` (256 bytes) for state key validation
- `MAX_INLINE_DATA_SIZE` (10MB) limit on `Part::InlineData`
- `provider_metadata: HashMap<String, String>` on `Event` — provider-agnostic replacement for GCP-specific fields
- `has_trailing_code_execution_result()` on `Event` for detecting pending code execution results
- Hand-written placeholder parser for instruction templates (replaces regex dependency)
- `LlmRequest::with_response_schema()` and `with_config()` builder methods for structured output

#### adk-agent
- `LlmEventSummarizer` — LLM-based event summarizer with configurable prompt template
- `LlmAgentBuilder::max_iterations()` to configure maximum LLM round-trips (default: 100)

#### adk-runner
- `compaction_config` field on `RunnerConfig` for enabling automatic context compaction
- Re-exports `BaseEventsSummarizer` and `EventsCompactionConfig` from `adk-core`
- Compaction triggers after invocation when user-event count reaches interval
- `MutableSession::conversation_history()` respects compaction events — replaces old events with summary

#### adk-model
- OpenAI/Azure clients now wire `output_schema` to `response_format` with `json_schema` type
  - Auto-injects `additionalProperties: false` at root level for strict mode compliance
  - Uses sanitized model name for schema name

#### adk-tool
- `ConnectionRefresher` for automatic MCP reconnection
  - `ConnectionFactory` trait for creating new connections
  - `RefreshConfig` for retry settings (max_attempts, retry_delay_ms)
  - `RetryResult<T>` to indicate if reconnection occurred
  - `should_refresh_connection()` to detect refreshable errors
  - `SimpleClient` wrapper for servers without reconnect support
  - Handles: connection closed, EOF, broken pipe, session not found, transport errors
- `McpHttpClientBuilder` for remote MCP server connections
  - Streamable HTTP transport (SEP-1686 compliant)
  - `with_auth()` for authentication configuration
  - `timeout()` for request timeout configuration
  - `header()` for custom headers
- `McpAuth` enum for MCP authentication
  - `McpAuth::bearer(token)` - Bearer token authentication
  - `McpAuth::api_key(header, key)` - API key in custom header
  - `McpAuth::oauth2(config)` - OAuth2 client credentials flow
- `OAuth2Config` for OAuth2 authentication (client credentials flow, token caching)
- `McpTaskConfig` for long-running operations (polling, timeout, max attempts)
- New feature flag `http-transport` for remote MCP servers
- `AgentTool` now forwards `state_delta` and `artifact_delta` to parent context
- Upgraded rmcp from 0.9 to 0.14

#### adk-plugin
- New plugin system crate (adk-go feature parity)
  - `Plugin` and `PluginConfig` for bundling related callbacks
  - `PluginBuilder` for fluent plugin construction
  - `PluginManager` for coordinating callback execution across plugins
  - Run lifecycle callbacks: `on_user_message`, `on_event`, `before_run`, `after_run`
  - Agent callbacks: `before_agent`, `after_agent`
  - Model callbacks: `before_model`, `after_model`, `on_model_error`
  - Tool callbacks: `before_tool`, `after_tool`, `on_tool_error`
  - Helper functions: `log_user_messages()`, `log_events()`, `collect_metrics()`

#### adk-server
- `TaskStore` for in-memory A2A task persistence and retrieval

#### adk-studio
- HTTP action node code generation (all methods, auth, body types, response handling)
- Database action node code generation (PostgreSQL, MySQL, SQLite via sqlx; MongoDB; Redis)
- Email action node code generation (SMTP send via lettre; IMAP monitor via imap + native-tls)
- Code action node code generation (JavaScript via boa_engine with sandboxing)
- Predecessor output injection for all action node types
- Smart Build button (detects when recompilation is needed)
- Webhook trigger endpoints (async, sync, GET)
- Schedule trigger service (cron-based with `last_executed` tracking)
- Event trigger endpoints (source/eventType matching, JSONPath filters)
- Trigger-aware Run button with type-specific default prompts
- Webhook event SSE notifications to UI

#### Examples
- `examples/ralph`: Autonomous agent with loop workflow, PRD management, and file/git/test tools
- `examples/ollama_structured`: Structured JSON output with local Ollama models
- `examples/openai_local`: OpenAI client with local models via `OpenAIConfig::compatible()`
- `examples/openai_structured_basic`: Basic structured output example with OpenAI
- `examples/openai_structured_strict`: Strict schema example with nested objects
- `examples/mcp_http`: Remote MCP server example (Fetch, Sequential Thinking)
- `examples/mcp_oauth`: GitHub Copilot MCP authentication example

#### Dependencies (Generated Projects)
- `reqwest` — auto-detected for HTTP action nodes
- `sqlx` — auto-detected per database type (postgres/mysql/sqlite features)
- `mongodb` — auto-detected for MongoDB action nodes
- `redis` — auto-detected for Redis action nodes
- `lettre` — auto-detected for Email send nodes
- `imap` + `native-tls` — auto-detected for Email monitor nodes
- `boa_engine` — auto-detected for Code action nodes

### Fixed
- **adk-agent**: `ConditionalAgent::sub_agents()` now returns branch agents (was returning empty slice)
- **adk-agent**: `LlmConditionalAgent::sub_agents()` now returns route + default agents (was returning empty slice)
- **adk-agent**: `ParallelAgent` now drains all futures before propagating first error (prevents resource leaks)
- **adk-agent**: Default max iterations increased from 10 to 100 for `LlmAgent`
- **adk-core**: `function_call_ids()` now falls back to function name when call ID is `None` (Gemini compatibility)
- **adk-core**: Removed GCP-specific fields from `Event` (replaced with `provider_metadata`)
- **adk-core**: Removed phantom `adk-3d-ui` workspace member
- **adk-model**: `output_schema` was ignored by OpenAI client — now properly sent as `response_format`
- **adk-model**: Fixed rustdoc bare URL warning in `AzureConfig` documentation
- **adk-session**: Replaced all `unwrap()` calls with proper error handling in `DatabaseSessionService`
- **adk-server**: A2A `tasks/get` endpoint now returns stored tasks instead of empty response
- **adk-studio**: Replaced non-existent `NodeError::Other` with `GraphError::NodeExecutionFailed` in all generated code
- **adk-studio**: Fixed sqlx type inference in database codegen by splitting fetch and map operations
- **adk-studio**: Added missing `sqlx::Row` and `sqlx::Column` imports in database codegen
- **adk-studio**: Fixed moved value error when capturing row count before consuming rows in JSON macro
- **adk-studio**: Run button now correctly uses trigger-specific default prompts
- **adk-studio**: `sendingRef` now properly resets on cancel, allowing re-runs
- **adk-studio**: Cron parsing now uses 6-field format (with seconds) for `cron` crate compatibility
- **adk-tool**: Bearer auth now passes raw token (rmcp adds "Bearer " prefix automatically)
- **Security**: Updated lodash to fix prototype pollution vulnerability (CVE-2020-8203)
- **Security**: Updated vite/esbuild to fix server.fs.deny bypass (CVE-2025-0291)
- **Security**: Updated rsa crate to fix Marvin Attack vulnerability (RUSTSEC-2023-0071)

### Documentation
- Added context compaction guide: `docs/official_docs/sessions/context-compaction.md`
- Updated all crate READMEs with v0.3.0 version references
- Updated all official docs with v0.3.0 version references
- Updated adk-core, adk-agent, adk-runner READMEs with compaction, security, and production hardening details
- Updated events and runner official docs with new EventActions fields and compaction config

### Migration Guide

**From 0.2.x to 0.3.0:**

- All crate versions bumped to `0.3.0`. Update your `Cargo.toml` dependencies.
- `Event` no longer has GCP-specific fields — use `provider_metadata` HashMap instead.
- rmcp 0.14 breaking changes were handled internally in `adk-tool`. Your existing MCP code using `McpToolset::new(client)` continues to work unchanged.

**New features available:**

```rust
// Context compaction for long-running sessions
use adk_runner::{Runner, RunnerConfig, EventsCompactionConfig};
use adk_agent::LlmEventSummarizer;

let config = RunnerConfig {
    compaction_config: Some(EventsCompactionConfig {
        compaction_interval: 3,
        overlap_size: 1,
        summarizer: Arc::new(LlmEventSummarizer::new(model.clone())),
    }),
    ..
};

// HTTP transport for remote MCP servers (requires http-transport feature)
use adk_tool::McpHttpClientBuilder;

let toolset = McpHttpClientBuilder::new("https://remote.mcpservers.org/fetch/mcp")
    .timeout(Duration::from_secs(30))
    .connect()
    .await?;

// Authentication for protected MCP servers
use adk_tool::{McpHttpClientBuilder, McpAuth};

let toolset = McpHttpClientBuilder::new("https://api.githubcopilot.com/mcp/")
    .with_auth(McpAuth::bearer(std::env::var("GITHUB_TOKEN")?))
    .connect()
    .await?;
```

## [0.2.0] - 2026-01-06

### ⭐ Highlights
- **Documentation Overhaul**: All crate READMEs validated against actual implementations
- **API Consistency**: Fixed incorrect API examples across documentation

### Fixed
- Fixed `LlmAgentBuilder` API: use `.tool()` in loop instead of non-existent `.tools(vec![...])`
- Fixed `Runner::new()` examples: use `Launcher` for simple cases, `RunnerConfig` for advanced
- Fixed `SessionService::create()` API: use `CreateRequest` struct
- Fixed `BrowserConfig` API: use builder pattern instead of `::new(url)`
- Fixed `LoopAgent` API: use `vec![]` and `with_max_iterations()`
- Fixed dotenv → dotenvy in examples
- Removed non-existent `Launcher` methods from docs (`with_server_mode`, `with_user_id`, `with_session_id`)

### Changed
- All ADK crates bumped to version 0.2.0
- Rust edition updated to 2024, requires Rust 1.94+

## [0.1.9] - 2026-01-03

### ⭐ Highlights
- **mistral.rs Integration**: Complete native local LLM inference via `adk-mistralrs` crate
- **Production-Ready Error Handling**: Comprehensive error types with actionable suggestions
- **Diagnostic Logging**: Structured tracing with timing spans for model loading and inference
- **Performance Benchmarks**: Criterion benchmarks for configuration and conversion operations

### Added
- **adk-mistralrs** (`adk-mistralrs`): Native mistral.rs integration for local LLM inference
  - `MistralRsModel`: Basic text generation implementing ADK `Llm` trait
  - `MistralRsAdapterModel`: LoRA/X-LoRA adapter support with hot-swapping
  - `MistralRsVisionModel`: Vision-language model support for image understanding
  - `MistralRsEmbeddingModel`: Semantic embeddings for RAG and search
  - `MistralRsSpeechModel`: Text-to-speech synthesis with multi-speaker support
  - `MistralRsDiffusionModel`: Image generation with FLUX models
  - `MistralRsMultiModel`: Multi-model serving with routing
  - ISQ (In-Situ Quantization) support for memory-efficient inference
  - PagedAttention for longer context windows
  - UQFF pre-quantized model loading for faster startup
  - MCP client integration for external tools
  - MatFormer support for Gemma 3n models
  - Multi-GPU model splitting across devices
- **Error handling improvements**:
  - Structured error types with contextual fields (model_id, reason, suggestion)
  - Convenience constructors for common error patterns
  - Error classification methods (`is_recoverable()`, `is_config_error()`, `is_resource_error()`)
  - Actionable suggestions based on error content
- **Diagnostic logging**:
  - `tracing_utils` module with timing utilities
  - `TimingGuard` for automatic operation timing
  - Logging functions for model loading, inference, embeddings, image/speech generation
  - Token throughput metrics in inference logs
- **CI integration**:
  - `.github/workflows/mistralrs-tests.yml` for mistral.rs-specific testing
  - Separate jobs for unit tests, property tests, doc tests, and clippy
  - Optional integration tests with manual trigger
- **Performance benchmarks**:
  - Criterion benchmarks for configuration, error creation, type conversions
  - MCP configuration benchmarks
  - Optional inference benchmarks behind `bench-inference` feature flag
- **Property tests**:
  - 21 error message quality tests validating contextual information and suggestions
  - Tests for error classification consistency
  - Tests for all error types (model load, inference, adapters, media processing, etc.)
- **FileData Part support**: Added `Part::FileData` variant handling in `adk-server` and `adk-cli`
- **New examples**: `mistralrs_speech` (TTS) and `mistralrs_diffusion` (image generation)

### Changed
- All ADK crates bumped to version 0.1.9
- `adk-mistralrs` version updated to 0.1.9
- Updated README with benchmark documentation and performance tips
- Enhanced error messages with platform-specific suggestions (CUDA, Metal)

### Fixed
- Non-exhaustive pattern match for `Part::FileData` in `adk-server/src/a2a/parts.rs`
- Non-exhaustive pattern match for `Part::FileData` in `adk-cli/src/console.rs`

## [0.1.9] - 2025-12-28

### ⭐ Highlights
- **ADK Studio**: Complete visual agent builder with drag-and-drop workflow design
- **Real-Time Streaming**: Live SSE streaming with agent animations and trace events
- **Code Generation**: Compile visual workflows to production Rust code
- **Rust 2024 Edition**: Migrated to Rust 2024 edition for latest language features

### Added
- **ADK Studio** (`adk-studio`): Visual agent development environment
  - Drag-and-drop agent creation with ReactFlow-based canvas
  - Full agent palette: LLM Agent, Sequential, Loop, Parallel, Router agents
  - Tools support: Function, MCP, Browser, Google Search, Load Artifact, Exit Loop
  - Real-time SSE streaming with chat interface and session management
  - **Code generation**: Compile visual designs to Rust code with one click
  - **Build system**: Compile and run generated Rust executables from Studio
  - Monaco Editor integration for viewing/editing generated code
  - MenuBar with File, Templates, Help menus and 7 agent templates
  - Sub-agent support in container nodes with proper event ordering
  - MCP server templates with friendly display names and timeout handling
  - Function tool templates with description editing
  - Session memory persistence across chat interactions
  - Agent rename and enhanced LLM property configuration
- **Studio UI architecture** (`studio-ui`):
  - Component extraction: Canvas reduced by 83% via modular architecture
  - Custom node components: `LlmAgentNode`, `RouterNode`, `ThoughtBubble`
  - Layout system with auto-layout, horizontal/vertical toggle
  - Node activity animations during execution
  - State management with Zustand store
  - Real-time trace events in Events tab
- **Real-time streaming** (`StreamMode::Messages`):
  - Live agent execution with proper event accumulation
  - Trace events for tool calls/results in SSE stream
  - Agent start and model call events for detailed debugging
  - Node start/end trace events for sub-agent tracking
- **Router Agent**: Conditional routing based on LLM decisions
- **Codegen example**: `codegen_demo` showing code generation from all templates
- **Host flag**: `--host` flag for backend and studio management scripts

### 🔥 Breaking Changes
- **Rust 2024 Edition**: All crates now use `edition = "2024"` (requires Rust 1.94+)
- **Workspace Restructure**: `vendor/gemini-rust` → `adk-gemini`
  - Import paths change from `gemini_rust::*` to `adk_gemini::*`
  - Standardized workspace dependencies for consistency

### Changed
- All ADK crates bumped to version 0.1.9
- Generated `Cargo.toml` now uses ADK version 0.1.9
- Improved sub-agent display in containers (robot icon, LLM Agent label, tool descriptions)
- Sequential agent now properly passes conversation history between sub-agents
- Output mapper now accumulates text correctly across agent events
- Auto-detect reqwest dependency in codegen, add User-Agent header
- Build cache invalidation on project changes

### Fixed
- **adk-studio**: Real-time streaming now works correctly
- **adk-studio**: Drag-drop fixed for both agents and tools
- **adk-studio**: Keyboard delete properly handles agent/tool deletion
- **adk-studio**: Agents sorted by workflow order, positioned at top-left
- **adk-studio**: Save on agent delete, handle keyboard delete properly
- **adk-studio**: MCP codegen only generates tool loop if config exists
- **adk-studio**: Sub-agent tools properly added to builders in containers
- **adk-studio**: Tool clicks open config panel, entire tool item clickable
- **studio-ui**: Prevent layout rearrangement during chat execution
- **studio-ui**: Thought bubble moved inside node to prevent overlap
- **adk-agent**: Sequential agent properly passes conversation history between sub-agents
- **adk-agent**: Output mapper accumulates text correctly across agent events
- **adk-graph**: Sub-agent events include agent name in completion log
- **adk-graph**: Proper node_start/node_end trace events emitted

### Internal
- Tracing subscriber with JSON output for telemetry
- Grounding metadata display with markdown rendering
- Screenshot display in console
- Build output now streams in real-time
- Graph-based workflow design document added
- ADK Studio roadmap and UI requirements updated

## [0.1.7] - 2025-12-14

### Added
- **adk-guardrail**: New crate for agent safety and validation
  - `Guardrail` trait with async `validate()` returning `Pass`, `Fail`, or `Transform`
  - `GuardrailSet` and `GuardrailExecutor` for parallel execution with early exit
  - `Severity` levels: `Low`, `Medium`, `High`, `Critical`
  - Built-in guardrails:
    - `PiiRedactor` - Detects and redacts Email, Phone, SSN, CreditCard, IpAddress
    - `ContentFilter` - Blocks harmful content, off-topic responses, keywords, max length
    - `SchemaValidator` - JSON schema validation with markdown code block extraction
- **adk-agent**: Guardrails integration (feature-gated)
  - `LlmAgentBuilder::input_guardrails()` - Validate/transform user input
  - `LlmAgentBuilder::output_guardrails()` - Validate/transform model output
  - Enable with `adk-agent = { features = ["guardrails"] }`
- 3 new guardrail examples:
  - `guardrail_basic` - PII redaction and content filtering
  - `guardrail_schema` - JSON schema validation
  - `guardrail_agent` - Full agent integration
- **translator example**: Refactored with adk-rust best practices

### Changed
- Roadmap documents added for guardrails, cloud integrations, enterprise, adk-studio
- Updated adk-ui roadmap to implemented status

## [0.1.6] - 2025-12-12

### Added
- **adk-ui**: New modules for improved LLM reliability and developer experience:
  - `prompts.rs` - Tested system prompts (`UI_AGENT_PROMPT`) with few-shot examples
  - `templates.rs` - 10 pre-built UI templates (Registration, Login, Dashboard, etc.)
  - `validation.rs` - Server-side validation with `validate_ui_response()`
- **adk-ui**: Component enhancements:
  - `Button`: Added `icon` field for icon buttons
  - `TextInput`: Added `min_length`, `max_length` validation
  - `NumberInput`: Added `default_value` field
  - `Table`: Added `sortable`, `striped`, `page_size` fields
  - `Chart`: Added `x_label`, `y_label`, `show_legend`, `colors` fields
  - `render_layout`: Added `key_value`, `list`, `code_block` section types
- **npm package**: Published `@zavora-ai/adk-ui-react@0.1.6` to npm
- **streaming_demo**: New example showing `UiUpdate` for real-time progress bar updates
- React client improvements:
  - Clickable example prompts table with instant send
  - Dark mode and theme support
  - Table sorting and pagination
  - Chart colors and axis labels

### Fixed
- All 10 render tools now use proper error handling (replaced `unwrap()`)
- TypeScript types updated for all new Rust schema fields

### Changed
- All crates now use workspace version inheritance (`version.workspace = true`)

## [0.1.5] - 2025-12-10

### Added
- **DeepSeek provider support**: Native integration with DeepSeek's LLM models
  - `DeepSeekClient` and `DeepSeekConfig` for easy configuration
  - Support for `deepseek-chat` (standard) and `deepseek-reasoner` (thinking mode)
  - Thinking mode with chain-of-thought reasoning (`<thinking>` tags in output)
  - Context caching for 10x cost reduction on repeated prefixes
  - Full function calling/tool support
  - Streaming support with proper response accumulation
  - Feature flag: `adk-model = { features = ["deepseek"] }`
- 8 new DeepSeek examples:
  - `deepseek_basic` - Basic chat completion
  - `deepseek_reasoner` - Thinking mode with chain-of-thought
  - `deepseek_tools` - Function calling with weather/calculator tools
  - `deepseek_thinking_tools` - Combined reasoning and tool use
  - `deepseek_caching` - Context caching demonstration
  - `deepseek_sequential` - Multi-agent pipeline (Researcher → Analyst → Writer)
  - `deepseek_supervisor` - Supervisor pattern with specialist agents
  - `deepseek_structured` - Structured JSON output
- DeepSeek documentation in official docs and all READMEs

### Fixed
- CI linker OOM crashes: Now using `mold` linker with reduced debug info
- Function response role mapping for DeepSeek API (uses "tool" not "function")
- Placeholder GitHub URLs updated to `zavora-ai/adk-rust`

## [0.1.4] - 2025-12-09

### Added
- **adk-graph crate**: LangGraph-style workflow orchestration
  - `StateGraph` for building complex agent workflows with state channels
  - `AgentNode` for wrapping LLM agents as graph nodes with input/output mappers
  - Conditional routing with `Router::by_field` and custom predicates
  - Human-in-the-loop (HITL) interrupts with `Interrupt::dynamic`
  - State checkpointing with `MemoryCheckpointer` for persistence and replay
  - Full `GraphInvocationContext` implementation for proper agent execution
- **adk-browser crate**: Browser automation with 46 WebDriver tools
  - `BrowserSession` wrapping thirtyfour WebDriver
  - Navigation, element interaction, screenshots, cookies, frames
  - Window/tab management, drag-and-drop, file uploads
  - PDF printing, JavaScript execution
- **adk-eval crate**: Agent evaluation framework
  - `TrajectoryEvaluator` for comparing tool call sequences
  - `SemanticEvaluator` for response similarity scoring
  - `RubricEvaluator` for LLM-based rubric assessment
  - Full `EvalInvocationContext` implementation for agent execution during evaluation
- 7 new graph examples:
  - `graph_agent` - Basic AgentNode usage
  - `graph_workflow` - Multi-agent pipeline (extractor → analyzer → formatter)
  - `graph_conditional` - Dynamic routing based on LLM decisions
  - `graph_react` - ReAct pattern with cyclic tool usage
  - `graph_supervisor` - Supervisor pattern with worker agents
  - `graph_hitl` - Human-in-the-loop interrupts
  - `graph_checkpoint` - State persistence and replay
- `eval_agent` example demonstrating evaluation framework
- Official documentation for graph agents, browser tools, and evaluation

### Fixed
- **AgentNode execution**: Now properly executes wrapped agents instead of returning empty events
- **after_agent_callback**: Now correctly stores and invokes the callback
- Clippy warning in adk-browser for field assignment style
- Documentation warnings for unresolved links in adk-model

### Changed
- All graph examples now use real LLM integration via `AgentNode` (no mock/placeholder code)
- Updated all crate versions to 0.1.4 with standardized workspace inheritance
- Improved documentation with complete AgentNode usage examples

## [0.1.3] - 2025-12-08

### Added
- **adk-realtime crate**: New crate for real-time voice-enabled AI agents
  - `RealtimeAgent` implementing `adk_core::Agent` trait with full callback/tool/instruction support
  - OpenAI Realtime API support (`gpt-4o-realtime-preview-2024-12-17`, `gpt-realtime`)
  - Gemini Live API support (`gemini-2.0-flash-live-preview-04-09`)
  - Bidirectional audio streaming (PCM16, G711 formats)
  - Server-side Voice Activity Detection (VAD)
  - Real-time tool calling during voice conversations
  - Multi-agent handoffs via `transfer_to_agent`
- 4 new realtime examples:
  - `realtime_basic` - Simple text-based realtime session
  - `realtime_vad` - Voice assistant with VAD
  - `realtime_tools` - Tool calling during voice conversations
  - `realtime_handoff` - Multi-agent routing system

### Changed
- Updated default Gemini model from `gemini-2.0-flash-exp` to `gemini-2.5-flash`
- Updated OpenAI model references to use `gpt-4.1` (latest)
- Updated Anthropic model references to use `claude-sonnet-4` (latest)
- Updated all documentation and examples with current model names

## [0.1.2] - 2025-12-07

### Added
- **OpenAI provider support**: Full integration with OpenAI's GPT models
  - `OpenAIClient` and `OpenAIConfig` for easy configuration
  - Streaming support with proper tool call accumulation
  - Compatible with GPT-4o, GPT-4o-mini, GPT-4-turbo, GPT-3.5-turbo
  - Feature flag: `adk-model = { features = ["openai"] }`
- **Anthropic provider support**: Full integration with Anthropic's Claude models
  - `AnthropicClient` and `AnthropicConfig` using the `claudius` crate
  - Streaming support with tool call support
  - Compatible with Claude Opus 4.5, Claude Sonnet 4.5, Claude 3.5 Sonnet, Claude 3 Opus
  - Feature flag: `adk-model = { features = ["anthropic"] }`
- New feature flag `all-providers` to enable Gemini, OpenAI, and Anthropic together
- 16 new OpenAI examples covering all ADK features:
  - `openai_basic`, `openai_tools`, `openai_workflow`, `openai_template`
  - `openai_parallel`, `openai_loop`, `openai_agent_tool`, `openai_structured`
  - `openai_artifacts`, `openai_mcp`, `openai_a2a`, `openai_server`, `openai_web`
  - `openai_sequential_code`, `openai_research_paper`, `debug_openai_error`
- 2 new Anthropic examples: `anthropic_basic`, `anthropic_tools`
- `MutableSession` struct in `adk-runner` for shared mutable session state
- `InvocationContext::with_mutable_session()` constructor for sharing sessions across contexts
- `InvocationContext::mutable_session()` accessor for the underlying mutable session
- New tests for `MutableSession` state propagation behavior
- New example: `structured_output` demonstrating JSON schema output constraints

### Fixed
- **Critical bug**: SequentialAgent now correctly propagates state between agents via `output_key`
  - Root cause: InvocationContext held an immutable snapshot of session state
  - Solution: Implemented `MutableSession` wrapper (matching ADK-Go's pattern) that allows
    state changes from `state_delta` to be immediately visible to downstream agents
  - This fix enables proper use of `output_key` in sequential/parallel agent workflows
- OpenAI 400 Bad Request errors caused by empty assistant messages (added placeholder content)
- OpenAI streaming empty Content accumulation issue

### Changed
- `InvocationContext` now internally uses `MutableSession` instead of immutable `SessionAdapter`
- Runner applies `state_delta` from events to the mutable session immediately after each event
- Agent transfers now share the same `MutableSession` to preserve state
- Updated README documentation with multi-provider examples

## [0.1.1] - 2025-11-30

### Fixed
- Clippy `redundant_pattern_matching` warning in test files
- Doc test for `ScopedArtifacts` using incorrect `Part` constructor
- Code formatting issues caught by `cargo fmt`
- Multiple doc tests in `adk-rust/src/lib.rs` with incorrect API usage:
  - `LoopAgent::new` signature (takes `Vec<Arc<dyn Agent>>`, use `.with_max_iterations()`)
  - `FunctionTool::new` handler signature (takes `Arc<dyn ToolContext>, Value`)
  - `McpToolset` API (uses `rmcp` crate, `McpToolset::new(client)`)
  - `SessionService::create` takes `CreateRequest` struct
  - Callback methods renamed to `after_model_callback`, `before_tool_callback`
  - `ArtifactService` trait and request/response structs
  - Server API uses `create_app_with_a2a`, `ServerConfig`, `AgentLoader`
  - Telemetry uses `init_telemetry` and `init_with_otlp` functions
- All clippy warnings for `--all-targets --all-features`:
  - Unused imports in test files and examples
  - Unused variables in example code (prefixed with underscore)
  - `unnecessary_literal_unwrap` in test assertions

### Changed
- Integration tests requiring `GEMINI_API_KEY` now marked with `#[ignore]` for CI compatibility

## [0.1.0] - 2025-11-30

Initial release - Published to crates.io.

### Features
- Complete Rust implementation of Google's ADK
- Core traits: Agent, Llm, Tool, Toolset, SessionService
- Agent types: LlmAgent, CustomAgent, SequentialAgent, ParallelAgent, LoopAgent, ConditionalAgent
- Gemini model integration with streaming support
- MCP (Model Context Protocol) integration via rmcp SDK
- Session management (in-memory and database backends)
- Artifact storage (in-memory and database backends)
- Memory system with semantic search
- Runner for agent execution with context management
- REST API server with Axum
- A2A (Agent-to-Agent) protocol support
- CLI with console mode and server mode
- Security configuration (CORS, timeouts, request limits)
- OpenTelemetry integration for observability

### Crates
- `adk-core` - Core traits and types
- `adk-agent` - Agent implementations
- `adk-model` - LLM integrations (Gemini)
- `adk-tool` - Tool system (FunctionTool, MCP, Google Search)
- `adk-session` - Session management
- `adk-artifact` - Binary artifact storage
- `adk-memory` - Semantic memory
- `adk-runner` - Agent execution runtime
- `adk-server` - HTTP server and A2A protocol
- `adk-cli` - Command-line launcher
- `adk-telemetry` - OpenTelemetry integration
- `adk-rust` - Umbrella crate

### Requirements
- Rust 1.75+
- Tokio async runtime
- Google API key for Gemini

[Unreleased]: https://github.com/zavora-ai/adk-rust/compare/v2.2.0...HEAD
[2.2.0]: https://github.com/zavora-ai/adk-rust/compare/v2.1.0...v2.2.0
[2.1.0]: https://github.com/zavora-ai/adk-rust/compare/v2.0.0...v2.1.0
[2.0.0]: https://github.com/zavora-ai/adk-rust/compare/v1.0.0...v2.0.0
[0.3.0]: https://github.com/zavora-ai/adk-rust/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/zavora-ai/adk-rust/compare/v0.1.9...v0.2.0
[0.1.9]: https://github.com/zavora-ai/adk-rust/compare/v0.1.7...v0.1.9
[0.1.7]: https://github.com/zavora-ai/adk-rust/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/zavora-ai/adk-rust/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/zavora-ai/adk-rust/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/zavora-ai/adk-rust/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/zavora-ai/adk-rust/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/zavora-ai/adk-rust/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/zavora-ai/adk-rust/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/zavora-ai/adk-rust/releases/tag/v0.1.0
