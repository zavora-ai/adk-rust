- **Background runs name their open mode** (`adk-server`, feature `background`): without a
  `RequestContextExtractor`, `ServerBuilder::build` logs a warning naming `/api/runs` and
  `/api/cron` when it mounts them, and so does building a standalone background or cron
  router, in place of a warning on the first request. In open mode an `idempotencyKey` is
  scoped to the run's user — the request's `userId`, or `anonymous` — and stored apart from
  authenticated callers' keys and cron firings' keys, so two users' identical keys create
  two runs and a client's key cannot take a cron firing. An open-mode key submitted before
  the upgrade does not match the same key submitted after it. `concurrencyKey` stays shared
  in open mode. The module rustdoc and the Server API page describe open mode and the
  production setup.
