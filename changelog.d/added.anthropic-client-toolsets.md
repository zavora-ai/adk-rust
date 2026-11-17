- **Computer use and browser use on current Claude models** (`adk-anthropic`, `adk-model`):
  `ToolUnionParam` declares `computer_toolset_20260801` and `browser_toolset_20260801`, beside the
  earlier `computer_20251124` and `computer_20250124` with their beta headers, which the client
  adds. `ToolUseBlock` and `ToolResultBlock` carry `toolset_name`, and tool results take the
  `browser_state` block (`BrowserStateBlock`). `ComputerToolVersion::for_model` and
  `supports_browser_toolset` say what each model accepts. `AnthropicClient` sends each model the
  computer use version it accepts — Claude Sonnet 5.5, Opus 5.5, Haiku 5.5 and Fable 5.2 reject
  `computer_20251124` — and hands member calls to the agent as function calls named
  `computer.<member>` and `browser.<member>`. Tools that declare
  `x-adk-anthropic-toolset-member` collapse into one toolset entry offering exactly their members.
