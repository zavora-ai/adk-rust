- **AWP event subscriptions deliver over HTTP and refuse internal callback URLs**
  (`adk-awp`): `InMemoryEventSubscriptionService` sends each matching event through
  `WebhookDelivery` (Standard Webhooks signatures keyed with the subscription secret,
  retries, dead-lettering) instead of logging it. `deliver` returns at once,
  `deliver_and_wait` returns each report, and `with_delivery` configures the sender.
  `EventSubscription::validate`, and with it `create` and `POST /awp/events/subscribe`,
  refuses a callback URL that names a loopback, private, or link-local address, which 2.3.0
  accepted; `validate_with` checks against a given `EgressPolicy`.
