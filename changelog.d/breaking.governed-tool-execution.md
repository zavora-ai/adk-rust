- **Tool calls are authorized after argument rewrites** (`adk-agent`): `LlmAgent` screened
  guardrails and resolved confirmation before enhanced plugins ran, so a plugin could change
  a call's arguments after they were approved. Enhanced plugins, runner hooks, and
  `BeforeToolCallback`s now run first; the policy, guardrails, and confirmation see the
  arguments that execute, and the `ToolConfirmationRequest` carries them. `CodeActAgent`
  follows the same order, and also honours `InvocationContext::requires_tool_confirmation`.
- **A held call is answered and its siblings run** (`adk-agent`): a call awaiting
  confirmation with no handler now receives `{"error": "Tool '<name>' requires
  confirmation"}` and the run ends after the turn's other calls finish, instead of ending
  before any call in the turn runs. The confirmation event is unchanged.
- **Confirmation handlers are bounded and concurrent** (`adk-core`, `adk-agent`):
  `ToolConfirmationHandler::decide` is no longer serialized across a batch, and a decision
  that takes longer than `RunConfig::tool_confirmation_timeout` (default five minutes)
  denies the call. A handler that prompts on a shared device serializes its own prompts.
- **New public fields** (`adk-core`, `adk-runner`): `RunConfig` gains `tool_policy`,
  `governance`, `tool_approvals`, `approval_store`, and `tool_confirmation_timeout`;
  `RunnerConfig` gains `tool_policy` and `governance`. Struct literals without
  `..Default::default()` must add them.
