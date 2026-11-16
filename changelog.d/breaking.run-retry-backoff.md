- **Retryable run failures back off** (`adk-core`, `adk-session`, `adk-server`,
  `adk-managed`): `RunOutcome::Failed` gains `backoff: Option<Duration>`, and every run
  store makes a requeued run claimable only once it has passed. After a model provider's
  retryable failure, or one that names a wait, `DurableWorker` waits
  `DurableWorkerConfig::retry_backoff` (`RetryBackoff`: about 5, 10 and 20 seconds,
  jittered, at least the error's `Retry-After`, at most 10 minutes) instead of retrying
  at once; other retryable failures still retry at once. The last attempt's failure
  reads `attempts exhausted (N of N)`. Construct `RunOutcome::Failed` with
  `backoff: None` to keep the old behaviour.
