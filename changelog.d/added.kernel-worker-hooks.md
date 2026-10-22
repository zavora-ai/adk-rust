- **Durable worker lifecycle hooks and suspend path** (`adk-server`, spec 3.2 §10 S1):
  `DurableWorker::with_hooks` registers `WorkerHooks` whose `on_claim`, `on_park` (with a
  `ParkReason`) and `on_finish` run after each store write the worker makes.
  `DurableWorker::suspend` and `WorkerHandle::suspend` stop an agent run at its next
  model-call boundary and park it `Waiting` on its `Session` checkpoint until woken; the
  claim after a wake resumes the session on any worker without executing a completed call
  again, and `request_cancel` still finishes the run `Cancelled`.
- **Run store conformance case for parked runs** (`adk-core`): `run_store_conformance_tests!`
  gains `parked_run_keeps_its_checkpoint`, the store sequence the suspend path relies on,
  checked on every backend.
