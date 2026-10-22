- **Gemini Live session resumption keeps the server's handle** (`adk-realtime`): the session
  read the handle from `sessionResumptionUpdate.resumptionToken`, but the server sends
  `newHandle`, so no handle was ever kept and every reconnect started a blank session. The
  handle is now read from `newHandle` (with `resumptionToken` as a fallback), skipped while
  `resumable` is false, and reaches `RealtimeRunner` as the `resumeToken` it reconnects with.
