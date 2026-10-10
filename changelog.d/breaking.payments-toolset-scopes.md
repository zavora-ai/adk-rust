- **Payment tools enforce their declared scopes** (`adk-payments`):
  `PaymentToolsetBuilder::build` wraps every tool in an `adk-auth` `ScopeGuard`, so a call
  fails unless the caller holds the tool's scope, such as `payments:checkout:complete`.
  The tools were documented as scope-protected but ran for any caller. The default guard
  reads `ToolContext::user_scopes()`; `with_scope_guard` supplies another resolver or an
  audit sink.
- **Payment tools report the caller's identity** (`adk-payments`): each tool records the
  calling agent as the acting `CommerceActor` and passes the caller's session identity to
  the commerce kernel, instead of the fixed actor `agent-tool` and no session. Status
  lookups are bound to the caller's session.
