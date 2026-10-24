- **`RunConfig` and `RunnerConfig` gain public fields** (`adk-core`, `adk-runner`): a
  `RunConfig` struct literal that names every field must add `invocation_hooks`, `budget`,
  `budget_tracker`, `action_ledger`, `tool_policy`, `governance`, `tool_approvals`,
  `approval_store`, `approval_service`, `durable_run_id`, `tool_confirmation_timeout`,
  `spend_ledger`, `llm_spend_estimate`, `audit`, `acting`, and
  `deny_on_confirmation_timeout`, or end with
  `..Default::default()`; `RunConfig::builder()` is unaffected. A `RunnerConfig` struct
  literal must set `tool_policy`, `governance`, `action_ledger`, `approval_store`,
  `spend_ledger`, `durable_runs`, `audit`, `identity_source`, and `subject_delegation`
  (`None` keeps the 2.3.0 behaviour), or build the runner with `Runner::builder()`.
