- **Payment policies are enforced** (`adk-payments`): `PaymentPolicySet` and its guardrails were
  never evaluated, so the payment tools completed a checkout of any amount at any merchant.
  `GovernedCheckoutService` now evaluates the policies before every checkout creation and
  completion, and the payment tools use it; `PaymentToolsetBuilder::with_payment_policies` and
  `with_spend_ledger` configure them. A denial refuses the checkout (`payments.policy.denied`)
  before the backend is reached; an escalation goes through the run's tool confirmation flow.
  The new `SpendLimitGuardrail` reserves each completed checkout against the spend ledger under
  `app / agent / merchant_id`, commits it on completion, releases it on failure, and denies the
  payment when the ledger is unreachable.
