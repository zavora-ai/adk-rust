- **Standard Webhooks signing and verification** (`adk-core`, feature `webhooks`):
  `webhook::{WebhookSigner, WebhookVerifier}` sign and verify `webhook-id` /
  `webhook-timestamp` / `webhook-signature` deliveries with a timestamp tolerance, several
  secrets for key rotation, and `WebhookReplayGuard` for replay protection. `adk-agent`
  (`ambient`) adds `StandardWebhookVerifier` for `WebhookTrigger`.
