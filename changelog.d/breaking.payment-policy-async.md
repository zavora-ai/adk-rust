- **`PaymentPolicyGuardrail::evaluate` is async and takes a `PaymentPolicyContext`**
  (`adk-payments`): the context carries the operation (`guardrail::PaymentOperation`), the
  paying org and agent, and the spend ledger, so a policy can read or reserve spend.
  `PaymentPolicySet::evaluate` is async and takes the same context. Implement the trait with
  `#[async_trait]` and add the `context` parameter.
- **Payment tools govern checkouts** (`adk-payments`): `create_checkout_tool`,
  `complete_checkout_tool`, and `PaymentToolsetBuilder` route checkout creation and completion
  through `GovernedCheckoutService`. Completion fails with `payments.policy.checkout_not_found`
  when the backend's `get_checkout` does not return the checkout. A run with
  `RunConfig::spend_ledger` set reserves each completed checkout against it, and refuses one
  whose total is not in USD or exceeds a limit. A checkout a policy escalates fails with
  `payments.policy.approval_required` unless the run approves the call.
