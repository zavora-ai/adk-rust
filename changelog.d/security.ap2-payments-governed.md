- **AP2 payments are governed** (`adk-payments`): `Ap2Adapter::submit_payment_mandate`
  called the payment backend without evaluating payment policies, reserving spend, or
  recording an audit event, so an AP2 payment bypassed every control a governed ACP
  checkout enforces. The adapter now evaluates its `PaymentPolicySet` before a cart
  mandate creates a checkout and before a payment mandate is consumed, reserves the
  payment total in the spend ledger under `app / agent / merchant_id` and commits it once
  the backend authorizes the payment, and, with `with_payment_auditor`, refuses a payment
  whose decision cannot be recorded (`payments.auth.audit_failed`). A refused or pending
  payment leaves its mandate unconsumed, so it can be resubmitted once approved.
- **An unverified intent grants no autonomous authority** (`adk-payments`): a
  human-not-present payment without user authorization was accepted on the strength of
  any stored intent mandate, including one submitted human-present without an
  authorization artifact. The payment now requires a verified intent authorization and
  fails with `MissingIntentAuthorization` otherwise.
