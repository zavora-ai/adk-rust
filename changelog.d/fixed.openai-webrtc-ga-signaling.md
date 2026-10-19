- **OpenAI Realtime over WebRTC connects and carries events** (`adk-realtime`): the transport
  requested its token from `POST /v1/realtime/sessions` and its SDP answer from
  `POST /v1/realtime?model=…`, which the GA API answers with 404 and 400, and nothing drove
  the WebRTC connection after signaling, so no event or audio ever arrived. It now mints an
  ephemeral key with `POST /v1/realtime/client_secrets` for a session built from the
  `RealtimeConfig`, exchanges SDP with `POST /v1/realtime/calls`, and runs an I/O task that
  carries the `oai-events` data channel and decodes the model's Opus audio into
  `ServerEvent::AudioDelta` (24 kHz PCM16).
