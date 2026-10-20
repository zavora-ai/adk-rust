- **Tool policies see the tool's origin and the call's ID** (`adk-core`):
  `ToolPolicy::evaluate_call(request, call)` is a provided method that receives a
  `PolicyCall` — the called tool's `ToolOrigin` (publisher and declaration digest) and the
  model's call ID — beside the `ToolPolicyRequest`. The governed path calls it for every
  evaluation: the call as made, a guardrail rewrite, arguments an approver edited, and
  built-in tools before each model request (origin only). The default returns
  `evaluate(request)`, so existing policies, including `DeclarativePolicy`, decide as
  before. See `docs/official_docs/security/tool-authorization.md`.
- **Governed audit records name the tool's origin** (`adk-core`): records the governed path
  writes for a call whose tool is known carry `toolSource` and `declarationDigest` in `ext`.
