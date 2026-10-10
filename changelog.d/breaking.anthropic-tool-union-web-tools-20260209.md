- **`ToolUnionParam` and `ContentBlock` gain variants** (`adk-anthropic`): `ToolUnionParam`
  adds `WebSearch20260209` and `WebFetch20260209`; `ContentBlock` adds
  `CodeExecutionToolResult`, `BashCodeExecutionToolResult` and
  `TextEditorCodeExecutionToolResult`. Neither enum is `#[non_exhaustive]`, so an exhaustive
  `match` on either needs the new arms.
