- **`PaymentPolicyGuardrail::evaluate` is async and takes a `PaymentPolicyContext`**
  (`adk-payments`): the context carries the operation, the paying org and agent, and
  the spend ledger, so a policy can read or reserve spend. `PaymentPolicySet::evaluate`
  is async and takes the same context. Implement the trait with `#[async_trait]` and add
  the `context` parameter.
- **Payment tools evaluate payment policies** (`adk-payments`): `create_checkout_tool`,
  `complete_checkout_tool`, and `PaymentToolsetBuilder` route checkout creation and
  completion through `GovernedCheckoutService`. An escalated checkout now fails with
  `payments.policy.approval_required` unless the run approves the call, and a completed
  checkout is reserved against `RunConfig::spend_ledger` when one is set.
- **`RunConfig` gains `spend_ledger`** (`adk-core`): a `RunConfig` built with a struct
  literal must set the field or end with `..Default::default()`.
