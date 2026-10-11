- **AP2 amounts compare by value** (`adk-payments`): `PaymentCurrencyAmount` equality
  compared the JSON number token, so a payment mandate total of `12` did not match a
  cart total of `12.0` and the payment was refused with a `payment_details_total`
  mismatch. Gemini re-serializes tool arguments through doubles and sends `12.0` as
  `12`, so every AP2 payment a Gemini agent submitted failed. Equality now compares
  the currency and the decimal value.
