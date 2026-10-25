- **A run a freeze parked resumes with a system notice** (`adk-server`, `adk-core`,
  `adk-runner`, `adk-agent`): the claim after the unfreeze adds `FREEZE_RESUMED_NOTICE` to the
  session, authored `system`, so the model's next request says that execution resumed, that
  the calls refused during the freeze did not run, and to continue the task and call again
  any refused call it still needs. A woken run previously continued with an empty user turn
  after the refused call's `not run` answer, and models ended the task there. The park leaves
  `FREEZE_PAUSED_NOTICE`, the resume writes a `governance.run_resumed` audit record, and
  `adk_core::history::system_notice_event` builds notices that every agent's history keeps as
  user-role turns. A run no freeze parked resumes without a notice.
