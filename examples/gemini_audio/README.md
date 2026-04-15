# Gemini Audio — Agentic TTS & STT

Demonstrates Gemini-powered text-to-speech and speech-to-text in an agentic workflow.

## Scenarios

1. **TTS Agent** — An LLM agent writes a podcast intro script, then calls the `synthesize_speech` tool which uses Gemini 3.1 Flash TTS to generate audio. Saved as WAV.

2. **STT Agent** — An LLM agent calls the `transcribe_audio` tool which uses Gemini STT to transcribe the audio from scenario 1, then summarizes it.

3. **Multi-Speaker TTS** — Direct API call demonstrating two-speaker dialogue synthesis with different voices.

## Usage

```bash
export GOOGLE_API_KEY=your-key-here
cargo run --manifest-path examples/gemini_audio/Cargo.toml
```

## Output

- `$TMPDIR/gemini_tts_output.wav` — Single-speaker TTS output
- `$TMPDIR/gemini_multi_speaker.wav` — Multi-speaker dialogue
