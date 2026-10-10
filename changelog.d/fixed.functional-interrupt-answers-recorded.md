- **Functional interrupt answers survive a later crash** (`adk-graph`): the first answer
  an interrupt receives is recorded in the execution log under its continuation key and
  checkpointed before `TaskContext::interrupt` returns, so a run resumed from any later
  checkpoint replays it without the value being supplied again. A recorded answer takes
  precedence over a different value supplied later. `ExecutionLog::record_interrupt_answer`
  and `ExecutionLog::interrupt_answer` expose the record.
