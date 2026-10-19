- **AP2 payment tool** (`adk-payments`, `ap2`): `ap2_payment_tool` and
  `PaymentToolsetBuilder::with_ap2_adapter` expose `payments_ap2_payment_submit`, which
  submits an AP2 payment mandate under the adapter's governance. The tool declares
  `ToolEffect::NonIdempotent` and the `payments:payment:execute` scope, routes escalations
  through the run's tool confirmation flow, uses `RunConfig::spend_ledger` when the adapter
  has no ledger, and passes the call's idempotency key to the payment backend. With
  `ap2-mcp`, its response includes the `Ap2McpPaymentContinuation` view.
- **AP2 adapter governance settings** (`adk-payments`): `Ap2Adapter::with_payment_policies`,
  `with_spend_ledger`, `with_payment_auditor`, and `submit_payment_mandate_as`, which
  attributes a payment to a `PaymentCaller`. `submit_payment_mandate` supplies no approver, so
  a payment a policy escalates fails with `payments.policy.approval_required`. Every executed
  payment command carries an `idempotency_key` extension field — the caller's key, else
  `ap2:payment_mandate:<payment_mandate_id>`.
- **`GovernedCheckoutService::with_auditor`** (`adk-payments`): records each governed
  checkout decision with a `PaymentAuditor` before the backend is reached, refusing an
  operation whose record cannot be written.
