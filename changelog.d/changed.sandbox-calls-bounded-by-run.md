- **Sandbox calls end with the run** (`adk-sandbox`, `adk-harness`): `SandboxTool` refuses a
  call while the run is frozen, ends a running call when a freeze arrives, and cuts its
  timeout to the wall time left in the run's budget. The container shell caps each call at
  the same remaining wall time, stops the session's container when a call is cancelled
  mid-command, and stops it when the container reports a command timed out, as it already did
  for a timeout it measured itself. Its `harness.bash.begin` records add `provider` and
  `environment`.
