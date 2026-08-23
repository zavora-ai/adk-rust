# MCP 2026-07-28 discovery and tasks

The bundled `advanced-mcp-server` publishes an immediate `count_stock` tool and
a slow `restock_warehouse` tool. The client:

1. probes `server/discover` and negotiates protocol revision `2026-07-28`;
2. declares the SEP-2663 tasks extension;
3. accepts a task handle from `restock_warehouse` and polls `tasks/get`;
4. returns the completed result to `mcp_warehouse` as a normal tool result.

Select `mcp_warehouse` and send:

> Count widgets, restock 12 widgets, then report the final total.

The transcript exposes the model's calls and completed tool results. The task
lifecycle remains inside `McpToolset`, as required by the MCP contract. The
Protocols tab separately lists the Runtime's MCP Apps bridge features.
