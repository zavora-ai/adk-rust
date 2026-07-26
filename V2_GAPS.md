# ADK-Rust v2 Gaps

## Document status

**Review date:** 2026-07-22
**Repository snapshot:** branch `feat/cooperative-cancellation`, commit `0c653af6ca76328a3f540018ba8ad4892bdedd26` (`2026-07-21T07:50:46+03:00`, `feat(acp): add full ACP v1 protocol support`)
**Landscape cutoff for the wider review:** 2026-07-21
**Purpose:** detailed technical record of shortcomings verified in the repository snapshot above
**Implementation changes by this review:** none; this report is the only file created by this review

This is a source and execution-evidence report, not a release announcement and not a complete audit of every crate. Each material statement below is tied to current repository source, a locally executed probe, or an explicit mismatch between source and repository documentation. Areas that were not completed are listed under [Pending review areas](#pending-review-areas) rather than presented as findings.

## Release-baseline limitation

The workspace declares version `2.0.0` in `Cargo.toml`, `CHANGELOG.md` contains a `2.0.0` entry dated 2026-07-16, and `README.md` calls v2 current. However, the inspected local repository has no `v2.0.0` tag. There is therefore no authoritative local Git object that identifies the exact source published as v2.0.0. Commit `2a4a85bec8c5b41a59b94d580a5c9e1684d8e2b3` is retained only as a provisional date-based comparison point; it is not treated as a release tag.

Unless a finding explicitly says otherwise, its release attribution is **current HEAD only**. A current-HEAD finding must not be read as proof that the same behavior exists in a particular crates.io package or released source archive.

## Evidence classification

- **Probe-confirmed:** behavior was reproduced by an executable local probe. Source was also inspected to identify the mechanism.
- **Source-confirmed:** the behavior follows directly from the inspected implementation. No claim is made about an external service beyond the repository boundary.
- **Documentation mismatch:** repository documentation promises or describes behavior that the inspected implementation does not provide.
- **Qualified absence:** no implementation was found within a stated search scope. This does not assert repository-wide or ecosystem-wide absence outside that scope.
- **Pending review:** insufficient inspection has been completed; the topic is intentionally not reported as a finding.

Unless a narrower scope is stated, each test-coverage-gap claim is limited to tests in the cited crate or subsystem plus repository-wide searches for the relevant symbols in this checkout. It does not describe downstream or external test suites.

A prior attempt to obtain independent source confirmation from reviewer agents was blocked because those agents had no filesystem or search tools. Their output is not used as evidence in this document.

## Severity definitions

- **Critical:** directly enables broad unauthorized control or disclosure under ordinary deployment assumptions, or predictably causes unrecoverable system-wide corruption.
- **High:** can violate isolation or authorization, execute an operation more than once, lose durable control state, report failed work as successful, or defeat a major runtime contract.
- **Medium:** materially reduces correctness, reliability, operability, or compatibility but generally needs a narrower trigger or has a bounded workaround.
- **Low:** localized API or documentation defect with limited operational impact.

Severity describes plausible impact of the repository behavior, not exploitability in every deployment. Conditional impacts are identified explicitly.

## Executive technical conclusion

The snapshot contains substantial implemented functionality, but several advertised runtime boundaries are not yet dependable enough to treat as uniform contracts. The highest-priority problems are:

1. execution paths that replay or execute work more than once;
2. context wrappers and adapters that remove cancellation, secrets, identity, or authorization data;
3. background and managed execution surfaces whose durability or execution is process-local or simulated;
4. unauthenticated UI bridge mutation routes when server authentication is configured;
5. coding and sandbox surfaces whose isolation is weaker than their public description;
6. realtime tool paths that are disconnected from the standard control pipeline; and
7. adapters that trust security-relevant remote objects without local binding checks.

The report does **not** conclude that every ADK-Rust feature is unsafe or unusable. It identifies concrete paths where the implementation does not meet its own local contract or where safe behavior depends on application wiring that the public surface does not enforce.

---

## 1. Execution correctness and runtime identity

### EX-01 — `ParallelAgent` starts sub-agents concurrently but drains their lazy streams serially

**Severity:** High
**Affected capability:** parallel workflow execution and latency
**Evidence:** Probe-confirmed
**Affected code:** `adk-agent/src/workflow/parallel_agent.rs` — `ParallelAgent::run`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`ParallelAgent::run` inserts futures that call `agent.run(ctx).await` into `FuturesUnordered`. An ADK agent run returns a lazy `EventStream`; obtaining that stream does not necessarily execute its contents. After one run future resolves, `ParallelAgent` enters an inner loop and drains that stream to completion before polling `FuturesUnordered` for another resolved stream.

#### Failure mechanism

Concurrency covers stream construction, not stream consumption. If two sub-agents each return a stream whose work happens while it is polled, only one stream is polled at a time. The second stream is dormant until the first completes. This is especially visible for LLM streams, tool streams, and custom agents implemented with `async_stream`.

#### Impact

Nominally parallel branches can have approximately additive latency and suffer head-of-line blocking. A slow or stalled first stream delays all other branch events. This also changes timing assumptions for agents coordinating through `SharedState`.

#### Evidence

A local probe used two lazy streams that each waited 300 ms. Total elapsed time was `604 ms`, rather than approximately 300 ms. Source inspection identifies the nested drain loop as the cause.

#### Test coverage gap

Existing tests do not assert overlap of lazy stream execution or interleaving of events from multiple sub-agents.

#### Required correction

Poll all returned streams concurrently. One approach is to map each sub-agent stream into a tagged stream and merge them with `SelectAll` or equivalent bounded fan-in. Preserve deterministic error policy explicitly rather than achieving ordering by serial drain.

#### Regression tests

Add a timing-independent barrier test proving both streams are polled before either can complete, an event-interleaving test, and a failure test showing one branch error does not leave other streams unpolled.

### EX-02 — Workflow context wrappers remove cancellation, secret access, and shared state

**Severity:** High
**Affected capability:** cooperative cancellation, secret-backed tools, authenticated metadata, and workflow composition
**Evidence:** Probe-confirmed
**Affected code:** `adk-agent/src/workflow/loop_agent.rs` — `HistoryTrackingContext`; `adk-agent/src/workflow/skill_context.rs` — `UserContentOverrideContext`; `adk-agent/src/workflow/shared_state_context.rs` — `SharedStateContext`; `adk-core/src/context.rs` — `InvocationContext` defaults
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

The wrappers reimplement `InvocationContext` and delegate only selected methods. Methods not explicitly delegated fall back to trait defaults: `is_cancelled()` becomes `false`, `get_secret()` becomes `Ok(None)`, `shared_state()` becomes `None`, scopes become empty where not delegated, and request metadata becomes empty where not delegated.

`HistoryTrackingContext` delegates scopes and request metadata but not cancellation, secrets, or shared state. `UserContentOverrideContext` delegates none of those optional capabilities. `SharedStateContext` injects shared state and delegates scopes and metadata, but still does not delegate cancellation or secrets.

#### Failure mechanism

Rust trait default methods make a partial wrapper compile successfully. Adding a new context capability therefore does not force every wrapper to forward it. Composition silently changes behavior according to which wrapper happens to be outermost.

#### Impact

A direct agent can stop when interrupted and retrieve a configured secret, while the same agent nested in `LoopAgent` or `SequentialAgent` can continue running and observe no secret provider. Parallel shared-state mode keeps shared state but removes cancellation and secrets. Security and lifecycle behavior therefore changes with workflow topology.

#### Evidence

The local probe reported:

- direct context: cancellation `true`, secret present, shared state present;
- loop/sequential context: cancellation `false`, secret absent, shared state absent;
- parallel shared context: cancellation `false`, secret absent, shared state present.

#### Test coverage gap

There is no context-capability conformance test that runs the same sentinel context through every workflow wrapper and compares all extension methods.

#### Required correction

Introduce a reusable delegating context adapter that forwards every capability by default and overrides only the intended field. Alternatively, make capabilities an owned inner object rather than default trait methods. Audit every context wrapper when new methods are added.

#### Regression tests

Create a sentinel context with non-default values for artifacts, memory, shared state, scopes, metadata, cancellation, and secrets. Verify direct, skill, loop, sequential, and parallel paths preserve all values except explicitly documented overrides.

### EX-03 — `LlmAgent` discards provider terminal error fields

**Severity:** High
**Affected capability:** model failure propagation and truthful event status
**Evidence:** Probe-confirmed
**Affected code:** `adk-core/src/model.rs` — `LlmResponse`; `adk-agent/src/llm_agent.rs` — `LlmAgent::run`; `adk-model/src/anthropic/convert.rs` — `from_stream_error`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`LlmResponse` carries `interrupted`, `error_code`, and `error_message`. Provider conversion code can return a successful stream item containing those fields; Anthropic's stream-error converter is one concrete source. `LlmAgent::run` copies content, partial/turn status, finish reason, usage, provider metadata, and interaction ID into events, but it does not copy or turn the three terminal error fields into an `Err`.

#### Failure mechanism

The agent distinguishes transport-level `Err` items from response-level error envelopes, but only handles the former. A terminal provider error encoded as `Ok(LlmResponse { error_code: ... })` passes through accumulation and can end as an apparently successful, often empty turn.

#### Impact

Callers, persistence backends, retry logic, and user interfaces can record a failed provider turn as success. Retry and alerting policy cannot react to the lost code. Empty output may be misdiagnosed as model behavior rather than provider failure.

#### Evidence

A local mock-provider probe emitted terminal error fields. The agent stream produced `stream_errors=[]`, and emitted event error fields were absent.

#### Test coverage gap

No cross-provider contract test requires response-level errors to survive the `Llm` → `LlmAgent` → `EventStream` boundary.

#### Required correction

Define one canonical rule: either provider adapters must emit `Err(AdkError)` for terminal errors, or `LlmAgent` must detect `error_code`, preserve the fields on an event, and terminate with a structured error. Apply the same rule to callback-provided responses.

#### Regression tests

Cover streaming and non-streaming modes, a terminal error with no content, an error after partial content, and `interrupted=true`. Assert persistence and telemetry receive an unambiguous failed outcome.

### EX-04 — Runner cancellation is keyed only by raw session ID and is unsafe for concurrent runs

**Severity:** High
**Affected capability:** run isolation and interruption
**Evidence:** Source-confirmed
**Affected code:** `adk-runner/src/runner.rs` — `Runner::run`, `Runner::interrupt`, `Runner::active_session_ids`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`Runner` stores active cancellation tokens in `HashMap<String, CancellationToken>` keyed by `session_id` only. The app and user dimensions used to retrieve the session are not part of the key. Starting another run with the same raw session ID replaces the prior token. Each stream's drop guard later removes the key unconditionally.

#### Failure mechanism

Two identities such as `(app-a, user-a, shared-id)` and `(app-b, user-b, shared-id)` collide inside one Runner. Two concurrent runs for one session also collide: the second insert hides the first token, and completion of the first can remove the second run's registration.

#### Impact

`interrupt("shared-id")` can target the wrong active execution, fail to reach an older execution, or report no active run while one is still running. This is an isolation defect when a Runner is shared across users or applications and a correctness defect under same-session concurrency.

#### Test coverage gap

No test starts colliding IDs across users/apps or overlaps two runs with the same identity while exercising cleanup order.

#### Required correction

Key active runs by full `AdkIdentity` plus a unique invocation/run ID. Define whether interrupt-by-session cancels all runs or requires an exact run ID. Make cleanup conditional on removing the same token/run entry that was inserted.

#### Regression tests

Cover duplicate raw IDs across identities, two overlapping runs for one identity, reverse completion order, targeted cancellation by run ID, and session-wide cancellation semantics.

### EX-05 — Runner persistence bypasses the full session identity API

**Severity:** High
**Affected capability:** tenant-safe session persistence
**Evidence:** Source-confirmed
**Affected code:** `adk-runner/src/runner.rs` — event append call sites in `Runner::run`; `adk-session/src/service.rs` — `SessionService::append_event`, `append_event_for_identity`, `AppendEventRequest`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

The Runner retrieves a session with `(app_name, user_id, session_id)` but persists user and agent events through `session_service.append_event(ctx.session_id(), event)`. The session trait now exposes `append_event_for_identity`, explicitly described as the preferred unambiguous API, but the Runner does not call it.

#### Failure mechanism

The full identity is available in the context and then discarded at the write boundary. A backend whose natural key is composite cannot use its identity-aware override because the Runner chooses the legacy raw-ID method directly.

#### Impact

Impact is backend-dependent. If session IDs are not globally unique, events can be appended to the wrong app/user session or rejected ambiguously. Even backends that currently impose global IDs cannot enforce tenant binding at this call site.

#### Test coverage gap

There is no Runner integration test using a backend that deliberately creates the same session ID under two identities and rejects raw-ID appends.

#### Required correction

Construct `AdkIdentity` once and use `append_event_for_identity` for every persistence write. Consider deprecating the raw append method or making the identity-aware method required for composite backends.

#### Regression tests

Use a strict composite-key fake backend and assert every user, model, plugin, transfer, and tool event is appended with the exact identity.

### EX-06 — Static tool approvals are keyed by tool name rather than exact call identity

**Severity:** High
**Affected capability:** human authorization for side-effecting tools
**Evidence:** Source-confirmed
**Affected code:** `adk-core/src/context.rs` — `RunConfig::tool_confirmation_decisions`, `ToolConfirmationRequest`; `adk-agent/src/llm_agent.rs` — tool confirmation pre-check in `LlmAgent::run`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

The confirmation request includes `function_call_id` and arguments. Live decisions from a `ToolConfirmationHandler` are tracked by call ID. In contrast, static decisions supplied through `RunConfig::tool_confirmation_decisions` are looked up by `fc_name`.

#### Failure mechanism

An approval entry for `delete_file` authorizes every matching call evaluated against that map, independent of path, arguments, provider call ID, or a digest of the request. Multiple calls with the same tool name cannot receive distinct static decisions.

#### Impact

A decision intended for one exact action can be replayed onto a materially different invocation. This weakens the authorization boundary in resumed/web-driven confirmation flows that use the static map rather than the live handler.

#### Test coverage gap

No test presents two same-name calls with different arguments and proves that approving one leaves the other pending or denied.

#### Required correction

Key decisions by stable function-call ID and bind them to a canonical digest of tool name plus arguments. Reject missing or mismatched IDs. If name-wide policy is desired, expose it as a separately named, explicit policy.

#### Regression tests

Cover two same-name calls, changed arguments under a reused ID, reordered calls, duplicate provider IDs, and one-shot consumption of an approval.

### EX-07 — Tool progress uses an unbounded in-process channel

**Severity:** Medium
**Affected capability:** resource bounds during long-running tools
**Evidence:** Source-confirmed
**Affected code:** `adk-agent/src/llm_agent.rs` — `AgentToolContext::with_progress`, `AgentToolContext::emit_progress`, progress channel creation in `LlmAgent::run`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

Each tool batch creates `tokio::sync::mpsc::unbounded_channel::<Event>()`. Every call to `emit_progress` allocates and sends an event without backpressure or an aggregate byte/event limit.

#### Failure mechanism

A verbose tool can produce progress faster than the agent stream or client consumes it. The sender never waits, so queued events retain strings and metadata until drained or the process exhausts memory.

#### Impact

Large compiler logs, shell output, or a faulty tool can cause unbounded memory growth. The risk is amplified when the downstream SSE client is slow. This is separate from any final-output truncation a tool applies after collecting output.

#### Test coverage gap

There is no stress test for a stalled consumer, queue depth, dropped/coalesced progress, or cancellation under sustained output.

#### Required correction

Use a bounded channel with explicit backpressure or a documented lossy/coalescing policy. Enforce per-call byte and event budgets and propagate cancellation when the receiver closes.

#### Regression tests

Flood progress with a non-reading receiver and assert bounded memory/queue size, deterministic truncation markers, and prompt producer termination after cancellation.

---

## 2. Context caching, memory, and provider contracts

### MC-01 — Automatic context cache state is Runner-global and built from incomplete inputs

**Severity:** High
**Affected capability:** prompt correctness and provider context caching
**Evidence:** Source-confirmed
**Affected code:** `adk-runner/src/cache.rs` — `CacheManager`; `adk-runner/src/runner.rs` — context-cache lifecycle in `Runner::run`; `adk-agent/src/llm_agent.rs` — cached-content propagation
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

A Runner owns one `CacheManager` with one `active_cache_name` and invocation counter. That cache is reused across sessions and whichever sub-agent `find_agent_to_run` selects. Cache creation uses `agent_to_run.description()` as the system instruction and an empty tool map. The real static/dynamic global instruction, agent instruction, selected skill content, history, and resolved tools are constructed later inside `LlmAgent`.

#### Failure mechanism

Cache identity is neither per agent nor keyed by the actual cacheable request material. A cache made for one selected agent can be attached to a later request for another agent. Refresh occurs by Runner-wide invocation count rather than content change. The cache therefore does not reliably represent the request to which its name is attached.

#### Impact

Provider behavior can include stale or wrong cached instructions, cache misses disguised as reuse, or provider-side validation failures. The current cache input contains descriptions rather than user history, so this finding does not claim cross-user prompt disclosure; it does establish cross-session/agent correctness coupling.

#### Test coverage gap

Tests cover `CacheManager` counters in isolation, not multiple users, agent transfers, dynamic instructions, toolset changes, or the actual `create_cache` arguments.

#### Required correction

Compute a canonical cache key from the exact provider-normalized system/tool material, scope entries by model/provider and agent, and maintain a bounded map rather than one global name. Invalidate on any material change. Do not substitute description for instruction.

#### Regression tests

Capture cache creation requests for two agents and two dynamic instruction values; assert distinct cache entries, correct tools, no cross-session name reuse when content differs, and reuse only for byte-equivalent normalized material.

### MC-02 — Project-aware memory defaults silently fall back to global operations

**Severity:** High
**Affected capability:** project isolation in memory backends
**Evidence:** Source-confirmed
**Affected code:** `adk-memory/src/service.rs` — default project methods on `MemoryService`; `adk-core/src/context.rs` — default project methods on `Memory`; `adk-memory/src/adapter.rs` — `MemoryServiceAdapter`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

Default implementations of `add_session_to_project`, `add_entry_to_project`, and `delete_entries_in_project` discard `project_id` and call their global equivalents. Core `Memory::search_in_project` and `add_to_project` likewise discard the project and delegate globally. `MemoryServiceAdapter::with_project_id` relies on those project methods.

#### Failure mechanism

A backend can compile as project-aware without overriding every project method. Calls carrying a project identifier then succeed while operating in global scope. The type system and return value provide no indication that isolation was not implemented.

#### Impact

For a custom or incomplete backend, data intended for one project can become globally visible to the same app/user, and project-scoped deletion can affect global entries. Inspected built-in backends may override these methods; this finding is about the unsafe trait fallback and does not assert that every backend collapses scope.

#### Test coverage gap

There is no conformance suite that requires every advertised project-aware backend to preserve project boundaries across add, search, and delete.

#### Required correction

Make project methods return `not implemented` by default, or split project-aware behavior into a separate required trait. Expose capability discovery so callers cannot mistake fallback behavior for isolation.

#### Regression tests

Run a shared backend conformance suite with two project IDs plus global entries. Verify visibility and deletion boundaries and assert incomplete implementations fail explicitly.

### PR-01 — DeepSeek advertises structured JSON output but hardcodes `response_format` to `None`

**Severity:** Medium
**Affected capability:** native structured output
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-model/src/deepseek/client.rs` — `DeepSeekClient::build_request`; `adk-model/src/deepseek/convert.rs` — `ChatCompletionRequest`; `adk-model/src/deepseek/mod.rs` — provider feature documentation
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

The request type contains `response_format`, and the provider module advertises “JSON Output: Structured JSON responses via `response_format`.” `DeepSeekClient::build_request` reads temperature, top-p, token limits, tools, thinking, and reasoning effort, but always sets `response_format: None`, even when `GenerateContentConfig.response_schema` is present.

#### Failure mechanism

The unified schema setting is dropped at the provider adapter. `LlmAgent` still injects a textual schema instruction and validates the eventual text, so callers may see retries rather than complete loss; native provider enforcement is nevertheless not requested.

#### Impact

Structured output is less reliable and can cost extra turns. Behavior differs from Gemini, OpenAI-compatible, OpenRouter, and Gemini Interactions adapters that explicitly map response schemas.

#### Test coverage gap

There is no provider request-serialization test asserting a DeepSeek response format when `response_schema` is supplied.

#### Required correction

Map supported schema intent to the exact DeepSeek response-format contract, validate model compatibility, and return an explicit unsupported-capability error where native schema mode is unavailable.

#### Regression tests

Serialize chat and reasoning-model requests with and without schemas and assert the expected wire field, plus an end-to-end fake-server test for invalid structured output handling.

### PR-02 — Provider converters silently drop or downgrade some content parts

**Severity:** Medium
**Affected capability:** multimodal fidelity and provider portability
**Evidence:** Source-confirmed
**Affected code:** `adk-model/src/bedrock/convert.rs` — `adk_parts_to_bedrock`; `adk-model/src/gemini/client.rs` — `GeminiModel::generate_content_internal`; `adk-model/src/anthropic/convert.rs` — `content_to_message`; `adk-model/src/openrouter/convert_chat.rs` — `adk_contents_to_chat_messages`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

Conversion policy is inconsistent. Bedrock silently returns `None` for unsupported inline MIME types, some file references, embedded blobs, and Gemini-specific server-tool parts. Gemini generateContent turns user `FileData` into attachment-description text rather than a native file part. Anthropic also textualizes unsupported attachments, while OpenRouter has broader file-part mappings.

#### Failure mechanism

The common `Content` type can express more than a given transport. Adapters choose unrelated fallback policies—drop, textualize, or encode—without a common capability result or warning event visible to the caller.

#### Impact

A request can reach a provider without material the caller supplied. The model may answer as if it saw a document or media object when it only saw a URI description, or no part at all. This is a portability defect, not a claim that every provider supports every MIME type.

#### Test coverage gap

Provider tests focus on supported examples. There is no matrix requiring every `Part` variant to be accepted, explicitly downgraded, or rejected with a structured error.

#### Required correction

Add provider capability negotiation and a normalized conversion outcome. Unsupported material should fail before network dispatch unless the caller explicitly opts into a documented textual fallback. Emit diagnostics listing every transformed or omitted part.

#### Regression tests

Build a cross-provider table covering inline/file image, audio, video, PDF, text, arbitrary binary, embedded resources, and server-tool parts. Assert no silent omission.

---

## 3. Graph execution and checkpointing

### GR-01 — Checkpoints save already-executed nodes as pending, so resume replays them

**Severity:** High
**Affected capability:** durable resume and side-effect safety
**Evidence:** Probe-confirmed
**Affected code:** `adk-graph/src/executor.rs` — `PregelExecutor::run`, `try_resume_from_checkpoint`, `save_checkpoint`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

After `execute_super_step`, `run` calls `save_checkpoint` before calculating and assigning the next nodes. `save_checkpoint` serializes `self.pending_nodes`, which still contains the nodes just executed. Resume restores that vector and executes those nodes again.

#### Failure mechanism

The checkpoint represents the scheduler state from before the completed super-step but the data state from after it. State and control position are inconsistent. Automatic loading of the latest checkpoint has the same issue as explicit `resume_from`.

#### Impact

Completed LLM calls, HTTP calls, database writes, desktop actions, payments, or other non-idempotent nodes can run twice after restart/resume. State reducers can also apply updates twice. External idempotency may reduce effects for a specific integration, but it is not a graph-level guarantee.

#### Evidence

A local probe invoked a one-node graph twice with the same checkpoint thread. It reported `first_count=1 second_count=2`, proving the completed node ran again.

#### Test coverage gap

Current graph tests do not assert that a completed side-effect node is skipped on a second invocation using the same thread.

#### Required correction

Compute the next scheduler state first and checkpoint state plus the **next** pending nodes atomically. Define terminal checkpoints explicitly. Include deferred/fan-in scheduler state if required for exact recovery.

#### Regression tests

Use a non-idempotent counter node, multi-step graph, parallel fan-out, deferred join, interrupt-before/after, and terminal checkpoint. Resume at every boundary and assert each completed node executes exactly once.

### GR-02 — Streaming graph execution never saves checkpoints

**Severity:** High
**Affected capability:** durable streaming, crash recovery, and streamed interrupts
**Evidence:** Source-confirmed
**Affected code:** `adk-graph/src/executor.rs` — `PregelExecutor::run_stream`, `save_checkpoint`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`save_checkpoint` is called only by non-streaming `run`. `run_stream` has no call to it in Values, Updates, Debug, Custom, or Messages mode. On a streamed interrupt, it emits `StreamEvent::interrupted` and returns without persisting the interrupt state.

#### Failure mechanism

The streaming implementation duplicates the scheduling loop but omits the persistence steps present in `run`.

#### Impact

A graph advertised as checkpointed is not durable when consumed through the streaming API. A crash restarts from an older checkpoint or from the beginning. Human-in-the-loop consumers cannot rely on a streamed interrupt having a resumable checkpoint.

#### Test coverage gap

There is no test comparing final/latest checkpoint state between `invoke` and every stream mode, or resuming after dropping a stream mid-run.

#### Required correction

Unify streaming and non-streaming execution around one state-transition engine. Persistence should occur once per committed transition independent of output mode, and streamed interrupt events should carry the saved checkpoint ID.

#### Regression tests

For every stream mode, drop after each step and resume; verify no replay, correct next nodes, and a durable interrupt checkpoint.

### GR-03 — Messages stream mode executes each node twice

**Severity:** High
**Affected capability:** graph message streaming and exactly-once execution
**Evidence:** Source-confirmed
**Affected code:** `adk-graph/src/executor.rs` — Messages branch of `PregelExecutor::run_stream`; `adk-graph/src/node.rs` — `Node::execute_stream`, `AgentNode::execute_stream`, `AgentNode::execute`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

Messages mode first drains `node.execute_stream(&ctx)`. It then calls `node.execute(&ctx)` to obtain state updates. The default `Node::execute_stream` already calls `execute`. `AgentNode::execute_stream` independently invokes the wrapped agent, and `AgentNode::execute` invokes it again.

#### Failure mechanism

The stream contract returns events but not the final `NodeOutput`, so the executor reruns the node to reconstruct updates instead of collecting one execution's result.

#### Impact

Every Messages-mode node can perform side effects twice, incur two model calls, produce inconsistent output, and charge twice. The second execution supplies state updates that may not correspond to the first execution's streamed messages.

#### Test coverage gap

There is no invocation-count assertion for Messages mode or consistency test tying streamed messages to committed state.

#### Required correction

Change the streaming node contract to yield events and return/emit one final output, or run `execute` once and stream events from that execution. Never invoke a node solely to recover state already produced by another invocation.

#### Regression tests

Count calls for default `FunctionNode` and `AgentNode`, verify one call per node, and assert state is derived from the same execution whose messages were emitted.

### GR-04 — `AgentNode` replaces runtime identity and services with a synthetic context

**Severity:** High
**Affected capability:** agents embedded in graphs
**Evidence:** Source-confirmed
**Affected code:** `adk-graph/src/node.rs` — `AgentNode`, `GraphInvocationContext`, `GraphSession`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`AgentNode` creates a new `GraphInvocationContext` for each run. It hardcodes `user_id="graph_user"`, `app_name="graph_app"`, and branch `main`; creates a fresh in-memory `GraphSession`; uses default `RunConfig`; and returns no artifacts or memory. Optional invocation methods fall back to empty scopes/metadata, no secrets, no shared state, and no cancellation.

#### Failure mechanism

The graph node API receives `NodeContext`, not the parent ADK `InvocationContext`, so the adapter fabricates the minimum context needed to call an agent. No bridge carries authenticated/runtime capabilities into the graph.

#### Impact

An agent behaves differently inside a graph: identity-dependent tools see synthetic principals, session history/state is detached, secret and memory access disappear, scope checks cannot use the caller's grants, and Runner interruption is not visible.

#### Test coverage gap

No integration test compares one agent run directly and through `AgentNode` with a capability-rich context.

#### Required correction

Allow graph execution to carry a parent invocation capability bundle and derive a child context while preserving identity, services, request context, cancellation, and session semantics. Make synthetic standalone execution an explicit mode.

#### Regression tests

Pass sentinel identity, scopes, metadata, secret service, memory, artifacts, cancellation, shared state, and history through an `AgentNode`; assert exact preservation and intentional branch/session derivation.

### GR-05 — `TimeTravelHandle::replay` lists checkpoints instead of replaying execution

**Severity:** Medium
**Affected capability:** graph time travel and reproducibility
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-graph/src/time_travel.rs` — `TimeTravelHandle::replay`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

Rustdoc says `replay` “re-executes the graph” between steps. The implementation lists checkpoints, sorts and filters them, and returns stored `(step, state)` pairs. It never invokes a node or the compiled graph.

#### Failure mechanism

Historical state retrieval and deterministic re-execution are represented by one method name and description, but only retrieval is implemented.

#### Impact

Callers cannot use this API to reproduce decisions, regenerate intermediate events, or validate deterministic behavior. They may incorrectly treat stored snapshots as a fresh replay.

#### Test coverage gap

Tests assert returned checkpoint ranges, which reinforces snapshot listing but does not test node execution counts or regenerated events.

#### Required correction

Either rename/document the method as checkpoint-range retrieval or implement real replay in an isolated branch with explicit side-effect policy and deterministic inputs.

#### Regression tests

For real replay, use deterministic nodes and counters to prove re-execution and state equivalence; reject or sandbox side-effecting nodes. For retrieval-only behavior, update names and docs and test that no execution occurs.

### GR-06 — Functional `TaskContext::interrupt` cannot resume with a typed value

**Severity:** High
**Affected capability:** functional graph human-in-the-loop control
**Evidence:** Source-confirmed
**Affected code:** `adk-graph/src/functional/context.rs` — `TaskContext::interrupt`; `adk-graph/src/functional/error.rs` — `FunctionalError::InterruptTypeMismatch`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`interrupt<T>` emits an event, saves a checkpoint, records an `__interrupt__` task, and then always returns `InterruptTypeMismatch` with “workflow interrupted.” A repository-wide source search found no code outside this method that consumes a resume value or handles `__interrupt__` to return `T`.

#### Failure mechanism

The API signature and rustdoc describe suspension followed by typed resumption, but the implementation has no resume-value channel. The source comment explicitly labels the runtime behavior as future work.

#### Impact

Functional workflows cannot use this method as documented for approval or external input. Applications must interpret an error as control flow and build their own resume machinery, with no typed value delivered to the interrupted call.

#### Test coverage gap

There is no end-to-end test that interrupts a functional entrypoint, supplies a value, resumes, and observes that value at the call site.

#### Required correction

Represent interruption as a distinct suspended outcome, persist a stable continuation key, accept a typed resume payload, and make the generated entrypoint restore execution at the interrupt boundary without misclassifying normal suspension as a type mismatch.

#### Regression tests

Cover bool and structured payload resumption, wrong-type rejection, repeated resume, crash between interrupt and resume, and idempotent continuation.

### GR-07 — Several advertised action executors are validated placeholders

**Severity:** Medium
**Affected capability:** graph action nodes
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-graph/src/action/database.rs` — `execute_database`; `adk-graph/src/action/email.rs` — `execute_email`; `adk-graph/src/action/code.rs` — JavaScript/TypeScript execution; `adk-graph/src/action/transform.rs` — builtin transforms
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

Database actions validate configuration and return an error explaining that drivers are not integrated. Email monitor/send modes validate then return “not yet available.” JavaScript/TypeScript code execution is a placeholder. Builtin transforms log that operations are not implemented.

#### Failure mechanism

Feature flags and action types expose configuration surfaces before execution backends exist.

#### Impact

A workflow can deserialize, validate, and compile yet fail only when the node executes. This is operationally different from an unsupported feature rejected during graph construction.

#### Test coverage gap

Tests largely verify validation and placeholder messages rather than executable behavior or build-time capability rejection.

#### Required correction

Either implement each backend with explicit security/resource controls or remove/reserve the executable variants until ready. Graph compilation should reject unavailable action capabilities before a run starts.

#### Regression tests

Add real backend integration tests behind feature flags, plus negative compile/build tests proving unavailable executors fail during validation rather than mid-workflow.

---

## 4. Background, ambient, and managed runtimes

### BG-01 — Background runs report success without resolving or executing a workflow

**Severity:** High
**Affected capability:** asynchronous workflow execution, timeout, retry, and durability
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-server/src/background/mod.rs` — `BackgroundRunner::execute`, `run_with_timeout`, `RunStore`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

A submitted run stores `workflow_id` and input, but `BackgroundRunner::run_with_timeout` receives neither. It checks cancellation and immediately returns `RunOutcome::Completed` with an empty JSON object. `RunStore` is an in-memory `HashMap`. Documentation says retry re-enqueues from the last checkpoint, but no workflow checkpoint is loaded or stored.

#### Failure mechanism

The REST lifecycle and status model are implemented around a placeholder work future. Since the placeholder cannot return `Failed`, normal retry behavior is not exercised by actual workflow failures.

#### Impact

Clients receive a completed status for work that never ran. Process restart loses all run records. Timeout and retry configuration give a false signal of execution guarantees.

#### Test coverage gap

There is no registered-workflow integration test asserting input reaches a workflow, output is returned, failure retries, or restart resumes from a checkpoint.

#### Required correction

Require a workflow registry/executor dependency, resolve `workflow_id`, pass input and cancellation, persist run/control state in a durable store, and define checkpoint-aware retry semantics. Reject unknown workflows before queuing.

#### Regression tests

Exercise a real test workflow for success, failure, timeout, cancellation, retry from a known checkpoint, unknown ID, and process reconstruction from durable state.

### BG-02 — Cron `Queue` policy duplicates overdue occurrences and then loses queue monitoring

**Severity:** High
**Affected capability:** scheduled-run concurrency control
**Evidence:** Source-confirmed
**Affected code:** `adk-server/src/background/cron.rs` — `CronJobStore::get_due_jobs`, `start_cron_scheduler`, `trigger_run`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

Due calculation uses `last_execution.unwrap_or(created_at)`. Under `Queue`, when a run is active, every one-second scheduler poll appends a new UUID but does not advance `last_execution`. The same overdue occurrence is therefore enqueued repeatedly. When the active run completes, its monitor dequeues and starts one queued run, but it does not create a monitor for that new run.

#### Failure mechanism

Queued occurrences have no schedule-occurrence identity, and queue progression is coupled to a monitor spawned only by `trigger_run`. A run started from the queue bypasses that monitor setup.

#### Impact

One scheduled occurrence can become many queued runs. After the first queued run starts, `active_run_count` can remain nonzero after it finishes, so later queue items are not drained and `Skip`/`Queue` decisions remain stuck.

#### Test coverage gap

There is no test with a long-running job, multiple scheduler ticks, and more than one queued occurrence through completion.

#### Required correction

Track the exact scheduled timestamp and atomically claim it once. Advance scheduling state when an occurrence is queued, not only when execution starts. Route every queued run through the same monitored execution function and reconcile active counts from durable run state.

#### Regression tests

Use a controllable clock to prove one queue entry per occurrence, FIFO draining across three runs, active-count recovery after failure/cancel, and restart behavior.

### BG-03 — The documented cron detail endpoint is not mounted

**Severity:** Low
**Affected capability:** cron REST API completeness
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-server/src/background/mod.rs` — module endpoint list; `adk-server/src/background/cron.rs` — `cron_jobs_router_with_state`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

Module documentation advertises `GET /cron/{job_id}`. The router mounts only PATCH and DELETE on that path. A store `get` method exists, but no GET handler is attached.

#### Failure mechanism

The endpoint list and the Axum route table are maintained separately. The documented detail read was not connected to a handler in `cron_jobs_router_with_state`.

#### Impact

Clients cannot retrieve one job by ID as documented and must list all jobs and filter locally.

#### Test coverage gap

No router test asserts the documented method/path table.

#### Required correction

Add the detail handler or remove the endpoint claim. Generate endpoint documentation from the router where practical.

#### Regression tests

Assert GET returns 200 for an existing ID and 404 for a missing ID, and maintain a route-table conformance test.

### AM-01 — `AmbientAgent` is an event-loop shell, not a self-contained ambient runtime

**Severity:** High
**Affected capability:** background trigger execution and delivery
**Evidence:** Source-confirmed
**Affected code:** `adk-agent/src/ambient/agent.rs` — `AmbientAgent::start`, `TriggerHandler`; `adk-agent/src/ambient/event_source.rs` — `EventSource`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

Without a `TriggerHandler`, trigger events are only logged. The handler must create/drive a Runner and choose identity/session behavior itself. The event loop reads one event, awaits the handler, and fully drains its returned stream before polling the source again. Produced events are logged rather than delivered through an AmbientAgent output API.

#### Failure mechanism

Execution, persistence, identity selection, retry, acknowledgement, output delivery, and failure policy are delegated to application code. Serial drain creates head-of-line blocking.

#### Impact

Using `AmbientAgent::new(...).start()` alone does not run the wrapped agent. A slow trigger blocks later triggers. There are no built-in durable offsets, dead letters, per-user session mapping, or ownership guarantees in the inspected ambient module.

#### Test coverage gap

No end-to-end test proves default construction invokes an agent, survives restart, retries a failed event, or processes independent triggers concurrently under a bound.

#### Required correction

Make execution policy explicit in the type: require a handler/Runner binding at construction, define identity/session strategy, expose output delivery, and add bounded concurrency plus acknowledgement/retry hooks. Keep a clearly named low-level event-loop type if application-managed behavior is desired.

#### Regression tests

Cover missing handler rejection, two overlapping triggers, bounded queueing, handler failure retry/dead-letter behavior, output delivery, and stop/cancel semantics.

### AM-02 — `WebhookTrigger` has no trust boundary and its listener outlives the ambient consumer

**Severity:** High
**Affected capability:** externally triggered agents and lifecycle control
**Evidence:** Source-confirmed
**Affected code:** `adk-agent/src/ambient/webhook_trigger.rs` — `WebhookTrigger::subscribe`; `adk-agent/src/ambient/agent.rs` — `AmbientAgent::stop`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

The webhook binds `0.0.0.0:<port>`, accepts every POST on the configured path, and converts invalid JSON to a string event. There is no signature/authentication hook, replay protection, principal mapping, authorization decision, body-specific rate limit, or source timestamp check. The HTTP server is spawned without retaining its join/shutdown handle. Stopping `AmbientAgent` aborts only the consumer task.

#### Failure mechanism

The event source has no request context interface and no owned server lifecycle. When the receiver is dropped, handlers log that the listener is stopping, but they only return a response; no shutdown signal reaches `axum::serve`.

#### Impact

If the bound port is reachable, any caller can trigger application-defined agent work. After stop/drop, the listener can remain bound and continue accepting requests that cannot be delivered, preventing clean restart on the same port. This finding covers only `WebhookTrigger`; it does not claim anything about uninspected channel integrations.

#### Test coverage gap

No tests send unsigned/replayed requests, stop and rebind the port, or verify listener shutdown when the subscriber disappears.

#### Required correction

Require or expose an authentication/verifier interface, support provider signatures and replay windows, map a verified principal into the trigger event, and own a graceful-shutdown token plus join handle. Reject malformed content according to configured policy.

#### Regression tests

Cover missing/invalid signatures, replay, body limits, malformed JSON, stop-and-rebind, receiver drop, and principal propagation.

### MR-01 — Managed checkpoints and registries are process-local, not crash-durable

**Severity:** High
**Affected capability:** managed runtime durability and replay
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-managed/src/checkpoint.rs` — `CheckpointManager`; `adk-managed/src/default_runtime.rs` — `DefaultManagedAgentRuntime`, `ActiveSession`
**Release attribution:** current HEAD; released-v2 attribution unverified; crate is marked experimental

#### Current behavior

`CheckpointManager` stores `Vec<SessionEvent>` and `RunState` in memory. Agent and active-session registries are in-memory maps. Replay reads the same live manager. The module itself notes that persistent `SessionService` integration is a platform concern.

#### Failure mechanism

“Atomic” checkpointing is an assignment within one process. There is no transaction with the injected `SessionService`, no load of managed checkpoint state on startup, and no reconstruction of active sessions or registered agents after process loss.

#### Impact

A crash loses managed event replay, parked-tool state, sequence position, lifecycle status, and registry handles even if conversation events were written by the nested Runner. A new process cannot resume from the managed checkpoint advertised by the crate description.

#### Test coverage gap

Tests instantiate one manager/runtime and do not destroy/recreate it against a persistent backend.

#### Required correction

Define a durable managed-state store with transactional event/run-state writes, stable agent definitions, and startup reconstruction. Treat in-memory storage as an explicitly named test backend.

#### Regression tests

Persist state, drop the runtime, recreate it, resume sequence and parked tools, and verify no event loss/duplication under failure injection between event and state commits.

### MR-02 — Managed session environment is ignored and identity is hardcoded

**Severity:** High
**Affected capability:** tenant isolation and environment provisioning
**Evidence:** Source-confirmed
**Affected code:** `adk-managed/src/default_runtime.rs` — `DefaultManagedAgentRuntime::start_session`; `adk-managed/src/session_loop.rs` — `SessionLoop::build_runner`, `process_turn`
**Release attribution:** current HEAD; released-v2 attribution unverified; crate is marked experimental

#### Current behavior

`start_session` names its environment argument `_env` and never reads it. Every persisted session uses app `managed` and user `managed_user`; every Runner call repeats that identity. The public session handle contains only a generated session ID.

#### Failure mechanism

The managed API accepts environment configuration without applying it and has no caller identity parameter. The runtime substitutes constants to satisfy the underlying Runner/SessionService contract.

#### Impact

Environment-specific isolation/configuration is absent. All managed sessions share one logical app/user namespace, reducing auditability and preventing caller-level ownership enforcement at the session layer. Random session IDs reduce direct collisions but do not restore user identity.

#### Test coverage gap

No test supplies two environment/tenant configurations and verifies different provisioned settings and persistence identities.

#### Required correction

Make authenticated owner/app identity and environment resolution required inputs, validate them at session creation, persist them with the managed session, and build the Runner from those values.

#### Regression tests

Start sessions for two principals/environments and assert isolation across session lookup, memory, sandbox configuration, event streams, and deletion.

### MR-03 — Public managed status is disconnected from normal session-loop transitions

**Severity:** Medium
**Affected capability:** managed lifecycle observability
**Evidence:** Source-confirmed
**Affected code:** `adk-managed/src/default_runtime.rs` — `ActiveSession::status`, `ManagedAgentRuntime::status`; `adk-managed/src/session_loop.rs` — `SessionLoop::status`, `process_turn`, `emit_idle`
**Release attribution:** current HEAD; released-v2 attribution unverified; crate is marked experimental

#### Current behavior

`ActiveSession` owns `Arc<RwLock<SessionStatus>>`, which `status()` reads. `SessionLoop` owns a separate plain `SessionStatus`. Normal queued → running → idle transitions update only the loop field and checkpoint. The public lock changes only through pause, resume, archive, or deletion control methods.

#### Failure mechanism

The comment says the status is shared with the loop, but the constructor does not pass the `Arc<RwLock<_>>` into `SessionLoop`.

#### Impact

A normally executing or idle session can continue to report `Queued`. Control-plane decisions and clients can act on stale lifecycle information.

#### Test coverage gap

No lifecycle test polls public status while a turn moves through running and idle.

#### Required correction

Use one shared status source, update it atomically with checkpointed status events, and define terminal/error transitions.

#### Regression tests

Observe status at each transition, including pause during work, interrupt, tool parking, error, idle, and archive.

### MR-04 — Managed deletion removes only the active handle, not persisted session data

**Severity:** High
**Affected capability:** data deletion and lifecycle cleanup
**Evidence:** Source-confirmed
**Affected code:** `adk-managed/src/default_runtime.rs` — `DefaultManagedAgentRuntime::delete_session`; `adk-session/src/service.rs` — `SessionService::delete`
**Release attribution:** current HEAD; released-v2 attribution unverified; crate is marked experimental

#### Current behavior

`delete_session` archives/cancels the active session and removes it from the in-memory map. It never calls the injected `SessionService::delete`, even though `start_session` seeded a persistent session and the Runner appended events to it.

#### Failure mechanism

Control-plane deletion and data-plane deletion are implemented as one method but only the former occurs.

#### Impact

The API reports deletion while conversation events remain in the configured backend. This can violate retention expectations and allows the orphaned data to survive process restart.

#### Test coverage gap

No test queries the session backend after managed deletion.

#### Required correction

Persist owner identity, invoke identity-aware session deletion, remove managed checkpoints/memory/artifacts according to documented policy, and return partial-failure details if cleanup cannot complete atomically.

#### Regression tests

Delete a session backed by persistent storage and assert session, events, checkpoint, parked state, and configured related data are removed or explicitly retained according to policy.

---

## 5. Server UI bridge and secrets

### SV-01 — UI bridge and resource routes bypass configured server authentication

**Severity:** High
**Affected capability:** authenticated UI state and resource ownership
**Evidence:** Source-confirmed
**Affected code:** `adk-server/src/rest/mod.rs` — `ui_api_router` and router layering; `adk-server/src/rest/controllers/ui.rs` — bridge handlers, `MCP_UI_BRIDGE_REGISTRY`, `UI_RESOURCE_REGISTRY`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

Session, artifact, and debug routers receive `auth_layer`; `ui_api_router` is merged without it. UI bridge request bodies carry caller-supplied `app_name`, `user_id`, and `session_id`, and handlers key a process-global registry by that tuple without deriving identity from `RequestContext`. Resource list/read/register endpoints share a global URI-keyed registry. Registration accepts arbitrary text with MIME type `text/html;profile=mcp-app` and overwrites an existing URI.

Runtime run routes also lack the shared middleware layer, but their handlers explicitly call `extract_request_context`, return 401 on missing/invalid configured auth, and override the supplied user with the authenticated user. They are therefore **not** included in this bypass finding.

#### Failure mechanism

UI routes have neither middleware extraction nor equivalent handler-level extraction. Registries do not store an authenticated owner or tenant.

#### Impact

When server authentication is configured, an unauthenticated caller can create or mutate bridge state under a chosen identity tuple, poll notifications, list/read globally registered resources, and replace HTML resource text. Cross-user state mutation is confirmed by source. Whether a particular UI renders that HTML as executable content was not assessed here.

#### Test coverage gap

No route test configures a rejecting auth extractor and asserts every `/api/ui/*` mutation/read is protected and identity-bound.

#### Required correction

Apply authentication to all non-public UI endpoints, derive user/tenant identity from `RequestContext`, store ownership with resources, authorize every read/write, namespace registries, and define safe HTML/CSP handling. Keep capability discovery public only if explicitly intended.

#### Regression tests

Enumerate the UI route table under configured auth; test missing/invalid tokens, body identity spoofing, cross-user resource access, overwrite authorization, and process-global registry isolation.

### SC-01 — Secret retrieval has no per-tool authorization or audit boundary

**Severity:** High
**Affected capability:** secret access from agent tools
**Evidence:** Source-confirmed
**Affected code:** `adk-core/src/context.rs` — `SecretService`, `InvocationContext::get_secret`; `adk-core/src/tool.rs` — `ToolContext::get_secret`; `adk-auth/src/secrets/provider.rs` — `SecretProvider`, `SecretServiceAdapter`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

A tool with a context can request any secret name string. `SecretService` and `SecretProvider` receive only that name. Their interfaces carry no tool identity, caller principal, namespace, declared allowlist, access decision, purpose, or audit sink.

#### Failure mechanism

Once a secret provider is attached to an invocation, policy is reduced to whatever names the backing cloud credentials can read. The ADK layer cannot distinguish a weather tool requesting its own API key from the same tool requesting a payment or database secret.

#### Impact

A compromised or model-influenced tool can attempt broad secret discovery. Cloud-provider IAM may still prevent access; this finding does not claim those providers leak values. It establishes that ADK supplies no finer per-tool boundary or access audit in the inspected interface.

#### Test coverage gap

Tests cover provider errors/cache behavior, not tool-specific allow/deny decisions or audit records.

#### Required correction

Use a request object containing principal, app/session, tool identity, requested secret/namespace, and purpose. Enforce declarative per-tool grants before provider access and emit a value-free audit event for allow/deny.

#### Regression tests

Verify a tool can retrieve only declared names, spoofed tool identity is rejected, denied names never reach the provider, and audit output contains no secret value.

### SC-02 — Secret cache has no revocation, capacity, purge, or memory-clearing controls

**Severity:** Medium
**Affected capability:** secret lifecycle in process memory
**Evidence:** Source-confirmed
**Affected code:** `adk-auth/src/secrets/cached.rs` — `CachedSecretProvider`, `CachedEntry`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

The cache stores secret `String` values in an unbounded `HashMap`. TTL is checked only when the same name is read. Expired entries are not proactively removed, there is no invalidate/revoke API, no size limit or eviction policy, and values are not zeroized on replacement/drop.

#### Failure mechanism

TTL controls return behavior, not memory residency. Names requested once can remain allocated for the process lifetime.

#### Impact

Revoked/expired material remains recoverable from process memory longer than its TTL, and many unique names can grow the cache without bound. This does not imply normal logs expose values.

#### Test coverage gap

No tests assert purge after time advance, bounded cardinality, explicit invalidation, or memory-clearing behavior.

#### Required correction

Add explicit invalidate/all purge, bounded LRU/size policy, periodic expired-entry removal, and a secrecy-aware value container with zeroization where practical. Document the residual process-memory threat model.

#### Regression tests

Use a controllable clock to test expiry purge, revocation before TTL, capacity eviction, concurrent refresh, and absence of values from Debug/log output.

---

## 6. Coding tools and optional sandbox

### CA-01 — Developer-tool path containment is bypassable through workspace symlinks

**Severity:** High
**Affected capability:** coding-agent filesystem containment
**Evidence:** Source-confirmed
**Affected code:** `adk-devtools/src/workspace.rs` — `Workspace::new`, `Workspace::resolve`; `adk-devtools/src/tools/read.rs` — `ReadFileTool::execute`; `adk-devtools/src/tools/write.rs` — `WriteFileTool::execute`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

The workspace root is canonicalized once. Each requested path is then normalized lexically and checked with `starts_with(root)`. Existing target paths and parent components are not canonicalized or opened with no-follow/dirfd-safe operations.

#### Failure mechanism

A symlink located lexically under the root can point outside it. The resolved path still starts with the root, but ordinary file I/O follows the symlink. A symlinked parent directory similarly redirects creation/writes.

#### Impact

Read/write/edit tools can access host files outside the advertised workspace. The shell tool already has broader host access; this finding shows that even nominally scoped file tools do not provide a containment boundary.

#### Test coverage gap

The containment test covers `..` traversal but not final-component or parent-directory symlinks and replacement races.

#### Required correction

Use descriptor-relative traversal from an opened root, reject symlinks for each component, and create/open with platform no-follow semantics. If symlinks are intentionally allowed, canonicalize and recheck the actual target immediately before access while addressing TOCTOU risk.

#### Regression tests

Create inside-root symlinks to outside files/directories and assert read, write, edit, glob, and grep cannot escape; include a symlink-swap race test where supported.

### CA-02 — Coding modes execute unrestricted host shells while describing the workspace as sandboxed

**Severity:** High
**Affected capability:** coding-agent host isolation
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-devtools/src/workspace.rs` — `Workspace::new`; `adk-devtools/src/tools/bash.rs` — `BashTool::execute`; `adk-cli/src/main.rs` — code/goal workspace construction and `run_check`; `adk-cli/src/ultra.rs` — ultracode workspace construction; `adk-cli/src/cli.rs` — help text
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`Workspace::new` enables writes and bash by default. `BashTool` runs host-local `sh -c`, sets only `current_dir`, inherits the parent environment because it does not call `env_clear`, has no network restriction, and can use absolute paths. Timeout calls `start_kill` on the direct child rather than a Unix process group. CLI code mode uses this workspace unless `--read-only`; goal mode and ultracode use it by default. Goal success checks independently run host `sh -c`.

#### Failure mechanism

A working directory is treated as a security boundary even though the OS does not enforce it. The CLI wires the permissive configuration as the ordinary path and labels it sandboxed.

#### Impact

Model-directed commands can read/write outside the project, access the network, inspect inherited API keys and credentials, and leave descendant processes after a timeout. Users may grant trust based on stronger help/README wording than the implementation supports.

#### Test coverage gap

No adversarial tests attempt absolute-path access, environment-secret reads, network access, or descendant survival through the CLI coding modes.

#### Required correction

Rename this surface as a host workspace unless backed by a real sandbox. Default to an environment-cleared, network-denied, filesystem-confined execution backend with explicit capability grants and confirmation for shell. Use process-group/job-object termination. Run goal checks through the same boundary.

#### Regression tests

Assert default code/goal/ultracode cannot read a sentinel outside root, cannot see an injected parent secret, cannot reach a test listener, and kills child/grandchild processes on timeout.

### SB-01 — `ProcessBackend` defaults to no OS enforcer, and Rust compilation bypasses configured enforcement

**Severity:** High
**Affected capability:** optional code-execution sandbox
**Evidence:** Source-confirmed
**Affected code:** `adk-sandbox/src/process.rs` — `ProcessBackend::default`, `execute_rust`, `run_command`; `adk-sandbox/src/sandbox/mod.rs` — `SandboxPolicy`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`ProcessBackend::default()` has no enforcer, so Python, JavaScript, and command execution are subprocess isolation only. When an enforcer is explicitly configured, runtime commands pass through it. Rust source is first compiled with a direct `rustc` command outside `run_command`; this compile phase has no sandbox wrapper, request timeout, or memory enforcement. `SandboxPolicy.env` is never applied by `ProcessBackend`; only `ExecRequest.env` is used.

#### Failure mechanism

Enforcement is optional and attached at the runtime-command helper, while compilation uses a separate path. Rust compile-time features such as `include_str!` can read host files before the resulting binary enters the runtime boundary.

#### Impact

Default process execution has host filesystem/network access (with a cleared environment). Even a configured OS policy does not constrain Rust compilation, and a compiler can hang beyond the requested execution timeout. Policy environment settings can give callers a false expectation about supplied variables.

#### Test coverage gap

No test compiles Rust that attempts an outside read, blocks in a procedural/build-time operation, or asserts policy env reaches the child.

#### Required correction

Make isolation class explicit at construction, require an enforcer for any API described as sandboxed, and run compilation through the same wrapper and resource limits. Merge/validate policy and request environments under one documented precedence rule.

#### Regression tests

Attempt compile-time outside reads, network access, and timeout; test default capability reporting, policy-env propagation, and configured-enforcer coverage of both compiler and binary.

### SB-02 — macOS filesystem isolation blocks writes but permits global host reads

**Severity:** High
**Affected capability:** macOS Seatbelt confidentiality boundary
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-sandbox/src/sandbox/macos.rs` — `MacOsEnforcer::generate_profile_from_paths`; `adk-sandbox/src/process.rs` — `ProcessBackend::capabilities`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

The generated profile contains both `(deny default)` and `(allow default)`, then selectively denies network, process fork, and all file writes before re-allowing writes to configured paths. It does not deny file reads outside `allowed_paths`; read-only path entries are effectively documentation under the global allow. `ProcessBackend::capabilities` nevertheless marks `filesystem_isolation: true` whenever any enforcer is configured.

#### Failure mechanism

The policy uses a “deny dangerous operations” model rather than an allowed-path read boundary, while capability language and examples describe allowed paths as filesystem access controls.

#### Impact

Sandboxed code can read host files outside configured paths on macOS, even though writes/network/fork may be constrained. This is not a claim that Seatbelt is absent; the profile provides meaningful write and network restrictions but not read isolation.

#### Test coverage gap

Profile tests inspect strings but do not execute a sandboxed process that reads an outside sentinel and writes inside/outside allowed paths.

#### Required correction

Either implement read allowlisting with the system/runtime paths required to launch interpreters, or report the capability as write isolation and document global reads prominently. Capability fields should distinguish read and write isolation.

#### Regression tests

On macOS CI, verify outside reads are denied if full isolation is claimed, allowed writes work, outside writes fail, network policy works, and capability reporting matches observed behavior.

### SB-03 — Windows AppContainer enforcement is structurally present but not implemented

**Severity:** High
**Affected capability:** Windows sandbox availability
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-sandbox/src/sandbox/windows.rs` — `WindowsEnforcer::probe`, `wrap_command`, `configure_command`; `README.md` and `examples/sandbox_agent/README.md` — platform support descriptions
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

On Windows, `probe` checks that the AppContainer API symbol is available. `wrap_command` returns the original executable and arguments. `configure_command` returns `EnforcerFailed` stating that AppContainer configuration is not yet implemented.

#### Failure mechanism

Availability probing validates the platform API, not a usable enforcement implementation. The execution path fails closed when configuration is attempted; it does not silently run sandboxed requests without restrictions.

#### Impact

Windows callers cannot use the advertised AppContainer backend. Cross-platform applications that require the sandbox fail at execution despite successful platform probing and documentation that presents AppContainer as a supported OS profile.

#### Test coverage gap

No Windows integration test creates a restricted process and verifies filesystem/network denial.

#### Required correction

Implement restricted token/AppContainer profile creation, ACLs, capabilities, process startup, cleanup, and job-object termination; make `probe` exercise a restricted child. Until then, mark the backend unavailable in capability/docs.

#### Regression tests

On Windows CI, execute a child that can access only allowed paths, cannot use denied network, receives the intended environment, and is terminated with descendants.

---

## 7. Computer-use adapter

### CU-01 — Security-relevant MCP response objects are not rebound to the requested action locally

**Severity:** High
**Affected capability:** defense-in-depth for governed desktop actions
**Evidence:** Source-confirmed
**Affected code:** `adk-computer-use/src/runtime/mcp.rs` — `ComputerUseMcpRuntime::acquire_lease`, `reserve_target`, `execute_action`; `adk-computer-use/src/contracts/lease.rs` — `ControlLease`, `TargetReservation`; `adk-computer-use/src/contracts/receipt.rs` — `ExecutionReceipt`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

The adapter positively checks session/principal on previews and follow-ups. It does not perform equivalent checks after lease acquisition or target reservation. It deserializes and returns those objects without comparing session, principal, agent, mode, active state, budget, or app/window boundaries to the envelope. After execution, it logs and returns the receipt without checking receipt session, action ID, or action digest against the requested envelope.

#### Failure mechanism

Typed deserialization validates shape but most of these structs have no invariant-enforcing constructor/deserializer. The adapter trusts the remote runtime to bind each returned object correctly.

#### Impact

If the MCP boundary returns stale, confused, or mismatched data, graph state accepts it and can proceed. This is a missing local defense, not evidence that `computer-use-mcp` itself is unsafe; the external runtime remains authoritative and was not independently audited here.

#### Test coverage gap

No adapter test injects a well-formed lease/reservation/receipt for another session, principal, action, mode, or target and expects rejection.

#### Required correction

Add explicit validators that bind every response to the configured principal/session and exact action envelope, including target boundaries, active state, expiry, remaining budget, action ID, and digest. Reject before storing in graph state.

#### Regression tests

Use a fake MCP toolset to return one mismatched field at a time and assert `IdentityMismatch` or a dedicated binding error; include expired and exhausted leases.

### CU-02 — Verification equates “committed” with a verified postcondition

**Severity:** High
**Affected capability:** post-action verification
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-computer-use/src/runtime/mcp.rs` — `ComputerUseMcpRuntime::verify`; `adk-computer-use/src/graph.rs` — `verify` node; `adk-computer-use/src/contracts/action.rs` — `ActionPostcondition`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`verify()` returns only `receipt.status == ReceiptStatus::Committed`. It does not query the desktop, filesystem, registry, process, or window state and does not evaluate the envelope's digest-only `ActionPostcondition` in the ADK adapter.

#### Failure mechanism

Execution acknowledgement and independent postcondition verification are collapsed into one boolean. The reference graph labels the resulting node and output as verification.

#### Impact

A committed action whose intended effect did not occur is reported as completed. The external runtime may already perform verification before issuing a receipt, but that contract is not rechecked or made explicit in this adapter.

#### Test coverage gap

No test returns a committed receipt with a failed/missing postcondition observation and expects `verified=false`.

#### Required correction

Define whether the receipt includes cryptographic/structured verification evidence or call a dedicated read-only verification endpoint. Bind the evidence to action/postcondition digest and freshness before returning true.

#### Regression tests

Cover committed-but-postcondition-failed, stale observation, wrong target/digest, indeterminate receipt, and successful independent verification.

---

## 8. Realtime execution

### RT-01 — Integrated realtime continuity loads history and memory but does not inject them

**Severity:** High
**Affected capability:** session continuity, memory grounding, and plugin lifecycle
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `adk-realtime/src/integration/mod.rs` — `IntegratedRealtimeRunner::connect`, `handle_aggregated_event`; `adk-session/src/service.rs` — identity-aware append API
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`connect` retrieves the prior session into `_session` and discards it. Memory search results are logged; a source comment says actual system-instruction injection is a future enhancement. Completed transcripts are persisted through raw `append_event(session_id, ...)`, and plugin `on_event` is skipped because no `InvocationContext` is available.

#### Failure mechanism

The integration layer performs service calls but has no bridge from returned context into `RealtimeConfig` and no full invocation context for plugins/persistence identity.

#### Impact

A resumed realtime session starts without the history and memory the builder/documentation implies it will use. Transcript writes inherit raw-session ambiguity, and configured event plugins do not run.

#### Test coverage gap

Tests cover persistence mechanics but not provider configuration containing prior turns/memory or plugin event invocation.

#### Required correction

Construct a bounded, policy-controlled context injection from session history and memory before connect; use full identity append; and create a realtime invocation context suitable for lifecycle plugins.

#### Regression tests

Inspect the provider connect config for prior context, verify identity-aware persistence with duplicate raw IDs, and assert plugin `on_event` runs for user/assistant/tool events.

### RT-02 — The active integrated tool bridge bypasses the configured plugin and control pipeline

**Severity:** High
**Affected capability:** realtime tool authorization, confirmation, retries, and plugins
**Evidence:** Source-confirmed
**Affected code:** `adk-realtime/src/integration/builder.rs` — ADK tool registration; `adk-realtime/src/integration/tool_bridge.rs` — `ToolBridgeAdapter::execute`; `adk-realtime/src/integration/mod.rs` — `next_event`, `execute_tool_with_plugins`; `adk-realtime/src/runner.rs` — `dispatch_tool_call`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

The builder wraps each ADK tool in `ToolBridgeAdapter`, and the active event path calls `RealtimeRunner::dispatch_tool_call`, which invokes that adapter directly. The adapter creates a context and calls `tool.execute`. It does not add confirmation, timeout, retry/circuit-breaker policy, before/after callbacks, or plugin hooks. The richer `execute_tool_with_plugins` method is not connected to this path. Its own before-plugin error branch would log and execute the tool directly.

Pre-wrapped tools such as `ScopeGuard` can still enforce their own wrapper logic; the bridge itself does not inspect `Tool::required_scopes`, and its default context carries no authenticated scopes.

#### Failure mechanism

Tool metadata, services, and a plugin manager are collected by the integration builder but execution is delegated to a lower-level handler interface that lacks those policies.

#### Impact

A tool that is controlled in the standard agent loop can run in realtime without equivalent confirmation/callback/plugin policy. A security plugin failure would be fail-open in the unused helper if it were wired unchanged.

#### Test coverage gap

No parity test runs one protected tool through standard and integrated realtime paths and compares authorization, callbacks, confirmation, timeout, retry, and audit outcomes.

#### Required correction

Route all realtime ADK tools through one policy executor shared with the standard agent path. Security plugin errors must fail closed by default. Carry authenticated scopes/secrets/cancellation into the realtime context and make native-handler bypass explicit.

#### Regression tests

Cover denied scopes, confirmation required, before-plugin deny/error, timeout, retry, after-plugin modification, cancellation, and an explicitly trusted native handler.

### RT-03 — Direct `RealtimeAgent` ignores before-tool callback errors and lacks context capabilities

**Severity:** High
**Affected capability:** direct realtime agent tool safety
**Evidence:** Source-confirmed
**Affected code:** `adk-realtime/src/agent.rs` — `RealtimeAgent::run`, `RealtimeToolContext`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

In the active `FunctionCallDone` branch, a before-tool callback error constructs an `(error_result, EventActions::default())` tuple inside the loop but discards it; execution then continues to `tool.execute`. After-tool callback errors are explicitly ignored. `RealtimeToolContext` delegates identity and memory search but not shared state, `user_scopes`, or `get_secret`, so those methods use empty/none defaults. Direct dispatch also has no confirmation or tool timeout.

#### Failure mechanism

The callback loop does not store a denial/error result or branch around execution. The context wrapper implements only required methods and inherits permissive/empty defaults.

#### Impact

A callback intended to block a tool cannot do so by returning an error in this path. Tools depending on scopes/secrets/shared coordination behave differently or fail closed unpredictably compared with the standard agent context.

#### Test coverage gap

No test configures a before-tool callback that errors and asserts the tool's execution counter remains zero; no context sentinel test covers scopes/secrets/shared state.

#### Required correction

Use the standard callback semantics: a deny/error must prevent execution, after-callback failures must follow an explicit policy, and context capabilities must delegate to the parent. Add confirmation, timeout, cancellation, and protected-tool handling through a shared executor.

#### Regression tests

Assert zero execution on callback deny/error, exact callback order, after-error behavior, and preservation of scopes, secret access, shared state, artifacts, and identity.

### RT-04 — Realtime tool concurrency is configured but not enforced, and transport loss is not automatically recovered

**Severity:** Medium
**Affected capability:** realtime latency and connection resilience
**Evidence:** Source-confirmed
**Affected code:** `adk-realtime/src/runner.rs` — `RunnerConfig::max_concurrent_tools`, `RealtimeRunner::run`, `handle_event`, `execute_tool_call`, resumption state machine
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

`max_concurrent_tools` defaults to four, but the only source references are the field and default; no semaphore or scheduler consumes it. `FunctionCallDone` is handled inline and awaits tool completion before the event loop reads another event. On a real disconnect, `run` exits successfully after distinguishing it from a concurrently installed session. Reconnection logic is used for deliberate context mutation (“phantom reconnect”), with process-local pending state and three attempts, not for unexpected transport loss.

#### Failure mechanism

Configuration and comments describe concurrency/resumption capabilities that are not connected to the event dispatch loop.

#### Impact

Multiple provider tool calls execute serially and block audio/event processing. Transient network loss ends the session rather than reconnecting with retained context, while callers may infer broader reconnect behavior from the state-machine documentation.

#### Test coverage gap

No test measures overlapping tool handlers under `max_concurrent_tools > 1` or simulates unexpected disconnect and recovery.

#### Required correction

Implement bounded task dispatch with ordered response aggregation and event-loop responsiveness. Define explicit reconnect policy for transport loss, persist/restore provider resume tokens where supported, and surface terminal disconnect distinctly.

#### Regression tests

Use barriers to prove the configured concurrency bound, ensure audio events continue during tools, and test recoverable/unrecoverable disconnects plus retry-budget exhaustion.

### RT-05 — OpenAI realtime schema-drift logs can contain raw user/provider content

**Severity:** Medium
**Affected capability:** telemetry privacy
**Evidence:** Source-confirmed
**Affected code:** `adk-realtime/src/openai/session.rs` — `OpenAIRealtimeSession::receive_raw`
**Release attribution:** current HEAD; released-v2 attribution unverified

#### Current behavior

When a recognized event type fails deserialization, the warning logs the first 300 bytes of the raw WebSocket text. This path is not conditioned on a payload-recording opt-in or redaction policy.

#### Failure mechanism

Schema diagnostics capture the raw frame rather than a field-safe summary. Realtime frames can contain transcripts, tool arguments/results, identifiers, or other user/provider content.

#### Impact

Sensitive conversation content can enter warning logs during provider schema drift—the exact moment operators may raise log collection/retention. The finding does not claim such a drift event was observed in a live provider session.

#### Test coverage gap

No test sends a malformed recognized event containing a sentinel secret and inspects captured logs for redaction.

#### Required correction

Log event type, parse error, payload size, and a digest by default. Gate bounded raw payload recording behind the existing explicit telemetry payload policy and apply structured redaction.

#### Regression tests

Capture tracing output for malformed frames and assert sensitive sentinel text is absent by default and only appears under an explicit, documented opt-in.

---

## 9. Release and documentation controls

### DOC-01 — Version metadata does not provide a reproducible local v2 source baseline, while capability claims exceed implementation

**Severity:** Medium
**Affected capability:** release provenance and user decision-making
**Evidence:** Source-confirmed and documentation mismatch
**Affected code:** `Cargo.toml` — workspace version; `CHANGELOG.md` — `2.0.0` entry; `README.md` — release/current-status, graph, background, coding, sandbox, and managed-runtime claims; `adk-graph/README.md` — durable-resume claim
**Release attribution:** metadata finding applies to the inspected repository; exact packaged-source attribution is unresolved

#### Current behavior

The workspace and installation snippets use `2.0.0`, the changelog dates 2.0.0, the README banner still announces v1, and the roadmap calls v2 current. No local `v2.0.0` tag identifies the exact release source. Separately, documentation says graph durable resume skips completed nodes, background runs execute workflows, coding workspaces are sandboxed, OS profiles include AppContainer, and managed execution is durable; the findings above establish narrower or placeholder behavior at current HEAD.

#### Failure mechanism

Version, release, and capability statements are maintained in multiple files without one machine-verifiable release object or capability test that gates the claims.

#### Impact

Reviewers cannot reliably attribute a current defect to the published v2 artifact from the local repository alone. Users can select features based on guarantees that the implementation only partially provides.

#### Test coverage gap

Existing documentation checks validate many command/feature references, but not semantic capability claims or correspondence between version/changelog/tag/source archive.

#### Required correction

Create and verify an immutable release tag/source manifest, record crate checksums/commit ID, and generate version statements from one source. Gate strong capability wording on executable conformance tests; label experimental/placeholders at the first public mention.

#### Regression tests

Add a release-metadata check requiring workspace version, changelog heading, README current version, and tag/manifest to agree. Maintain claim-to-test mappings for durable resume, background execution, sandbox isolation, and managed recovery.

---

## Cross-cutting correction sequence

The ordering below is based on dependency and risk, not release attribution.

1. **Restore exact execution semantics.** Fix graph checkpoint ordering, graph streaming persistence, Messages-mode double execution, and `ParallelAgent` stream polling before building additional orchestration features on them.
2. **Make context capabilities structurally preservable.** Introduce a complete delegating context mechanism and migrate workflow, graph, realtime, and plugin adapters. Include full identity and cancellation in every long-running path.
3. **Unify tool policy execution.** Route standard and realtime tools through one executor for scope wrappers, confirmation, callbacks/plugins, timeout, retry, cancellation, and audit. Bind approvals to exact calls.
4. **Close externally reachable trust gaps.** Authenticate/authorize UI bridge routes and webhook triggers; namespace state by verified identity; add secret access policy.
5. **Separate host workspaces from enforced sandboxes.** Correct naming immediately, then implement no-follow file containment, environment/network isolation, compiler confinement, and platform-accurate capability reporting.
6. **Make asynchronous runtimes truthful.** Do not report background completion until a registered workflow ran. Add durable stores and restart reconstruction for background, cron, and managed execution.
7. **Harden external adapters.** Locally validate computer-use leases/reservations/receipts and perform or verify postcondition evidence.
8. **Normalize provider contracts.** Require explicit accept/downgrade/reject outcomes for schemas and every content-part class.
9. **Repair release provenance and claims.** Establish one immutable v2 baseline and tie strong documentation statements to conformance tests.

## Required regression-test program

A minimal release gate for the corrected behavior should include:

- **Exactly-once graph suite:** invoke, every stream mode, interrupt, resume, deferred fan-in, and terminal checkpoint with non-idempotent counters.
- **Context conformance suite:** sentinel values for identity, scopes, metadata, secrets, artifacts, memory, shared state, and cancellation through every wrapper/adapter.
- **Session identity suite:** duplicate raw session IDs across app/user dimensions for read, append, interrupt, realtime transcript, and deletion paths.
- **Tool policy suite:** identical protected tool through `LlmAgent`, direct `RealtimeAgent`, and `IntegratedRealtimeRunner`, including exact-call approval binding.
- **Background recovery suite:** real workflow resolution, timeout/cancel/retry, cron occurrence claiming/queue draining, and restart reconstruction.
- **Filesystem/sandbox adversarial suite:** traversal, symlink escape, environment leakage, network, compile-time reads, child-process escape, and platform capability checks.
- **External object-binding suite:** malformed/mismatched computer-use lease, reservation, receipt, and postcondition evidence.
- **Provider conversion matrix:** schema and every `Part` variant for each provider adapter, with no silent loss.
- **Route authorization inventory:** automatically enumerate every stateful/read-sensitive route and assert configured auth plus owner binding.
- **Telemetry privacy suite:** sentinel secrets in malformed provider events, errors, tool arguments, and traces with payload recording disabled.

## Pending review areas

The following areas were not reviewed deeply enough to support absence or correctness claims in this document:

- complete deployment/cloud integration behavior and production topology;
- full telemetry pipeline, exporter defaults, retention, and redaction outside the realtime schema-drift path;
- evaluation framework correctness and benchmark methodology;
- complete CI, feature-matrix, semver, packaging, and crates.io artifact verification;
- the full documentation site and every example/template;
- Slack, email channel integrations, and notification systems beyond the inspected `WebhookTrigger` and graph email placeholder;
- all session and memory backend implementations under fault injection;
- external `computer-use-mcp` server implementation and its own validation/idempotency guarantees;
- browser automation and A2A/ACP/AWP protocol security as complete subsystems;
- standard server/CLI end-user memory management controls outside the inspected route/command assembly.

No conclusion should be drawn from this list other than that review is pending. In particular, this document does not claim that channel integrations or memory APIs are absent repository-wide.

## Evidence appendix

### Executed probe outputs

The following outputs were recorded from local probes against the inspected source:

```text
Graph checkpoint replay:
first_count=1 second_count=2

Parallel lazy streams (two 300 ms streams):
elapsed=604 ms

Context capability propagation:
direct: cancellation=true, secret=present, shared_state=present
loop/sequential: cancellation=false, secret=absent, shared_state=absent
parallel shared: cancellation=false, secret=absent, shared_state=present

Provider terminal response errors through LlmAgent:
stream_errors=[]
emitted event error fields=absent
```

Temporary probe files were kept outside the repository under `/tmp/adk-parallel-timing-probe`; implementation source was not changed.

### Scoped source searches used for qualified claims

- Context wrappers and optional `InvocationContext` methods were searched across `adk-core`, `adk-runner`, `adk-agent`, and graph/realtime adapters.
- Graph checkpoint writes were searched across `adk-graph/src`; only non-streaming `PregelExecutor::run` calls `save_checkpoint`.
- Response-schema propagation was searched across `adk-model/src`; explicit mappings were found in Gemini, Gemini Interactions, OpenAI-compatible, and OpenRouter paths, while DeepSeek hardcodes `None` in its request builder.
- Computer-use response checks were inspected within `adk-computer-use/src/runtime/mcp.rs`; this scope does not include the external MCP server.
- Realtime `max_concurrent_tools` references were searched across `adk-realtime/src`; only the field and its default were found.
- Functional interrupt handling was searched repository-wide for `InterruptTypeMismatch`, `__interrupt__`, and the emitted interruption message; no separate resume-value handler was found.

### Important interpretation limits

- Source-confirmed behavior can still be mitigated by deployment controls outside this repository.
- Documentation alone was not used to infer runtime behavior.
- A missing use within an explicitly named search scope is not a claim of universal absence.
- External providers and servers were not declared insecure based solely on missing local defense-in-depth.
- No Linux bubblewrap effectiveness or network-permissiveness finding is made in this report.
- macOS Seatbelt is not described as absent: the inspected policy restricts writes, network, and process fork but permits broad reads.
- Windows AppContainer configuration fails closed as unimplemented; this report does not claim it silently executes an allegedly sandboxed command.
