- **`FreezeDelegate` for the governance admin endpoints** (`adk-server`; spec 3.2 §10 S4):
  `ServerBuilder::with_freeze_delegate` and `GovernanceController::with_freeze_delegate` hand
  `POST /api/admin/freeze` and `POST /api/admin/unfreeze` to a delegate, as a kernel registers
  itself, so a freeze parks the runs it supervises instead of cancelling the background run
  stores' runs. The responses report `parkedRuns` and `cancelledRuns` separately, and
  `GET /api/admin/governance` reports `runningRuns` (`park` under a delegate, `cancel`
  otherwise). A server without a delegate keeps the cancel, a terminate rather than a pause.
