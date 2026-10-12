- **`durable_company` example** (`examples/durable_company`): one accounts-payable day run
  end to end against OpenAI or Anthropic on in-memory, SQLite, or PostgreSQL stores — a
  `DurableScheduler` cron firing, a worker process killed mid-run and resumed by another,
  a payment over the policy threshold parked until a quorum of two approves, the payment
  made once through the governed checkout under a daily spend cap, a replicated freeze,
  and per-process signed audit logs that `adk-rust audit verify` accepts.
