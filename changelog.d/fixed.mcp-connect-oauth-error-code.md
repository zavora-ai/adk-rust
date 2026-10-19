- **`McpHttpClientBuilder::connect` keeps the OAuth error code** (`adk-tool`): a token that
  could not be fetched at connect time was rewrapped as `tool.legacy` (`Internal`), so a
  caller could not tell rejected client credentials from an unreachable server. `connect()`
  and `connect_with_elicitation()` now return the transport's error unchanged:
  `mcp.oauth.token_fetch` with `ErrorCategory::Unauthorized`.
