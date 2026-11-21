# Recorded video answers

Live exchanges the video contract tests replay through the real clients, recorded on
2026-11-21 with `proxy.py` beside this file. The proxy keeps each request's method, path and
body — every video or image reduced to its SHA-256 and size, a resumable upload's id replaced
by `<upload>` — and the response; it keeps no request header and refuses to save a record
that contains a credential it was sent.

| File | Provider and model | What it holds |
|------|--------------------|---------------|
| `sam-3-1.forklift-crossing-orange-rectangle.json` | Meta `sam-3.1` video | `forklift-crossing-3s.mp4` as an `input_video` data URL with the phrase "orange rectangle": three tracked objects (the forklift's body and two shelf boxes) on all 30 frames, `video_frames_processed: 50`. The repeated copies of the answer text the parser never reads are dropped |
| `gemini-file-api.aisle-forklift.json` | Gemini File API and `generateContent` | `aisle-forklift-120s.mp4` uploaded through the resumable File API, polled from `PROCESSING` to `ACTIVE`, then asked about as a `fileData` part with `videoMetadata.fps` 1 and `mediaResolution` `LOW`; `gemini-3.8-flash` answered `503` (high demand) twice, and a retry on `gemini-3.7-flash` (kept as `retry`) answered `503` after about ten minutes |

The phrase "orange forklift" matched nothing in the drawn scene on `sam-3.1` (an empty
`output_text`, still billed 50 frames).

## Recording again

```bash
python3 -I proxy.py 18761 https://api.meta.ai /tmp/rec-sam &
python3 -I proxy.py 18762 https://generativelanguage.googleapis.com /tmp/rec-gemini &
ADK_VISION_SAM_BASE=http://127.0.0.1:18761/v1 ADK_VISION_GEMINI_BASE=http://127.0.0.1:18762/v1beta/ \
  META_API_KEY=… GEMINI_API_KEY=… \
  cargo test -p adk-vision --features cloud,sam,gemini-video --test video_live_tests -- --ignored --nocapture
```
