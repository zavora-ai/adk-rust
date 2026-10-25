- **`adk_awp::sign_payload` and `verify_signature` are deprecated** (`adk-awp`, spec 3.5 §10
  X5): event deliveries carry Standard Webhooks signatures, which receivers verify with
  `adk_core::webhook::WebhookVerifier`; the `sha256=<hex_digest>` pair stays for receivers of
  the earlier format and warns on use.
