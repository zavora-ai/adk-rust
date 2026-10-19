- **`should_refresh_connection` takes an `rmcp::ServiceError`** (`adk-tool`): it took the
  error's text and matched substrings such as `eof`, so an HTTP 500 whose body said
  "geoffrey" reconnected and replaced a healthy session. It now decides from the error's
  variant: a closed transport, a broken pipe or reset connection, an interrupted Streamable
  HTTP stream, an expired session, or an HTTP 401. An HTTP 5xx or a JSON-RPC error never
  reconnects. Pass the `ServiceError` instead of its `to_string()`. A custom transport whose
  failures should trigger a reconnect closes the transport, or reports the failure as a
  `std::io::Error` of kind `BrokenPipe`, `ConnectionReset`, `ConnectionAborted`,
  `NotConnected`, or `UnexpectedEof` in its error's `source()` chain.
