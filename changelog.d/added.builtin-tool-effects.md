- **Provider-executed built-in tools follow the run's tool policy and budget**
  (`adk-core`, `adk-agent`): before each model request, `LlmAgent` evaluates every built-in
  tool (Gemini Google Search, URL context, Maps, File Search, code execution and computer use;
  OpenAI hosted web search, file search, code interpreter, image generation, MCP, shell and
  computer use; Anthropic web search) against the kill switch, the tool-call budget and the
  `ToolPolicy`, through `BuiltinToolGovernor`. A denied tool is left out of the request;
  `RequireApproval` attaches it only when a fingerprint approval or `ApprovalStore` decision
  already exists, and otherwise records it as pending. A default-deny `DeclarativePolicy`
  withholds built-in tools that no rule allows. Model events of an affected request list the
  withheld tools under `adk.builtin_tools.withheld` (`BUILTIN_TOOLS_WITHHELD_KEY`). Runs
  without a policy and without a tool-call limit send built-in tools as before.
- **Built-in tool uses are metered and priced** (`adk-core`, `adk-model`, `adk-anthropic`):
  `UsageMetadata::builtin_tool_uses` is filled from Anthropic `usage.server_tool_use`, OpenAI
  Responses hosted-tool items, and Gemini grounding metadata and `executableCode` parts. Each
  use counts as a tool call, and `BudgetTracker::begin_model_call` refuses the next model call
  once tool calls pass the limit. `PricingCatalog::cost_usd` adds published per-use and
  per-response fees (`BuiltinToolFee`): Anthropic and OpenAI web search, OpenAI file search,
  and Gemini Google Search and Maps grounding.
- **Built-in tools declare their effect** (`adk-tool`): Gemini Google Search, URL
  context, Maps and File Search, OpenAI web and file search, and Anthropic web search
  declare `ToolEffect::ReadOnly`; code execution, code interpreter, computer use, image
  generation, shell, apply-patch and OpenAI hosted MCP tools declare
  `ToolEffect::NonIdempotent`, so
  `DeclarativePolicy::allow_read_only()` and `PolicyRule::with_effect` rules apply to them.
