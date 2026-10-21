- **The payment tools declare their arguments** (`adk-payments`): `payments_checkout_create`,
  `payments_checkout_update`, `payments_checkout_complete`, `payments_checkout_cancel`,
  `payments_status_lookup`, and `payments_intervention_continue` return a `parameters_schema`
  describing the arguments they parse (merchant, cart lines and `Money` amounts, fulfillment,
  transaction and intervention ids, payment method), with their required fields and enums.
  Without one each tool was declared to take no arguments, so a live model called it with `{}`
  and every call failed at argument parsing.
