- **The debug trace route returns a span to its owner only** (`adk-server`): with a
  `RequestContextExtractor` and `SecurityConfig::expose_admin_debug` (set by
  `SecurityConfig::development()`), `GET /api/debug/trace/{event_id}` returned any user's span,
  prompt and response attributes included, to any authenticated caller, and a value that was not
  an event ID, such as a session ID, matched the first span carrying it in any attribute. The
  route now matches the span's `gcp.vertex.agent.event_id` only, answering `404` for any other
  value, and answers `403` to an authenticated caller unless the span's `adk.user_id` is theirs,
  so a span that records no owner is refused too.
