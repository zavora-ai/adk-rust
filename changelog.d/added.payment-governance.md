- **Governed checkout API** (`adk-payments`): `GovernedCheckoutService` wraps a
  `MerchantCheckoutService` with a `PaymentPolicySet`. Its `create_checkout_as` and
  `complete_checkout_as` take a `PaymentCaller` that names the paying org and agent, the spend
  ledger, and the `PaymentApprover` that decides escalations; as a `MerchantCheckoutService`
  itself it supplies no approver, so an escalation fails with
  `payments.policy.approval_required`. `SpendLimitGuardrail::new` denies a payment when no
  ledger is configured, and `SpendLimitGuardrail::when_configured` reserves only when one is.
  `usd_micro_amount` converts a USD `Money` amount to micro-USD.
