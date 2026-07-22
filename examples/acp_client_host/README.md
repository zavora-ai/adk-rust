# ACP client and host example

This example shows the client side of ACP. The application starts an external
coding agent and remains responsible for the interface, project boundary,
permissions, and services supplied to that agent.

It demonstrates:

- stable ACP v1 initialization over stdio;
- streamed text, tool, permission, completion, and error updates;
- an asynchronous permission policy that allows read-oriented work once and
  denies other operations;
- an `AcpFileSystem` implementation that exposes only file reads;
- canonical-path workspace enforcement, including rejection of paths outside
  the approved project;
- accurate capability negotiation: file writes and terminal execution are not
  advertised.

## Run it

Choose an ACP-compatible coding agent and use the command that starts its ACP
stdio mode:

```bash
cd examples/acp_client_host
cp .env.example .env
# Edit ACP_AGENT_COMMAND in .env
cargo run
```

The example does not require a model API key itself. Authentication required by
the external coding agent remains that agent's responsibility.

## What the boundary means

`working_dir` tells the agent which project the session belongs to. The
`ReadOnlyWorkspace` callback separately decides which file requests the client
will honor. It canonicalizes the requested path before comparing it with the
approved root, so `..` and symlink paths cannot escape the example workspace.

The permission policy handles tool approval requests made by the coding agent.
It is an additional control and does not replace filesystem validation or an
operating-system sandbox.

## Verify without an external agent

The filesystem boundary has deterministic tests:

```bash
cargo test --manifest-path examples/acp_client_host/Cargo.toml
```

For a continuing multi-turn process and concurrent cancellation, see
[`examples/acp_kiro`](../acp_kiro). To expose an ADK-Rust agent to an editor,
see [`examples/acp_server`](../acp_server).
