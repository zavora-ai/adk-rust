- **`A2aV1Client` reads streams and works over REST** (`adk-server`, feature `a2a-v1`):
  - `stream_message` and `subscribe` return the server's updates as a parsed stream
    (`v1_client::UpdateStream`), where `send_streaming_message` and `subscribe_to_task`
    return a raw response.
  - Both use the JSON-RPC binding, or the HTTP+JSON binding (`POST /message:stream`,
    `GET /tasks/{id}:subscribe`) when the card offers only that.
  - An error answer to a streaming call is returned as an error, and an error inside the
    stream ends it as its last item.
  - `send_message_with` sends a `SendMessageConfiguration`, for example
    `returnImmediately` or a push config.
  - The SSE decoder accepts a lone CR as a line end, which the SSE specification allows.
  - `RemoteA2aV1Agent::from_url` builds a remote agent from the card at a URL.
    `RemoteA2aV1Agent` uses the parsed stream, and on a card that offers only HTTP+JSON it
    sends over that binding.
