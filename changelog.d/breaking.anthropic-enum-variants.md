- **New Anthropic API values no longer fail the turn** (`adk-anthropic`, `adk-model`):
  `ContentBlock`, `ContentBlockDelta`, and `MessageStreamEvent` gain `Unknown(serde_json::Value)`
  and `StopReason` gains `Unknown(String)`, holding what the API sent, where a block, delta,
  event, or stop reason newer than the client failed the whole response with a serialization
  error and an unknown stream event was reported as `Ping`. A malformed value of a known type
  still fails with its specific error. Each tagged enum exposes `KNOWN_TYPES`, and `StopReason`
  gains `as_str`. `adk-model` skips an unknown block with a `warn` log and keeps it in the
  native copy of the turn, so the next request replays it unchanged; an unknown stop reason
  maps to `FinishReason::Other`.
- **`StopReason` is no longer `Copy`** (`adk-anthropic`): clone it or match on a reference.
  `StopReason::from_str` is infallible (`Err = Infallible`).
- **Tool and result enums gain variants** (`adk-anthropic`): `ToolUnionParam` adds
  `WebSearch20260209` and `WebFetch20260209`, and `ContentBlock` adds
  `CodeExecutionToolResult`, `BashCodeExecutionToolResult`, and
  `TextEditorCodeExecutionToolResult`.
- **Web tool error codes round-trip** (`adk-anthropic`): `WebFetchErrorCode` gains
  `UrlTooLong`, `UrlNotAccessible`, and `UnsupportedContentType`, and its `Unknown` becomes
  `Unknown(String)` holding the code the API sent. `WebSearchErrorCode` gains `RequestTooLarge`
  and `Unknown(String)`, so a `web_search_tool_result_error` with a new code, or a
  `request_too_large` result, no longer fails deserialization. Both codes gain `as_str`;
  `WebSearchToolResultError` gains `is_request_too_large` and `is_unknown`.
- **Exhaustive matches on the Anthropic enums need the new arms** (`adk-anthropic`):
  `ContentBlock`, `ContentBlockDelta`, `MessageStreamEvent`, `StopReason`, `ToolUnionParam`,
  `WebFetchErrorCode`, and `WebSearchErrorCode` are not `#[non_exhaustive]`, and a match on
  `WebFetchErrorCode::Unknown` becomes `Unknown(_)`.
