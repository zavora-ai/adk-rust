- **`kernel` feature of `adk-harness`** (spec 3.2 §10 HK1, HK3): `Harness::kernel` returns
  an `adk_kernel::KernelBuilder` hosting the harness's runner as the app's target and as
  `harness.goal`, with its sessions, approval service, kill switch, audit, spend ledger and
  run store, and a `ProgressSource` for the process view. A decision through the harness's
  approval service wakes the process asleep on it.
  - Goal threads keep the last `2 × GOAL_STALL_WINDOW` checkpoints. Under a kernel, a goal
    whose check repeats its last three results, or whose iteration made no tool call,
    records `harness.goal.stalled` and parks `Waiting` instead of spending its cap.
