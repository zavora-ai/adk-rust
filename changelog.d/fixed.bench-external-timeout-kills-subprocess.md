- **A timed-out external benchmark subprocess is killed** (`adk-bench`):
  `ExternalRunner::run` now ends the subprocess when it returns `ExternalTimeout`.
  It previously left the framework running after reporting the timeout.
