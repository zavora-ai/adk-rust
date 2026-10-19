- **Usage records count built-in tool uses** (`adk-core`, `adk-anthropic`): `UsageMetadata`
  gains `builtin_tool_uses` (`Vec<BuiltinToolUse>`), and `adk_anthropic::ServerToolUsage`
  gains `web_fetch_requests` and `code_execution_requests` and now implements `Default`.
  Struct literals of either type add the new fields or end with `..Default::default()`.
