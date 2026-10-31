- **STT streams no longer end silently** (`adk-audio`): `GeminiStt::transcribe_stream` and
  `WhisperApiStt::transcribe_stream` returned an empty stream, which reads as "no speech"; they
  now return `AudioError::Stt` saying streaming is not yet implemented and to call `transcribe`
  with the buffered audio, as `AssemblyAiStt` and `MlxSttProvider` do. `DeepgramStt`'s
  audio-sending task outlived the transcript stream and kept the caller's audio stream and the
  WebSocket open; dropping the stream now aborts the task and closes the socket.
