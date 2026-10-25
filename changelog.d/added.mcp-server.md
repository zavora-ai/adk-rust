- **MCP server for ADK-Rust tools** (`adk-tool`, feature `mcp-server`, experimental): `McpServer`
  publishes a set of `Tool`s over MCP Streamable HTTP (spec 3.5 §7.9). Each tool's name,
  description and schemas become its MCP definition and its `ToolEffect` sets the
  `readOnlyHint`, `idempotentHint` and `destructiveHint` annotations. Every call goes through an
  `McpCallExecutor` hook that can refuse it, with the caller an `McpCallerResolver` resolves from
  the HTTP request. `McpRouterBuilder` returns an `axum::Router` that bounds bodies, answers
  malformed JSON-RPC with standard error codes, binds sessions to their caller and serves the
  MCP Server Card (SEP-2127) at `{endpoint}/server-card`, an AI Catalog at
  `/.well-known/ai-catalog.json` and the card's `/.well-known/mcp/server-card.json` alias.
