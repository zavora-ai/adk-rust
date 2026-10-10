- **1-hour prompt cache entries** (`adk-anthropic`): `CacheTtl` serializes to the API's
  `"5m"` and `"1h"` values instead of a `{"type": ...}` object the API rejects with a 400.
  `CacheTtl::standard()` and `CacheTtl::long()` map to `"5m"` and `"1h"`, and
  `Usage::cache_creation_input_tokens_1h` is read from the response's
  `cache_creation.ephemeral_1h_input_tokens`.
