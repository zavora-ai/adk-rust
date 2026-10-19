- **Functional workflows keep their execution log across repeated resumes**
  (`adk-graph`, `adk-rust-macros`): the pre-execution checkpoint of a resumed
  `#[entrypoint]` run and every interrupt checkpoint carry the execution log. A crash
  during a resumed run previously left a log-less latest checkpoint, so the next resume
  ran every completed task again, such as a payment.
- **Functional interrupts resume through the generated agent** (`adk-graph`,
  `adk-rust-macros`): the generated `invoke` hands `ExecutionConfig::resume_values` to the
  run's `TaskContext`, so a value supplied with `with_resume_value(key, value)` under the
  interrupt's continuation key, such as `interrupt-1`, answers it. `invoke` had no way to
  pass a value, so an interrupted workflow suspended again on every resume.
