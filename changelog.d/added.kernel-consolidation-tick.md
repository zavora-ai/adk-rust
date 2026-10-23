- **The consolidation tick** (`adk-kernel`, `adk-harness`, spec 3.2 §7.5, §13 Q1, Q12): with
  `ProactivityPolicy::consolidation` set and a `Consolidator` registered through
  `KernelBuilder::consolidator`, the kernel runs a `Maintenance` process
  (`kernel.consolidation`, no tools) once a partition has new material since the last tick
  (`min_new_sessions` distilled sessions or `min_new_facts` new facts), `min_interval` has
  passed and the partition has been idle for `debounce`, outside quiet hours and under
  `budget_share` of the daily proactive budget and `max_tokens_per_tick`; a bare `schedule`
  is an explicit setting. `admit_tick` is the gate, `TickInputs` and `TickAdmission` its
  input and decision. The tick's spend counts toward the partition's proactive budget, and
  its proposals are agenda items of origin `Consolidation`, which a person approves in every
  mode. `Harness::consolidator(user)` returns the harness's `HarnessConsolidator`.
