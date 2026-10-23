- OpenAI Responses can opt into a session-owned WebSocket with shared HTTP request
  adaptation, header observation, response parsing and usage tracking. Handshake-only
  preconnection, idle and maximum lifetimes, adapted-header comparison and transport
  observations give callers lifecycle control without another retry owner. The transport
  closes unused preconnections after `WsOptions::preconnect_timeout` (25 seconds by
  default); helper clients stay on HTTP without `with_websocket`. Preconnection prepares
  only handshake headers, so `max` reasoning applies to the generation alone.

- The socket URL appends `/responses` to the base path, including gateway paths without
  `/v1` (`https://gw.example/openai` → `wss://gw.example/openai/responses`). Only
  non-function provider declarations count as native tools on either transport.

- The first provider event has a fixed 15-second default deadline. HTTP fallback is disabled by default;
  `WsOptions::fallback_to_http` explicitly enables it. Fallback before that event may generate and bill twice if the server already accepted the
  original request. Accepted requests are not automatically regenerated after socket
  delivery failures. Explicit terminal failures keep their documented error identity
  and leave a completed socket reusable. A request-level error event leaves a healthy
  socket reusable; an error after the response started closes it, so no later frame
  reaches the next request. Unserializable requests report `ws_serialize`. Caller beta
  values are retained alongside the required WebSocket token. Platform certificate
  verification, proxy bypass rules and legacy endpoint forms are preserved, and TLS
  configuration failures are not cached.
