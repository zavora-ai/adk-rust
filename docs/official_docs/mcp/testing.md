# MCP testing and verification

MCP integrations cross process, transport, schema, and authorization
boundaries. Unit tests alone are insufficient; keep at least one deterministic
client/server test that completes a real handshake and tool call.

## Focused framework gates

```bash
cargo test -p adk-tool --features mcp --lib mcp
cargo test -p adk-tool --features mcp --test mcp_roundtrip_property_tests
cargo test -p adk-tool --features mcp,mcp-sampling --test mcp_sampling_property_tests
cargo clippy -p adk-tool --features mcp,http-transport,mcp-sampling -- -D warnings
```

The sampling gate is compatibility coverage for an upstream-deprecated
capability, not a recommendation for new architecture.

## Compile integration consumers

```bash
cargo check -p adk-acp --all-features
cargo check -p adk-computer-use
```

These gates catch mismatched public `rmcp` types across crates that embed or
carry MCP configurations.

## Standalone examples

```bash
cargo check --manifest-path examples/mcp_elicitation/Cargo.toml
cargo check --manifest-path examples/mcp_sampling/Cargo.toml
cargo check --manifest-path examples/mcp_manager/Cargo.toml
cargo run --manifest-path examples/mcp_manager/Cargo.toml
```

`examples/mcp_manager` is the deterministic live gate. It starts a real Rust
stdio server by executing its own binary in fixture mode. The test does not
download a package or contact a remote endpoint.

## What the live example proves

- child-process startup;
- MCP initialization and capability negotiation;
- tool discovery;
- a real tool request and result;
- runtime add, enable, update, disable, and remove;
- registry serialization and file persistence; and
- session shutdown.

## What requires deployment-specific testing

- remote HTTP identity and token policy;
- full MCP OAuth discovery and user authorization;
- external API and database health;
- process sandbox enforcement;
- tenant isolation;
- approval UI and audit storage;
- retry and idempotency behavior for consequential tools; and
- task recovery after an application restart.

## Test server contracts

For every server, keep fixtures for:

- initialization capability accuracy;
- tool and resource schemas;
- invalid and unauthorized inputs;
- maximum result sizes;
- cancellation and timeout behavior;
- idempotent retry of write operations; and
- redaction of credentials and sensitive fields.
