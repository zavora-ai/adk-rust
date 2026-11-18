- **Client toolset fields and variants** (`adk-anthropic`): `ToolUseBlock` and `ToolResultBlock`
  gain a public `toolset_name` field, so struct literals add `toolset_name: None` or use `new`.
  `ToolUnionParam` gains `ComputerToolset20260801`, `BrowserToolset20260801`, `Computer20251124`
  and `Computer20250124`, and the tool-result `Content` gains `BrowserState`, so exhaustive matches
  need the new arms. `ServerTool` becomes `#[non_exhaustive]` and gains `ComputerUse20251124`,
  `ComputerToolset20260801` and `BrowserToolset20260801`.
