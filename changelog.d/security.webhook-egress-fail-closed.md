- **Guarded HTTP clients fail closed when they cannot be built** (`adk-core`, `adk-server`,
  `adk-surface`): `WebhookDelivery` whose public-only client cannot be built dead-letters
  every message with the reason and sends nothing, instead of falling back to a default
  client that skips the egress check, uses system proxies and follows redirects. The AWP
  edge's event delivery client and the phone's push client fail their builders instead
  (`awp.events.client_unavailable`, `surface.phone.push_client`).
