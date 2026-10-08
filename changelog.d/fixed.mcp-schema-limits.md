- **MCP tool schemas are bounded at discovery** (`adk-tool`): `McpToolset` measures
  each tool's input and output schema against `McpSchemaLimits` (256 KiB and 10 000
  JSON values by default) before copying or logging it. A tool over a limit is skipped
  with a warning naming the toolset and the tool, and discovery continues with the
  remaining tools. Accepted tools log `schema.bytes` and `schema.nodes` at `debug`
  instead of the full schema. `McpToolset::with_schema_limits` and
  `McpServerManager::with_schema_limits` configure the limits, and managed toolsets
  are named after their server ID.
