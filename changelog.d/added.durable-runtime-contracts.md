- **Durable runtime contracts** (`adk-core`): `LeaseStore` (leases with fencing epochs),
  `DurableRunStore` (run queue with idempotent enqueue, `OnConflict` double-texting policies,
  epoch-fenced claim, heartbeat, checkpoint, wait/wake, retries, cancellation, and prune),
  `ScheduleStore` (cron, one-shot, and interval schedules with a revision compare-and-swap),
  and `GovernanceStateStore` (freeze state per organisation, tenant, and agent with an epoch
  compare-and-swap), each with an in-memory implementation that takes a `Clock` and a
  conformance macro (`lease_store_conformance_tests!`, `run_store_conformance_tests!`,
  `schedule_store_conformance_tests!`, `governance_state_store_conformance_tests!`).
  `schedule::plan_fires` applies `MissedFirePolicy`, and each fire time enqueues a run keyed
  `schedule:{id}:{fire_time}`; cron specs in IANA time zones need the new `cron` feature.
- **Agent identity types** (`adk-core`): the `principal` module adds `AgentId`, `SubjectKind`,
  `SubjectRef`, `AgentPrincipal`, and `ActingContext`.
