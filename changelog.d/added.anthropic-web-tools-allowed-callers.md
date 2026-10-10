- **`allowed_callers` on the dynamic-filtering web tools** (`adk-anthropic`, `adk-tool`):
  `WebSearchTool20260209::with_allowed_callers`, `WebFetchTool20260209::with_allowed_callers`
  and `WebSearchTool::with_allowed_callers` set the tool's `allowed_callers`. `["direct"]`
  keeps the `_20260209` version but turns dynamic filtering off, which Zero Data Retention
  and models without programmatic tool calling require.
