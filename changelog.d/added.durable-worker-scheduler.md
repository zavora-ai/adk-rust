- **Durable background runs, worker, and scheduler** (`adk-server`, feature `background`):
  `BackgroundState` runs on a `DurableRunStore` (in memory by default, `with_run_store` for a
  shared backend) with agent targets (`with_agent`) and workflow targets. `DurableWorker`
  claims runs under fenced leases, heartbeats at `lease_ttl / 3`, records the checkpoint
  before any progress, stops on a cancellation request or a stale epoch, and fences agent
  model and tool calls behind the lease. A run recovered after its lease expired resumes:
  agents continue the same session with unanswered calls answered from the action ledger,
  graph workflows resume from their checkpointer through `WorkflowRunContext`
  (`WorkflowRegistry::register_durable`, `WorkflowExecutor::execute_durable`).
  `DurableScheduler` and `start_cron_scheduler` enqueue each fire time once across any
  number of replicas and fire nothing while the kill switch refuses work. `POST /runs` takes
  `target`, `sessionId`, `idempotencyKey`, `concurrencyKey`, `onConflict` (double texting),
  and `notBefore`; `GET /runs` lists runs.
- **Durable cancellation and recovery on the runner** (`adk-runner`):
  `RunnerConfig::durable_runs` and `RunnerConfigBuilder::durable_run_store` make
  `Runner::interrupt` also request cancellation of the session's durable runs on every
  replica; `Runner::cancel_durable_runs` awaits the same request; and
  `Runner::recover_unanswered_calls` answers function calls a stopped process left
  unanswered from the action ledger (`already_succeeded`, `already_failed`, or
  `outcome_unknown`) without executing them.
- **Parking on held tool calls** (`adk-server`, `adk-core`): an agent run that ends holding a
  tool call for approval is parked with `DurableRunStore::wait` under the new
  `RunCheckpointRef::AwaitingApproval` checkpoint (held fingerprints and expiry,
  `DurableWorkerConfig::approval_timeout`) instead of finishing. `BackgroundState::wake_run`,
  `BackgroundState::wake_session_runs`, and `POST /runs/{run_id}/wake` resume it on the same
  session, where the held call executes with its decision.
