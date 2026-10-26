- **Token rails beside the AP2 desk** (`adk-website`): `edge::token::TokenDesk` takes
  `pay_order`'s single-use `credential` on rails such as `card`, hands it to the owner's
  `PaymentExecutionService` with the order's total and idempotency key, pays an order once and
  answers with a receipt signed by the `receipt` key; the token reaches the processor only.
  `edge::token::PaymentRails` serves several desks by rail (spec 3.5 §7.5).
