- **Cost estimates bill 1-hour cache writes at the 1-hour rate** (`adk-anthropic`):
  `pricing::estimate_cost` splits `cache_creation_input_tokens` by TTL, billing the
  `cache_creation_input_tokens_1h` share at `cache_write_1h` (2× base input) and the
  remainder at `cache_write_5m` (1.25× base input). It previously billed every write at
  the 5-minute rate.
