- **A timed-out external benchmark subprocess is killed** (`adk-bench`):
  `ExternalRunner::run` ends the subprocess when it returns `BenchError::ExternalTimeout`
  instead of leaving the framework running.
