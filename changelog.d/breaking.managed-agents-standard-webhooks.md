- **Managed Agents webhooks use Standard Webhooks** (`adk-anthropic`): `verify_webhook`
  takes `WebhookHeaders` (`webhook-id`, `webhook-timestamp`, `webhook-signature`) instead
  of one signature string, and verifies the base64 `v1` HMAC-SHA256 over
  `{id}.{timestamp}.{body}` against any of the header's space-separated signatures, with a
  five-minute tolerance in both directions. `WebhookVerifier` sets the tolerance and
  `WebhookHeaders::from_header_map` reads the headers. The previous scheme matched no real
  delivery. `WebhookVerifyError` is `#[non_exhaustive]` and gains `MissingHeader` and
  `TimestampInFuture`.
