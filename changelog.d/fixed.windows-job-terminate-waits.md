- **A killed Windows process tree has exited before the call returns** (`adk-sandbox`,
  `adk-devtools`, `adk-tool`): `TerminateJobObject` only starts termination, so a process
  a job ended could still complete a write it had in flight after `bash` returned from a
  timeout or a dropped call. `JobObject::active_processes`, `terminate_and_wait` and
  `terminate_and_wait_async` wait, for at most `JobObject::TERMINATION_WAIT` (2 s), until
  every process in the job has exited: each is held open from before the termination and
  waited on, since a terminated process leaves the job's active count before it exits. `bash`'s timeout, shell-exit and drop paths, `McpServerManager`'s stdio
  transport and `WindowsEnforcer::run_confined` use them, and log a warning when a process
  outlasts the wait. Spec 3.2 §10 W1, W5.
