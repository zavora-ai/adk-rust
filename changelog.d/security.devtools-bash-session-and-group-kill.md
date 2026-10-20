- **`bash` leaves no background processes and cannot reach the terminal** (`adk-devtools`):
  the command inherited the agent's standard input and controlling terminal, and the
  process-group kill applied only to a timeout or a cancelled call, so `sleep 300 &` or a
  server started with its output redirected kept running after the call returned, and a
  background process holding the output open made the call wait for the timeout. The command
  now reads `/dev/null`, starts in a new session (`setsid`) with no controlling terminal, and
  the rest of its process group is killed once the shell exits. A process that starts its own
  session or group escapes the kill. `Workspace::bash_kill_group_on_exit(false)` keeps
  background processes running. Spec 3.1 §10 D3.
