- **AWP event subscriptions deliver over HTTP** (`adk-awp`): `InMemoryEventSubscriptionService`
  now sends each matching event through `WebhookDelivery` (Standard Webhooks signatures keyed
  with the subscription secret, retries, dead-lettering) instead of logging it. `deliver`
  returns immediately; `deliver_and_wait` returns each report; `with_delivery` configures the
  sender. Subscriptions whose callback URL names a loopback, private, or link-local address are
  rejected at creation.
