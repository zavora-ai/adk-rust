- **`ReadonlyContext::is_authenticated`** (`adk-core`, `adk-runner`): a provided method,
  `false` by default, that says whether `user_id()` comes from an authenticated
  `RequestContext` rather than from the request body. The runner's `InvocationContext`
  answers `true` when the run carries a `RequestContext` (which `adk-server` sets only from
  a request its `RequestContextExtractor` authenticated), and `ToolCallbackContext`
  forwards it, so an agent can keep per-user privileges such as standing approvals inert on
  an unauthenticated server (spec 3.1 §10 F5).
