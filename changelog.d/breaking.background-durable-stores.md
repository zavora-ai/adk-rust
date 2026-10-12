- **Background runs and cron jobs run on the durable stores** (`adk-server`, feature
  `background`): the process-local run and cron stores are removed, because the file store
  failed in-flight runs on restart and could not fence across processes; the in-memory
  `DurableRunStore` keeps single-process deployments working with the same REST surface.
  - Removed: `background::{RunStore, BackgroundRun, BackgroundRunner, RunPersistence,
    FileRunPersistence, PersistedRun, RunRetention}`, `background::cron::{CronJobStore,
    CronJob, DEFAULT_MAX_QUEUE_DEPTH}`, `CronState::with_max_queue_depth`, and the
    `BackgroundState::{store, runner}` fields. `CronState::cron_store` is now `schedules`
    (an `Arc<dyn ScheduleStore>`). Finished runs are pruned after
    `DurableWorkerConfig::retain_finished`.
  - `validate_cron_expression` returns `Result<(), String>`. `SubmitRunRequest::timeout_secs`
    is removed; use `DurableWorkerConfig::run_timeout`.
  - `RunStatus` gains `Waiting` and is `#[non_exhaustive]`. `RunStatusResponse` reports
    `target`, `attempt`, `maxAttempts`, `cancelRequested`, `sessionId`, and `checkpoint`.
    `CronJobResponse` uses the job name as `jobId` and reports `timezone`, `spec`,
    `missedFirePolicy`, `nextFireAt`, and `revision`; `createdAt`, `lastExecution`,
    `executionCount`, and `activeRunCount` are removed. Creating a job with an existing name
    returns `409`.
  - `DELETE /runs/{id}` on a running run returns it `running` with `cancelRequested: true`
    until its worker stops it at the next heartbeat.
  - `GovernanceController::{with_background_runner, with_cron_store}` are replaced by
    `with_run_store`; freeze and unfreeze responses add `mirrored` and `stale`, and a freeze
    adds `cancelledRuns`.
- **`RunnerConfig` gains `durable_runs`** (`adk-runner`): struct literals need the field
  (`None` keeps the previous behaviour).
- **`adk-core` enables tokio's `macros` feature** for the kill-switch mirror loop.
