# Rust & Beyond Podcast

AI-generated podcast series about ADK-Rust — two hosts, natural voices, zero manual recording. Every episode is produced entirely with code.

## Episodes

| # | Title | Duration | Date |
|---|-------|----------|------|
| 2 | [v1.0.0 — The Stable Foundation](https://www.youtube.com/watch?v=tlqaE8qeHac) | 10:12 | 2026-06-07 |
| 1 | What is ADK-Rust? | 2:21 | 2026-03-14 |

## How Episodes Are Made

Each episode is produced using the [ADK-Rust MCP Toolkit](https://github.com/zavora-ai/adk-rust-mcp-toolkit) — a set of MCP servers that provide speech synthesis, audio processing, image generation, and video creation capabilities.

### Pipeline

```
Script (markdown) → TTS Synthesis → Audio Segments → Concatenate → Slides (Marp) → Video (ffmpeg) → MP4
```

### Tools Used

| Step | Tool | MCP Server |
|------|------|-----------|
| Voice synthesis | Google Chirp3-HD TTS | `adk-speech` from [adk-rust-mcp-toolkit](https://github.com/zavora-ai/adk-rust-mcp-toolkit) |
| Audio concatenation | ffmpeg | `adk-avtool` from [adk-rust-mcp-toolkit](https://github.com/zavora-ai/adk-rust-mcp-toolkit) |
| Audio/video combine | ffmpeg | `adk-avtool` from [adk-rust-mcp-toolkit](https://github.com/zavora-ai/adk-rust-mcp-toolkit) |
| Slide deck | [Marp](https://marp.app) | Local CLI (`marp --images png`) |
| Video encoding | ffmpeg (H.264 High, faststart) | `adk-avtool` from [adk-rust-mcp-toolkit](https://github.com/zavora-ai/adk-rust-mcp-toolkit) |
| Thumbnail | Pillow (play button overlay) | Local Python script |

### Voices

- **James** — `en-US-Chirp3-HD-Fenrir` (male)
- **Ada** — `en-US-Chirp3-HD-Kore` (female)

### Video Encoding (Smart TV Compatible)

```
H.264 High Profile, Level 4.1
30fps, keyframes every 2 seconds (-g 60 -sc_threshold 0)
yuv420p pixel format
Stereo AAC-LC at 48kHz, 192kbps
-movflags +faststart (moov atom at front)
```

## MCP Toolkit

The [ADK-Rust MCP Toolkit](https://github.com/zavora-ai/adk-rust-mcp-toolkit) provides the MCP servers used in podcast production:

- **`adk-speech`** — Text-to-speech via Google Cloud Chirp3-HD voices
- **`adk-avtool`** — FFmpeg-based audio/video processing
- **`adk-image`** — Image generation via Google Imagen
- **`adk-music`** — Music generation via Google Lyria
- **`adk-video`** — Video generation via Google Veo

## File Structure

```
docs/podcast/
├── README.md                          # This file
├── adk-rust-episode-1.mp4             # Episode 1
├── adk-rust-episode-2.mp4             # Episode 2 (TV-compatible)
├── adk-rust-episode-2.wav             # Episode 2 audio-only
├── episode-2-thumbnail.jpg            # Thumbnail with play button
├── episode-2-v1-launch-script.md      # Full script
├── episode-2-slides.md                # Marp slide deck
├── ep2-segments/                      # Individual TTS segments
└── slides/                            # Exported slide PNGs
```

## Contributing an Episode

1. Write the script in markdown (see `episode-2-v1-launch-script.md`)
2. Create the Marp slide deck (see `episode-2-slides.md`)
3. Use the [MCP toolkit](https://github.com/zavora-ai/adk-rust-mcp-toolkit) to synthesize and assemble
4. Export to MP4 with TV-compatible encoding
5. Upload to YouTube, update this README
