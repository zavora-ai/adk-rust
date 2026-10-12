- **Signed outbound webhook delivery** (`adk-core`, feature `webhook-delivery`):
  `webhook_delivery::WebhookDelivery` sends Standard Webhooks-signed POSTs with exponential
  backoff and full jitter, honours `Retry-After`, dead-letters to a `DeadLetterSink` after the
  last attempt, and refuses loopback, private, and link-local destinations through an
  `EgressPolicy` checked at DNS resolution.
