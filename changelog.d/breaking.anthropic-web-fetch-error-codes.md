- **Web fetch error codes round-trip** (`adk-anthropic`): `WebFetchErrorCode` is
  `#[non_exhaustive]`, gains `UrlTooLong`, `UrlNotAccessible`, and
  `UnsupportedContentType`, and `Unknown` becomes `Unknown(String)` holding the code the
  API sent. Matches on `WebFetchErrorCode::Unknown` become `Unknown(_)`.
