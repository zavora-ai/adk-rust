# MCP Server Card schema fixtures

Vendored, unmodified, from
[`modelcontextprotocol/experimental-ext-server-card`](https://github.com/modelcontextprotocol/experimental-ext-server-card)
at commit `526201bbc80231daa40ffcdecfc9da4e54e5dc93` — the snapshot SEP-2127
([modelcontextprotocol#2127](https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2127),
merged 2026-10-06) names as the contract it accepted.

| File | Upstream path |
|------|---------------|
| `schema.json` | `schema.json` (generated from `schema.ts`; SHA-256 `2c772b51edb367f154771d84ddbae87ddba00a624422c8e46f218a9ac03bf042`) |
| `valid/*.json` | `examples/ServerCard/valid/` |
| `invalid/*.json` | `examples/ServerCard/invalid/` |

Licensed under the Apache License 2.0 by the Model Context Protocol authors.
`tests/mcp_server_card_tests.rs` validates the cards `adk_tool::mcp::server` produces against
`#/$defs/ServerCard`, as the upstream `scripts/validate-examples.ts` does.
