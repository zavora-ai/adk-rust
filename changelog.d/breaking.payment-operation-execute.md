- **`auth::PaymentOperation` gains `ExecutePayment`** (`adk-payments`): the operation of a
  direct payment, such as an AP2 payment mandate, with the scope `payments:payment:execute`
  (`PAYMENT_EXECUTE_SCOPE`), which `ALL_PAYMENT_SCOPES` now includes. An exhaustive `match` on
  the enum needs the new arm.
- **Autonomous AP2 payments need a verified intent authorization** (`adk-payments`): a
  human-not-present payment mandate without user authorization fails with
  `MissingIntentAuthorization` unless the transaction's intent mandate was submitted with its
  user authorization artifact and verified. Submit the intent mandate with the artifact.
