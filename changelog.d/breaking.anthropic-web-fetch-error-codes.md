- **Web fetch error codes round-trip** (`adk-anthropic`): `WebFetchErrorCode` gains `UrlTooLong`, `UrlNotAccessible`, and
  `UnsupportedContentType`, and `Unknown` becomes `Unknown(String)` holding the code the
  API sent. Matches on `WebFetchErrorCode::Unknown` become `Unknown(_)`.
