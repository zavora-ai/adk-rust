# Audio Samples

Pre-generated TTS audio samples from three providers. Play with any WAV-compatible player.

## ElevenLabs (Rachel & Antoni voices)
- `elevenlabs_rachel.wav` — Female voice
- `elevenlabs_antoni.wav` — Male voice
- `elevenlabs_emotion_happy.wav` — Happy tone
- `elevenlabs_emotion_calm.wav` — Calm tone
- `elevenlabs_emotion_whisper.wav` — Whispered tone

## Gemini (Puck, Kore, Aoede voices)
- `gemini_tts_puck.wav` — Male voice
- `gemini_tts_kore.wav` — Female voice
- `gemini_tts_aoede.wav` — Female voice

## OpenAI (Alloy, Nova, Onyx, Shimmer voices)
- `openai_tts_alloy.wav` — Neutral voice
- `openai_tts_nova.wav` — Female voice
- `openai_tts_onyx.wav` — Male voice
- `openai_tts_hd.wav` — Shimmer voice, HD model

## Regenerate

```bash
cargo run --example audio_elevenlabs_tts --features audio
cargo run --example audio_gemini_tts --features audio
cargo run --example audio_openai_tts --features audio
```
