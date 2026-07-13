# adk-computer-use

First-party ADK-Rust reference orchestration for `computer-use-mcp` v8.

The crate deliberately does not implement desktop actuation. It supplies:

- camelCase wire types for v8 capabilities, action previews, leases, receipts, and events;
- operation-aware auth context and the `computer:*` scope model;
- a deterministic `adk-graph` workflow with parallel capability/visual/semantic observation, fan-in, preview routing, human interrupt, action-and-policy-digest-bound resume, planner target reservation, exactly one executor node, reservation release, and independent verification;
- a runtime trait that can be backed by MCP, in-process TypeScript, or a deterministic fake.

Desktop policy, target validation, lease ownership, physical-user interruption, and idempotent effects remain authoritative in `computer-use-mcp`.

The graph constructs authorization context from the v8 action envelope plus the
tenant identity already verified by `adk-auth`; model/graph state cannot supply
or replace the authenticated principal or tenant.

The crate also consumes the same versioned `fixtures/v8/safety-corpus.json` as
the TypeScript fake-desktop harness. This keeps graph/evaluation contracts tied
to the runtime's commit, stale-target, crash, revocation, and replay invariants.

## Live graph showcase

The compiled example launches the real MCP server, starts an authenticated v8
session, runs capability/visual/semantic observation concurrently, previews a
background clipboard write, acquires the one-writer lease, executes once, and
verifies the receipt:

```bash
COMPUTER_USE_PRINCIPAL_ID=adk-local-operator \
cargo run -p adk-computer-use --example live_v8_graph
```

Set `COMPUTER_USE_MCP_PACKAGE` to a local/package specifier when testing an
unpublished preview. The example intentionally changes the clipboard and does
not bypass v8 policy, identity, lease, or receipt enforcement. It launches the
server with `COMPUTER_USE_ACTIVE_PROFILE=v8-safe`; even visual and semantic
observations traverse `execute_action` in shadow mode, so no raw MCP actuator or
observer is available to the graph.
