- **Interrupt ids, typed answers and strict resume** (`adk-graph`): every dynamic pause has a
  stable id (`Interrupt::id`, `interrupt_id`) bound to its thread, step, node and question,
  reported on `InterruptedExecution`, `StreamEvent::Interrupted` and `GraphInterruptPayload`
  and kept in the checkpoint. `NodeOutput::with_response_schema` declares a JSON Schema the
  answer must satisfy; an answer that does not is refused with `GraphError::ResumeRejected`
  (`graph.resume_rejected`) before any node runs. `ExecutionConfig::strict_resume` accepts
  answers only under the id of a pending pause, so a replayed or stale answer cannot answer a
  later pause, as an approval binds to its call's fingerprint.
