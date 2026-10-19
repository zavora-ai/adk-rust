- **`autonomy_validation` example** (`examples/autonomy_validation`): live scenarios against
  OpenAI (Responses and Chat Completions), Anthropic, and Gemini that assert on session history,
  tool execution counts, persisted state, checkpoints, and usage rather than model prose:
  transfers, runner plugin denials, fail-closed path guardrails and eval judges, a payment
  timeout answered `outcome_unknown`, run budgets, default-deny policies, approvals across runs
  and through the approval service, the kill switch, a daily spend cap, AP2 governance,
  built-in tool policies, durable worker crash recovery, the signed audit log, agent identity
  across two deployments, and webhook wake-ups.
