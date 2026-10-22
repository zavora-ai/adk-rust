- **A freeze parks running durable runs instead of failing them** (`adk-server`, `adk-agent`;
  spec 3.2 §10 S3): a `DurableWorker` whose governance control refuses a run after it made a
  model or tool call now parks it `Waiting` on its `Session` checkpoint at its next model-call
  boundary, reported to `WorkerHooks::on_park` as `ParkReason::Frozen`, and wakes exactly those
  runs when the control allows work again (`DurableWorker::wake_frozen`,
  `WorkerHandle::wake_frozen`, `frozen_runs`); a run suspended by request stays parked.
  `DurableWorkerConfig::on_governance_refusal` selects it: `GovernanceRefusal::Park`, the
  default, or `Fail` for the previous outcome; a worker without a governance control keeps the
  previous outcome. `LlmAgent` answers a call the kill switch refuses inside a tool batch, and
  each later call of the batch, `not run` instead of ending the run mid-batch, and ends the run
  with `governance.frozen` at its next model call, so the session holds every response before
  the park. `request_cancel` still finishes a parked run `Cancelled`, and a freeze-parked run
  has no expiry. `run_store_conformance_tests!` gains `freeze_parked_runs_wake_only_when_woken`.
- **A durable worker sleeps until its earliest deadline** (`adk-server`; spec 3.2 §7.9, §13 Q2):
  when nothing is claimable, the claim loop waits until the earliest future `not_before` of a
  queued or parked run on its queue when that comes before `poll_interval`, so a delayed run or
  a timed wait resumes at its time rather than at the next poll.
