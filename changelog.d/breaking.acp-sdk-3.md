- **`agent-client-protocol` 3.3** (`adk-acp`): the ACP Rust SDK moves from 2.2 to 3.3 (ACP
  schema 1.11). `adk-acp` re-exports it as `adk_acp::agent_client_protocol`, so code that
  uses the re-export follows the SDK's
  [3.0 migration notes](https://github.com/agentclientprotocol/rust-sdk/blob/main/src/agent-client-protocol/CHANGELOG.md).
  The ACP wire protocol stays at version 1.
