- **Signed outbound webhook delivery** (`adk-core`, feature `webhook-delivery`):
  `webhook_delivery::WebhookDelivery` sends Standard Webhooks-signed POSTs with exponential
  backoff and full jitter, honours `Retry-After`, dead-letters to a `DeadLetterSink` after the
  last attempt, never follows redirects, and through an `adk_core::egress::EgressPolicy`
  checked at DNS resolution refuses plain-HTTP, loopback, private, link-local and cloud
  metadata destinations (`EgressPolicy::allow_private_networks` allows all but metadata for
  in-cluster receivers).
