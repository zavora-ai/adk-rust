- **The Responses WebSocket transport pools connections, times out, and never resends**
  (`adk-model`, `openai-ws`): `WsTransport` keeps up to `with_max_connections` connections
  (default 4), so concurrent responses no longer wait on one shared connection, and a request
  beyond the limit fails with `model.openai_responses.ws_pool_timeout` after
  `with_acquire_timeout`. A continuation takes the connection of its `previous_response_id`.
  Handshakes are bounded by `with_connect_timeout` and server events by `with_read_timeout`
  (`model.openai_responses.ws_timeout`). A failed send is no longer sent again on a new
  connection: it fails with `model.openai_responses.ws_send`, not retryable, because the
  server may have received it. A connection returns to the pool before the final chunk is
  yielded, so a request made while that chunk is held no longer waits on itself. The
  handshake sends the configured organization and project.
