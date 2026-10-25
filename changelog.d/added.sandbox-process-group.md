- **`adk_sandbox::process_tree::ProcessGroup`** (`adk-sandbox`, feature `process-tree`; Unix):
  the Unix counterpart of `JobObject` (spec 3.2 §7.7, K-I5). `ProcessGroup::new_session`
  starts a command as the leader of a new session and process group, `contain` holds the
  spawned leader, and `terminate_and_wait` sends the group `SIGTERM`, then `SIGKILL`, and
  returns once every process in it has exited. `adk-tool` depends on `adk-sandbox` on Unix
  for it, as it does on Windows.
