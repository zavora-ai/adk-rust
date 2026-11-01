- **Live transcription yields typed events** (`adk-audio`): `SttProvider::transcribe_stream`
  yields `TranscriptEvent`s (`Started`, `SpeechStarted`, `Partial`, `Final`, `SpeechEnded`,
  `Speaker`, `Progress`, `Usage`) instead of `Transcript`s, so partial and final text are told
  apart, and has a default that returns `AudioError::Stt` for a provider without streaming.
  `DeepgramStt` maps `is_final`, `speech_final`, `SpeechStarted`, `UtteranceEnd` and the closing
  `Metadata` duration, and requests `vad_events` and `utterance_end_ms`; the ONNX providers yield
  one `Final` per segment. `SttOptions` is `#[non_exhaustive]` with `with_*` builders and gains
  `keywords` and `language_bias`.

  Migration: match `TranscriptEvent::Final { transcript, .. }` where a stream item was a
  `Transcript`, and `Partial { text, .. }` for interim text; return `TranscriptStream` from an
  implementation, or drop a `transcribe_stream` that only failed; build `SttOptions` with
  `SttOptions::default().with_language(..)` instead of a struct literal.
