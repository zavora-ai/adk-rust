- **A2A v1 `SendMessage` answers with the `SendMessageResponse` oneof** (`adk-server`): JSON-RPC
  `SendMessage` and REST `POST /message:send` returned a bare `Task`, which adk-go and other v1
  clients reject. Both now return `{"task": …}`, as A2A §9.4.1 and §11.4 require.
  `A2aV1Client::send_message` and `A2aV1Client::send_message_rest` return
  `a2a_protocol_types::responses::SendMessageResponse` instead of `Task`. They still accept the
  bare `Task` that adk-server before 3.0 sends, so old and new peers keep working together.
  `RemoteA2aV1Agent` also accepts a direct `Message` reply.

  Migration: match `SendMessageResponse::Task` and `SendMessageResponse::Message` where you used
  the returned `Task`.
