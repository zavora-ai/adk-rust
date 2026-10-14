- **Web search error codes round-trip** (`adk-anthropic`): `WebSearchErrorCode` gains
  `RequestTooLarge` and `Unknown(String)` holding the code the API sent, so a
  `web_search_tool_result_error` with a new code no longer fails the response. A
  `request_too_large` result previously failed JSON deserialization.
  `WebSearchToolResultError` gains `is_request_too_large` and `is_unknown`, and the code
  gains `as_str`. Exhaustive matches on `WebSearchErrorCode` need `RequestTooLarge` and
  `Unknown(_)` arms.
