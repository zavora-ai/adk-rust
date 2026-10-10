- **A tool-created checkout's transaction ID derives from the idempotency key**
  (`adk-payments`): `payments_checkout_create` named the transaction after the clock, so
  a replayed call opened a second one. A replay now names the same transaction.
