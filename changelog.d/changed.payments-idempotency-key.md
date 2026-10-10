- **Checkout tools carry the call's idempotency key** (`adk-payments`):
  `payments_checkout_create` and `payments_checkout_complete` declare
  `ToolEffect::NonIdempotent`, so an agent never retries them, and every checkout tool
  passes `ToolContext::idempotency_key()` in the `idempotency_key` extension field the
  ACP adapter uses for the `Idempotency-Key` header.
