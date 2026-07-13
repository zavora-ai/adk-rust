# adk-computer-use

First-party ADK-Rust reference orchestration for `computer-use-mcp` v8.

The crate deliberately does not implement desktop actuation. It supplies:

- camelCase wire types for v8 capabilities, action previews, leases, receipts, and events;
- operation-aware auth context and the `computer:*` scope model;
- a deterministic `adk-graph` workflow with parallel capability/visual/semantic observation, fan-in, preview routing, human interrupt, action-and-policy-digest-bound resume, planner target reservation, exactly one executor node, reservation release, and independent verification;
- typed digest-only UI/filesystem/registry/process/window postconditions plus value-free AX/UIA target-sensitivity evidence that survive Rust/TypeScript wire round-trip and remain part of the exact action/approval digest;
- a runtime trait that can be backed by MCP, in-process TypeScript, or a deterministic fake.
- principal-checked monotonic follow-up consumption for remote supervisor steering; instruction text stays outside the redacted event stream;

Desktop policy, target validation, lease ownership, physical-user interruption, and idempotent effects remain authoritative in `computer-use-mcp`.

The graph constructs authorization context from the v8 action envelope plus the
tenant identity already verified by `adk-auth`; model/graph state cannot supply
or replace the authenticated principal or tenant.

The crate also consumes the same versioned `fixtures/v8/safety-corpus.json` as
the TypeScript fake-desktop harness. This keeps graph/evaluation contracts tied
to the runtime's commit, stale-target, crash, revocation, and replay invariants.

## Release evaluation evidence

The crate exposes `AdkEvaluationReceipt` and publishes a canonical receipt
fixture. Its generator runs the complete `adk-computer-use` suite plus the real
`adk-tool` MCP structured-text/image preservation test. It requires exact tests
for parallel observation with one executor, action/policy digest approval
binding, verified auth principal/tenant binding, pre-effect crash retry,
post-commit receipt replay, duplicate-mutation rejection, and multimodal image
delivery:

```bash
node scripts/generate-computer-use-v8-evidence.mjs \
  --subject-version 7.0.0 \
  --output adk-computer-use-v8-evidence.json
```

The receipt contains source/output digests and a canonical receipt digest. It
is deliberately unsigned: ADK CI uploads it, then a release authority reviews
and signs the matching `adk_graph` evidence statement using a key trusted by
the computer-use v8 readiness evaluator. CI output alone cannot self-promote a
release stage.

## Live graph showcase

The compiled example sends a natural-language prompt through an ADK `LlmAgent`
with schema-constrained output, then launches the real MCP server, starts an
authenticated v8 session, runs capability/visual/semantic observation
concurrently, previews the planned background clipboard write, acquires the
one-writer lease, executes once, verifies the receipt, and confirms the real
macOS clipboard value:

```bash
COMPUTER_USE_PRINCIPAL_ID=adk-local-operator \
cargo run -p adk-computer-use --example live_v8_graph
```

Set `COMPUTER_USE_MCP_PACKAGE` to a local/package specifier when testing an
unpublished preview, or set `COMPUTER_USE_MCP_ENTRYPOINT` to a built local
`dist/server.js`. Pass the desired task as command-line text. The planner is
deliberately restricted to one public `write_clipboard` showcase action; the
v8 graph remains the sole executor. The example intentionally changes the clipboard and does
not bypass v8 policy, identity, lease, or receipt enforcement. It launches the
server with `COMPUTER_USE_ACTIVE_PROFILE=v8-safe`; even visual and semantic
observations traverse `execute_action` in shadow mode, so no raw MCP actuator or
observer is available to the graph.
