- **Functional workflows keep their execution log across repeated resumes**
  (`adk-graph`, `adk-rust-macros`): the pre-execution checkpoint of a resumed
  `#[entrypoint]` run and every interrupt checkpoint carry the execution log. A crash
  during a resumed run previously left a log-less latest checkpoint, so the next resume
  ran every completed task again, such as a payment.
- **Functional interrupts resume through the generated agent** (`adk-graph`,
  `adk-rust-macros`): `ExecutionConfig::with_resume_value(continuation_key, value)`
  supplies an interrupt's value, and the generated `invoke` hands it to the run's
  `TaskContext`. `invoke` previously dropped resume values, so an interrupted workflow
  suspended again on every resume.
