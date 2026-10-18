- **MCP tool descriptions are bounded** (`adk-tool`): `McpSchemaLimits` gains
  `max_description_bytes` (default 8 KiB, `with_max_description_bytes`); a longer
  server-supplied description is cut at a character boundary with a `warn!`, and the
  tool is kept. A 1 MB description reached every model request whole.
