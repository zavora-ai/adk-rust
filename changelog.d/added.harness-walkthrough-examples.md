- **`harness_openai` and `harness_anthropic` examples** (`examples/harness_openai`,
  `examples/harness_anthropic`): the `adk-harness` walkthrough of spec 3.1 §5 on OpenAI and
  Anthropic, scripted through `ScriptedLlm` with no key or live with one. Nine scenarios assert on
  files, stores and audit records: Ask-mode approvals and a remembered `ShellPrefix` rule (H2),
  Plan mode that writes nothing, a checkpointed plan graph across a restart with a gate and a
  rewind (H4), a durable goal on SQLite that resumes in a new process and runs each approved call
  once (H1), team members in their own context under the stricter mode (H3), a role map that
  refuses `bash` without `harness:shell` (H6), the spend report and the loop guard (H7), and the
  OS sandbox. Long-term memory (H11) is reported as skipped until `adk-harness` exports
  `FactStore`.
