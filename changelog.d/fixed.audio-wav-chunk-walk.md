- **WAV decoding finds `fmt ` and `data` wherever they are** (`adk-audio`): `decode` with
  `AudioFormat::Wav` walks the RIFF chunks instead of reading the format at fixed offsets, so
  CoreAudio WAVs — macOS `say` writes a `JUNK` chunk before `fmt ` and `FLLR` before
  `data` — no longer fail with `unsupported bits per sample: 0`. Odd-sized chunks skip their
  pad byte, and a missing or short `fmt ` chunk, or a channel count of 0, is reported by name.
