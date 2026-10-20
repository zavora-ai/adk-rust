- **RBAC and scope wrappers forward tool annotations** (`adk-auth`): `ProtectedTool`,
  `ProtectedToolDyn`, `ScopedTool`, and `ScopedToolDyn` forwarded the wrapped tool's
  origin and effect but not `annotations()`, so a wrapped MCP tool lost its
  `readOnlyHint`/`destructiveHint` hints and its `ToolOrigin::declaration_digest` differed
  from the bare tool's, and an approval recorded against the bare tool did not match the
  wrapped one. All four now forward `annotations()` (spec 3.1 §10 F2).
