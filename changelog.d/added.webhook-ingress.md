- **Webhook wake-ups** (`adk-server`, feature `webhook-ingress`):
  `ServerBuilder::with_webhook_ingress` serves `POST /hooks/{name}` for each `WebhookHook` of
  a `WebhookIngress`. A verified Standard Webhooks delivery maps to a `NewRun` (target, input
  template, queue, user, session, concurrency key and `OnConflict`) and is enqueued in a
  `DurableRunStore` with idempotency key `hook:{name}:{webhook-id}`: `202` for a new run,
  `200` with the same run for a redelivery, `401` for a bad signature, `409` when
  `OnConflict::Reject` blocks it, `429` past `max_pending`, and `503` while the kill switch
  refuses work.
