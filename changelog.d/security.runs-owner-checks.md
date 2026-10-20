- **Background runs are scoped to their owner** (`adk-server`, feature `background`): with a
  `RequestContextExtractor`, any authenticated caller could read, cancel, and wake every
  run by id and list every user's runs, and caller-chosen keys were global, so one user's
  `idempotencyKey` returned another user's run and a `concurrencyKey` with
  `onConflict: interrupt` cancelled it. `GET /runs` now lists only the caller's runs;
  `GET /runs/{id}`, `DELETE /runs/{id}`, and `POST /runs/{id}/wake` answer `404` for another
  owner's run; and the caller's `idempotencyKey` and `concurrencyKey` are stored prefixed
  with the owner, so keys never cross users. A key submitted before the upgrade does not
  match the same key submitted after it. Runs enqueued by cron schedules and webhook hooks
  belong to their service user and are no longer listed to other callers. Without an
  extractor the endpoints behave as before and the server logs a warning once (spec 3.1
  §8.2, §12 phase 0).
