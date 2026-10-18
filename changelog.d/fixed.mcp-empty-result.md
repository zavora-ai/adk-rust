- **A successful MCP result with no content is an empty output** (`adk-tool`): it
  reaches the model as `{"output": ""}` instead of the error "MCP tool returned no
  content", which invited a retry of a tool that had already run.
