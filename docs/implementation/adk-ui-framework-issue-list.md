# ADK UI Framework Issue List

Date: 2026-03-20
Owner: framework team (`adk-rust`)
Scope: protocol contracts that should be framework-owned rather than patched inside `adk-ui`

## Goal

Move UI protocol support from app-specific compatibility shims into stable, framework-owned contracts without breaking existing `adk-rust` server, agent, or non-UI consumers.

## Compatibility Guardrails

- Keep all rollout steps additive first. Do not remove or change the existing `/api/run`, `/api/run_sse`, or UI tool response shapes until a dual-path migration has shipped.
- Preserve the current generic ADK runtime event stream for non-UI consumers. Protocol-native transport should be opt-in by request and clearly versioned.
- Keep `/api/ui/capabilities` truthful. If a protocol is only partially supported, the framework should advertise it as a subset instead of implying full spec parity.
- Keep MCP Apps resource endpoints (`/api/ui/resources`, `/api/ui/resources/read`, `/api/ui/resources/register`) working as-is while a richer host bridge is added beside them.
- Do not make `adk-server` depend on `adk-ui`. Shared protocol metadata should live in a small framework-owned module or crate.

## Completed Groundwork In This Patch

- `adk-server` capability metadata now includes `implementationTier`, `specTrack`, `summary`, and `limitations` in `/api/ui/capabilities`.
- Framework docs now describe A2UI as a draft-aligned subset, AG-UI as a hybrid subset, and MCP Apps as a compatibility subset.
- `adk-server` now exposes additive HTTP helpers for `ui/initialize`, `ui/message`, and `ui/update-model-context`, with optional JSON-RPC-like envelopes.
- `adk-server` now exposes additive MCP Apps notification helpers for polling queued notifications and reporting resource/tool list changes.
- `/api/run` and `/api/run_sse` now support additive `uiTransport` / `x-adk-ui-transport` negotiation, with opt-in `protocol_native` AG-UI SSE serialization.
- `/api/run_sse` now accepts dual-path AG-UI input (`input` / `agUiInput`) beside the existing generic `newMessage` request shape.
- `/api/run_sse` now accepts additive MCP Apps bridge envelopes (`mcpAppsInitialize`, `mcpAppsRequest`, `mcpAppsInitialized`) and routes them through the same framework-owned bridge state used by the direct `/api/ui/*` endpoints.
- CORS now allows the transport header needed for browser-based protocol negotiation.
- `adk-server::ui_types` now exposes `McpUiToolResult` and `McpUiToolResultBridge` as the canonical additive helper for MCP Apps tool responses, with typed bridge metadata plus `resourceUri` / `html` fallbacks.
- `adk-server::ui_types` now exposes `McpUiBridgeSnapshot`, and `adk-server` bridge controllers now reuse that typed snapshot as the internal constructor layer for MCP Apps host/app bridge state.
- `adk-server` now emits native AG-UI `TEXT_MESSAGE_CHUNK` events for partial assistant text, `REASONING_MESSAGE_CHUNK` events for `Part::Thinking`, `TOOL_CALL_CHUNK` events when partial function-call args are available as string deltas on `protocol_native` streams, and input-derived `ACTIVITY_SNAPSHOT` plus `ACTIVITY_DELTA` events for frontend-only activity continuity, with fixture-backed regression coverage.
- Framework examples and docs now include a runnable MCP Apps tool-result source example, fixture-backed summaries for canonical MCP Apps output and AG-UI native SSE, and updated MCP Apps lifecycle/migration guidance.

## Remaining Follow-On Issues Beyond The Current App Modernization Scope

### P0. Define a Framework-Owned UI Capability Schema

Problem:
`adk-server` and `adk-ui` have been drifting in how they describe protocol support. That creates capability mismatches and forces the app repo to override framework copy locally.

Why framework-owned:
Capability truthfulness is a server contract. Clients should not need per-app patches to learn what the framework actually supports.

Proposed change:
- Move the enriched capability schema into a single shared framework definition.
- Expose the same shape everywhere the framework reports UI protocol support.
- Keep the current fields (`protocol`, `versions`, `features`, `deprecation`) and add support-boundary fields rather than replacing anything.

Acceptance criteria:
- `/api/ui/capabilities` stays backward compatible.
- Capability metadata is defined in one place.
- Docs and tests consume the same source of truth.

### P1. Add Durable MCP Apps Host Persistence And Full Embedded-Host Parity

Problem:
The framework now exposes additive HTTP helpers, initialized notifications, change-notification flows, and runtime-side bridge request fields for MCP Apps, but bridge state is still in-memory and the framework does not yet provide full browser `postMessage` parity for deeply embedded hosts.

Why framework-owned:
Durable bridge persistence and full embedded-host transport semantics are still server/runtime contract concerns, not renderer-specific app logic.

Likely touch points:
- `adk-server/src/rest/controllers/runtime.rs`
- `adk-server/src/rest/controllers/ui.rs`
- shared request/response models for host bridge payloads

Proposed change:
- Keep the new additive HTTP helpers and runtime request fields as the stable compatibility layer.
- Move bridge state from process-memory storage toward runtime/session-aware persistence where needed.
- Define how a fuller browser `postMessage` host bridge maps onto the existing server-side primitives without breaking existing clients.

Compatibility requirements:
- Existing HTTP bridge endpoints and resource-only flows must continue to work unchanged.
- Full transport integration must remain opt-in until clients migrate.

Acceptance criteria:
- A host can survive restarts or multi-process deployments without losing required bridge session state.
- A richer embedded host can map onto framework-owned MCP Apps bridge primitives without inventing repo-specific contracts.
- Existing resource-only and additive HTTP bridge consumers still pass unchanged.

### P1. Emit Native AG-UI Stable Event Families From The Framework

Problem:
The server boundary can now translate generic runtime events into a stable AG-UI subset, but the agent/runtime stack still produces generic ADK events internally.

Why framework-owned:
Stable event production belongs in the runtime/agent stack, not in downstream UI examples.

Likely touch points:
- `adk-agent/src/llm_agent.rs`
- server serialization in `adk-server/src/rest/controllers/runtime.rs`

Proposed change:
- Keep the additive server-side serializer, but move more of the stable-event production deeper into the framework so the server is not reconstructing as much from generic ADK events.
- Cover at least run lifecycle, text message content/chunks, tool call lifecycle, run errors, and message snapshots.
- Move more activity and snapshot semantics below the server boundary where practical.
- Keep custom events available for app-specific extensions.

Compatibility requirements:
- Existing generic ADK events remain available.
- AG-UI emission should be opt-in and versioned.

Acceptance criteria:
- A client can render from AG-UI stable events without depending on a custom surface tunnel.
- Tool call and tool result metadata are preserved end-to-end.
- The server translation layer becomes thinner because more AG-UI semantics are emitted directly by the framework.

### P2. Broaden Adoption Of The Canonical MCP Apps Tool Result Helper

Problem:
The framework now provides both the canonical MCP Apps tool-result helper and a typed bridge snapshot constructor layer, but external examples and downstream adapters have not all migrated to it yet.

Why framework-owned:
The framework should not just define the helper; its own official examples and adapters should emit the same shape so downstream apps do not drift again.

Likely touch points:
- tool response adapters
- official examples
- framework docs and examples

Proposed change:
- Keep migrating official framework examples and adapters to `McpUiBridgeSnapshot::build_tool_result(...)`.
- Keep HTML/resource fallback available.
- Add fixture coverage that asserts the helper shape at the response boundary.

Compatibility requirements:
- Existing tool payloads remain valid.
- Helper adoption must be additive and documented.

Acceptance criteria:
- Official framework examples emit the canonical helper shape.
- Multiple apps can emit the same MCP Apps response structure without custom per-repo conventions.

### P2. Extend Conformance Fixtures Beyond The Current Core Paths

Problem:
The current core paths now have fixture coverage, but additional protocol paths can still drift if the framework expands further without adding new fixtures.

Why framework-owned:
The framework owns the server/runtime boundary and should prevent regressions centrally.

Proposed change:
- Keep the existing fixture-driven tests for `/api/ui/capabilities`, AG-UI native SSE summaries, and MCP Apps bridge contracts.
- Add more fixtures when the framework expands beyond the current core protocol paths.
- Add more generic-mode and mixed-mode snapshots if breaking changes become likely.

Acceptance criteria:
- Core protocol metadata, request parsing, and stream serialization continue to have regression coverage.
- New protocol work adds fixtures as part of landing new behavior.

### P2. Keep Official Docs And Migration Guidance In Sync With Future Changes

Problem:
The docs are now aligned with the current implementation, but future framework changes can drift again if documentation is not maintained with the same rigor as the capability endpoint and fixtures.

Why framework-owned:
Framework documentation should match framework behavior, especially when external teams build clients against it.

Proposed change:
- Keep `adk-server` and official UI tool docs aligned with `/api/ui/capabilities` and the shipped bridge/runtime contracts.
- Extend migration guidance only when the framework actually introduces new opt-in transport paths or deprecations.

Acceptance criteria:
- Docs match `/api/ui/capabilities`.
- Teams have a clear path from compatibility mode to native protocol mode.

## Recommended Rollout Order

1. Keep the capability schema truthful and centralized.
2. Deepen AG-UI stable event production inside the runtime/agent stack where it materially reduces server-edge reconstruction.
3. Add durable MCP Apps persistence and richer embedded-host parity only if product requirements justify it.
4. Extend conformance coverage when new protocol surface area is introduced.
5. Update migration guidance and only then consider deprecating compatibility shims.
