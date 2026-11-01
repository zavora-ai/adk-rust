- **Muse realtime transcription** (`adk-model`, `adk-audio`): `MuseRealtimeAsrClient` (feature
  `muse-realtime`) transcribes live PCM16 audio over Meta's realtime WebSocket: an ack-gated
  handshake with the key only in its frame, a lead window over the audio processed, `endStream`
  drained to close `1000`, `AsrEvent`s with turn attribution, close codes mapped to error
  categories (`model.muse.session_limit` for the 60-minute limit), no automatic reconnect, and
  the session metered against the current run. `adk_audio::MuseStt` (feature `muse`) is an
  `SttProvider` over it and `MuseAsrClient`, yielding `TranscriptEvent`s with one `Final` per
  turn.
