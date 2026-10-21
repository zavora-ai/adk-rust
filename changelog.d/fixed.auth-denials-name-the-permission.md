- **Scope and RBAC denials name the missing permission** (`adk-auth`, `adk-core`): a call
  `ScopeGuard` or `ProtectedTool` refuses fails with component `Tool`, category `Forbidden`,
  and the new code `tool.permission_denied` (`TOOL_PERMISSION_DENIED_CODE`) instead of
  `tool.internal (tool.legacy)`, with `missingScopes` and `requiredScopes`, or `permission`, in
  its details. `tool_error_response` always shows that code's message, so `LlmAgent` tells the
  model the call was refused and which scope or permission it needed. `ScopeDenied` converts
  into `AdkError`.
