- **A2A push notifications check the address they connect to** (`adk-server`, feature
  `a2a-v1`): `HttpPushNotificationSender::new` sends through an `adk_core::egress::GuardedClient`,
  so a webhook host that resolves to a loopback, private, metadata or other non-public address
  is refused when the sender connects, not only a literal in the registered URL; it ignores
  proxy settings, and a client that cannot be built fails every delivery instead of panicking.
  Refs #1281.
