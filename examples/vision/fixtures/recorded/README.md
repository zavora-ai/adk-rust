# Recorded responses

Live answers the scenarios replay through the real clients, recorded on 2026-11-19 with
`proxy.py` beside this file. The proxy kept each request body with every inline image reduced
to its SHA-256 and size, and the response; it kept no request header and refuses to save a
record that contains a credential it was sent.

| Directory or file | Provider and model | Scenario |
|-------------------|--------------------|----------|
| `nim-crops/000.json`–`004.json` | NVIDIA NIM `nvidia/nemotron-ocr-v1` on `ai.api.nvidia.com` | S2: the five escalated crops, in the order the desk sends them |
| `claude/s3-haiku-id-card.json` | Anthropic `claude-haiku-5-5` | S3: the redacted identity card |
| `claude/s4-sonnet-id-card.json` | Anthropic `claude-sonnet-5-5` | S4: the approved crop of an identity card |
| `gemini/s13-aisle-20min.json` | Gemini `gemini-3.6-flash` File API and `generateContent`, recorded 2026-11-23 | S13: the twenty-minute clip uploaded once, polled to `ACTIVE` (15 more `PROCESSING` polls dropped), asked about as a file part at 1 fps and low media resolution; it answered 12:40.0. `gemini-3.8-flash` answered `503` (high demand) to the same request minutes earlier |
| `qwen3-vl-4b/s13-aisle-20min.json` | Scripted, not recorded | S13: the visibility pass's ten requests over 240 frames and answers in Qwen3-VL's JSON form stating what each frame shows. Qwen3-VL-4B's 8.9 GB weights need 14 GiB free; the machine had 7–11 GiB, so the model did not run. Token counts are estimates. `adk-vision`'s `video_local_live_tests` runs the model itself |

S5, S7 and S8 replay adk-vision's own recordings (`adk-vision/tests/fixtures/cloud/recorded/`).
S11's `sam-3.1` video answers are in `../conveyor/recorded/`, one per escalation clip.

## Recording again

Each scenario sends to a recording proxy instead of its local server when
`VISION_RECORD_<NAME>` names one (`host:port`), with the provider's key in the environment:

```bash
python3 fixtures/recorded/proxy.py 18741 https://ai.api.nvidia.com /tmp/rec-nim &
python3 fixtures/recorded/proxy.py 18742 https://api.anthropic.com /tmp/rec-claude &
VISION_RECORD_NIM=127.0.0.1:18741 VISION_RECORD_ANTHROPIC=127.0.0.1:18742 \
  NVIDIA_API_KEY=… ANTHROPIC_API_KEY=… \
  cargo run -- s1_document_desk_local_first s2_escalate_the_crop_not_the_page \
  s3_redact_before_it_leaves s4_approve_with_the_image
```

then copy the records here under the names above.
