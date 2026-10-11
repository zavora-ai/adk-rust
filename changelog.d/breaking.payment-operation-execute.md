- **`PaymentOperation` gains `ExecutePayment`** (`adk-payments`): both
  `guardrail::PaymentOperation` (the operation a payment policy evaluates) and
  `auth::PaymentOperation` (scopes and audit resources) have a new `ExecutePayment`
  variant for direct payment execution such as an AP2 payment mandate. An exhaustive
  `match` on either enum needs the new arm; `guardrail::PaymentOperation::moves_money`
  reports whether an operation moves money. `ALL_PAYMENT_SCOPES` includes
  `payments:payment:execute`.
- **`Ap2Adapter` enforces payment policies** (`adk-payments`): a payment mandate is
  evaluated by `SpendLimitGuardrail::when_configured` by default, so a spend ledger on
  the adapter or the caller now caps AP2 payments, and policies added with
  `with_payment_policies` can deny or escalate a cart mandate or payment mandate that
  previously went through. An escalated protocol payment fails with
  `payments.policy.approval_required`.
