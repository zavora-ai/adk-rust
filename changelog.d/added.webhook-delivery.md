- **Signed outbound webhook delivery** (`adk-core`, feature `webhook-delivery`):
  `webhook_delivery::WebhookDelivery` sends Standard Webhooks-signed POSTs with exponential
  backoff and full jitter, honours `Retry-After`, dead-letters to a `DeadLetterSink` after the
  last attempt, never follows redirects, and through an `EgressPolicy` checked at DNS
  resolution refuses plain-HTTP, loopback, private, and link-local destinations
  (`EgressPolicy::allow_private_networks` allows them for in-cluster receivers).
