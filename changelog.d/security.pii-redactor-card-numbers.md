- **`PiiRedactor` redacts every common card number layout, Luhn-checked** (`adk-guardrail`):
  `PiiType::CreditCard` matches 13–19 digit card numbers written as one run of digits or grouped
  4-4-4-4, 4-6-5 (American Express), or 4-4-4-4-3, so American Express and 19-digit numbers are
  redacted whole instead of passing through or leaving their last group. A number that fails the
  Luhn check, such as a 16-digit order number, is no longer redacted. `PiiType::Email` no longer
  matches `|` in the top-level domain, so `user@example.com|ref` keeps `|ref`.
  `PiiRedactor::new()` still leaves IP addresses unredacted; its documentation now says so and
  shows `with_types` naming `PiiType::IpAddress`.
