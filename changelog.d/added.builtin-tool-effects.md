- **Built-in tools declare their effect** (`adk-tool`): Gemini Google Search, URL
  context, Maps and File Search, OpenAI web and file search, and Anthropic web search
  declare `ToolEffect::ReadOnly`; code execution, computer use, image generation, shell,
  apply-patch and MCP tools declare `ToolEffect::NonIdempotent`, so
  `DeclarativePolicy::allow_read_only()` and `PolicyRule::with_effect` rules apply to them.
- **`builtin_tool_policy` live scenario** (`examples/autonomy_validation`): a policy that
  denies web search keeps the tool out of OpenAI, Anthropic and Gemini requests; one that
  allows it meters every reported search and adds its fee to the cost.
