- **`list_session_files` encodes the session ID** (`adk-anthropic`):
  `ManagedAgentsClient::list_session_files` wrote the session ID into the `scope_id` query
  parameter unencoded, so an ID containing `&`, `#`, or `=` changed the request. It is now
  URL-encoded.
