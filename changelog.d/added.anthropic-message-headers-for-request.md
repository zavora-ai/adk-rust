- **Request header preview** (`adk-anthropic`): `Anthropic::message_headers_for_request`
  returns the headers `send` or `stream` would send for given parameters, including
  feature-derived `anthropic-beta` values, as a starting map for `send_with_headers`
  and `stream_with_headers`.
