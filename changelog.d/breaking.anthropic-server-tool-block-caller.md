- **Server tool blocks carry `caller`** (`adk-anthropic`): `ServerToolUseBlock`,
  `WebSearchToolResultBlock` and `WebFetchToolResultBlock` gain a
  `caller: Option<serde_json::Value>` field, read from responses and streams, so blocks from
  a dynamic-filtering turn replay with the `caller` the API sent. Struct literals need
  `caller: None`; the `new` constructors set it.
