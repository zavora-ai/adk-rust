- **AP2 amounts compare by value** (`adk-payments`): `PaymentCurrencyAmount` equality
  compared the JSON number token, so a payment mandate total of `12` did not match a cart
  total of `12.0` and the payment was refused with a `payment_details_total` mismatch.
  Gemini sends function-call numbers as doubles without a fractional part, so a mandate a
  Gemini agent passed through a tool call was refused. Equality now compares the currency
  and the decimal value, and the types that contain an amount, such as `PaymentItem`,
  inherit it.
