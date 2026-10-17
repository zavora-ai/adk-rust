- **Tool-call fingerprints name the agent and the tool's origin** (`adk-core`):
  `ToolConfirmationRequest::fingerprint()` is the acting agent's id, the tool's
  `ToolOrigin`, the tool name, and the canonical arguments, separated by U+001F. A key built
  with `tool_call_fingerprint(name, args)` for `RunConfig::tool_approvals` or
  `RunConfig::tool_confirmation_fingerprints` no longer matches any call, so the call is held
  (the safe direction): take the fingerprint from the request the held call surfaced, or build
  it with `ToolConfirmationRequest::for_tool(tool, args).with_acting_context(..)`. Decisions
  recorded under a fingerprint before the upgrade do not apply, and a call held before the
  upgrade, which records no origin, is answered as not run when it resumes; re-issue it.
  `ToolConfirmationRequest` gains `origin: Option<ToolOrigin>` and `GovernedCall` gains
  `origin` and `resumes`, so struct literals of either need the new fields. `Tool` gains
  `origin()`, defaulting to `STATIC_TOOL_ORIGIN` (`"static"`); MCP tools report
  `mcp:{toolset name}`, and tool wrappers should forward it. `adk-core` now depends on `sha2`
  unconditionally.
