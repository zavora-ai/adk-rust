# Realtime voice

`voice_assistant` is a real `RealtimeAgent` backed by OpenAI Realtime. Select it
and send:

> Give me a calm, twenty-second breathing exercise.

The response transcript streams into one message and the completed PCM stream
is exposed as a playable WAV control. A realtime session remains open for
additional bidirectional input, so press **Stop response stream** after the
first response in this text-seeded runtime demonstration.

The embedded runtime currently seeds the realtime session with text. The
full microphone/WebSocket conversation surface remains available in
[`examples/realtime_voice`](../../realtime_voice/README.md); it is intentionally
not simulated here.
