- **Background runs and cron jobs run on the durable stores** (`adk-server`, feature
  `background`): the process-local run and cron stores are removed, because the file store
  failed in-flight runs on restart and could not fence across processes.
  `BackgroundState::new()` keeps runs in an in-memory `DurableRunStore` and
  `CronState::new` keeps jobs in an in-memory `ScheduleStore`; `with_run_store` and
  `with_schedule_store` take shared ones.
  - Removed: `background::{RunStore, BackgroundRun, BackgroundRunner, RunPersistence,
    FileRunPersistence, PersistedRun, RunRetention}`, `background::cron::{CronJobStore,
    CronJob, DEFAULT_MAX_QUEUE_DEPTH}`, `CronState::with_max_queue_depth`, and the
    `BackgroundState::{store, runner}` fields. `CronState::cron_store` is now `schedules`
    (an `Arc<dyn ScheduleStore>`). Finished runs are pruned after
    `DurableWorkerConfig::retain_finished` (24 hours by default).
  - `validate_cron_expression` returns `Result<(), String>`. `SubmitRunRequest::timeout_secs`
    is removed; use `DurableWorkerConfig::run_timeout`. `SubmitRunRequest::workflow_id` is an
    optional alias of the new `target`, and `input` is a `serde_json::Value`.
    `SubmitRunRequest` gains `session_id`, `user_id`, `idempotency_key`, `concurrency_key`,
    `on_conflict`, and `not_before`, and `CreateCronJobRequest` gains `timezone` and
    `missed_fire_policy`; struct literals of either need the new fields.
  - `RunStatus` gains `Waiting` and is `#[non_exhaustive]`. `RunStatusResponse` reports
    `target`, `attempt`, `maxAttempts`, `cancelRequested`, `sessionId`, and `checkpoint`, and
    always reports `retryCount` and `retriesRemaining`. `CronJobResponse` uses the job name as
    `jobId` and reports `timezone`, `spec`, `missedFirePolicy`, `nextFireAt`, and `revision`;
    `createdAt`, `lastExecution`, `executionCount`, and `activeRunCount` are removed. Creating
    a job with an existing name returns `409`.
  - `POST /runs` refuses a target that no registered agent or workflow provides with `404`,
    also when no executor is set. `DELETE /runs/{id}` on a running run returns it `running`
    with `cancelRequested: true` until its worker stops it at the next heartbeat.
