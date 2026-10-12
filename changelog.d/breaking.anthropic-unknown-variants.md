- **New Anthropic API values no longer fail the turn** (`adk-anthropic`, `adk-model`):
  `ContentBlock`, `ContentBlockDelta`, and `MessageStreamEvent` gain `Unknown(serde_json::Value)`
  and `StopReason` gains `Unknown(String)`, holding what the API sent. A block, delta, event,
  or stop reason newer than the client previously failed the whole response with a
  serialization error, and an unknown stream event was reported as `Ping`. A malformed value
  of a known type still fails with its specific error. Each tagged enum exposes
  `KNOWN_TYPES`, and `StopReason` gains `as_str`.
  - Exhaustive `match` statements on these enums need an `Unknown` arm.
  - `StopReason` is no longer `Copy`; clone it or match on a reference.
    `StopReason::from_str` is infallible (`Err = Infallible`).
  - `adk-model` skips an unknown block with a `warn` log and keeps it in the native copy
    of the turn, so the next request replays it unchanged. An unknown stop reason maps to
    `FinishReason::Other`.
  - `WebFetchErrorCode`, `WebhookVerifyError`, and the server-fallback types are no longer
    `#[non_exhaustive]`.
