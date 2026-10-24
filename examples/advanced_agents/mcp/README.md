# MCP 2026-07-28 discovery and tasks

> **Note:** `/ui/` answers `404`: `adk-server` no longer bundles a runtime UI (spec 3.3 §10
> S1). Send prompts through the REST API the runtime serves under `http://127.0.0.1:8088/api/`
> (`POST /api/run_sse` or `POST /api/run`; see
> [Server API](../../../docs/official_docs/deployment/server.md)).

The bundled `advanced-mcp-server` publishes an immediate `count_stock` tool and
a slow `restock_warehouse` tool. The client:

1. probes `server/discover` and negotiates protocol revision `2026-07-28`;
2. declares the SEP-2663 tasks extension;
3. accepts a task handle from `restock_warehouse` and polls `tasks/get`;
4. returns the completed result to `mcp_warehouse` as a normal tool result.

Send to `mcp_warehouse` (`appName` `mcp_warehouse`, `POST /api/run_sse`):

> Count widgets, restock 12 widgets, then report the final total.

The run's events carry the model's calls and the completed tool results. The
task lifecycle remains inside `McpToolset`, as required by the MCP contract.
`GET /api/ui/capabilities` separately lists the server's MCP Apps bridge
features.
