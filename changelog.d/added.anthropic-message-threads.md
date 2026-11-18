- **Claude message threads** (`adk-model`, `adk-anthropic`): `AnthropicConfig::with_message_threads(true)`
  continues a thread the API keeps for each conversation (beta `message-threads-2026-08-12`), so a
  request sends only the messages since the previous response; in a live Claude Haiku 5.5 tool
  loop the first request of a second turn sent 327 bytes instead of 13,463. The session stays the
  source of truth: a request continues its thread only when the conversation it sends starts with
  exactly what the thread holds and its fixed fields are unchanged, a refused thread request is
  sent again without one, and a conversation that carries a compaction goes without a thread.
  `adk-anthropic` gains `ThreadParam`, `MessageCreateParams::thread`, `MessageCreateParams::betas`
  and `MessageCreateParams::required_betas`.
