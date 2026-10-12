- **Approval webhooks share the Standard Webhooks implementation** (`adk-server`, feature
  `approvals`): `WebhookApprovalNotifier` signs and sends through
  `adk_core::webhook_delivery::WebhookDelivery`, so notifications are retried with backoff and
  jitter in the background, dead-lettered after the last attempt, and refused for loopback,
  private, and link-local destinations. `with_delivery` sets the retry, dead-letter, and egress
  policy; `EgressPolicy::allow_private_networks()` restores delivery to in-cluster receivers.
