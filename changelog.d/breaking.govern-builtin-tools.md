- **Provider-executed built-in tools follow the run's tool policy and budget**
  (`adk-core`, `adk-agent`): `LlmAgent` evaluates every built-in tool (Gemini Google
  Search, URL context, Maps, File Search, code execution and computer use; OpenAI hosted
  web search, file search, code interpreter, image generation, MCP, shell and computer
  use; Anthropic web search) against the kill switch, the tool-call budget and the
  `ToolPolicy` before each model request, through the new `BuiltinToolGovernor`. A
  denied tool is left out of the request; `RequireApproval` attaches it only when a
  fingerprint approval or `ApprovalStore` decision already exists, and otherwise records
  it as pending. A default-deny `DeclarativePolicy` now withholds built-in tools that no
  rule allows — add `allow_read_only()` or a rule naming the tool to keep them. Model
  events of an affected request list the withheld tools under
  `adk.builtin_tools.withheld`. Runs without a policy and without a tool-call limit are
  unchanged.
- **Built-in tool uses count against `RunBudget` and cost** (`adk-core`, `adk-model`,
  `adk-anthropic`): `UsageMetadata` has a new `builtin_tool_uses` field, filled from
  Anthropic `usage.server_tool_use`, OpenAI Responses hosted-tool items and Gemini
  grounding metadata and `executableCode` parts. Each use counts as a tool call, and
  `BudgetTracker::begin_model_call` now refuses a model call once tool calls exceed the
  limit. `PricingCatalog::cost_usd` adds published per-use fees (`BuiltinToolFee`):
  Anthropic and OpenAI web search, OpenAI file search, Gemini Google Search and Maps
  grounding. `adk_anthropic::ServerToolUsage` gains `web_fetch_requests` and
  `code_execution_requests`. Struct literals of `UsageMetadata` or `ServerToolUsage`
  need `..Default::default()`.
