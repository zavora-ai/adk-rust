- **A2A streams that stop early are errors, and split characters stay intact** (`adk-server`):
  `A2aClient::send_streaming_message`, `RemoteA2aAgent`, and `RemoteA2aV1Agent` treated a stream
  that ended before a final status as a completed response, and decoded each network read on its
  own, so a multi-byte character split across reads became `U+FFFD`. They now decode whole SSE
  lines (joining multi-line `data:` fields) and report a stream that ends without a final status
  as an error.
