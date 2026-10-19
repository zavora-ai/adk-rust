- **Checkout tools are keyed by the call's idempotency key** (`adk-payments`): every checkout
  tool passes `ToolContext::idempotency_key()` in the `idempotency_key` extension field, where
  the ACP server places an inbound `Idempotency-Key` header, so a backend can deduplicate a
  replayed call. `payments_checkout_create` names its transaction after that key instead of the
  clock, so a replayed call names the same transaction instead of opening a second one.
  `payments_checkout_create` and `payments_checkout_complete` declare
  `ToolEffect::NonIdempotent`, so an agent never retries them.
