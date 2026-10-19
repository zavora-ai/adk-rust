- **Tool-call fingerprints name the agent and the tool's origin** (`adk-core`):
  `ToolConfirmationRequest::fingerprint()` is the acting agent's id, the tool's
  `ToolOrigin`, the tool name, and the canonical arguments, separated by U+001F. A value
  built with `tool_call_fingerprint(name, args)` for `RunConfig::tool_confirmation_fingerprints`
  no longer matches any call, so the call-ID decision it binds is ignored and the call is held
  (the safe direction): take the fingerprint from the request the held call surfaced, or build
  it with `ToolConfirmationRequest::for_tool(tool, args).with_acting_context(..)`. A call held
  under 2.3.0, which records no agent or origin, is answered as not run when a decision resumes
  it; re-issue it.
- **`ToolConfirmationRequest::origin` and `Tool::origin()`** (`adk-core`, `adk-tool`):
  struct literals of `ToolConfirmationRequest` add `origin` (or use
  `ToolConfirmationRequest::new`). `Tool::origin()` defaults to `STATIC_TOOL_ORIGIN`
  (`"static"`); MCP tools report `mcp:{toolset name}`. Tool wrappers should forward it, or
  approvals for the wrapped tool bind to `"static"`.
