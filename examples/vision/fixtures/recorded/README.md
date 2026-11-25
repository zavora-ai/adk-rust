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
| `claude/s15-haiku-crop.json` | Anthropic Messages, `claude-haiku-5-5`, recorded 2026-11-25 | S15: the reasoner's verdict on the scratch's crop — a defect at 0.60, read as a crack |
| `nim-vlm/s15-nemotron-crop.json` | OpenAI-compatible chat completion, NVIDIA NIM `nvidia/nemotron-3.5-super-vl-120b-a12b` on `integrate.api.nvidia.com`, recorded 2026-11-25 | S15: the verifier's verdict on the same crop, its request carrying the reasoner's claim — no defect at 0.80, a surface mark |
| `qwen3-vl-4b/s13-aisle-20min.json` | Qwen3-VL-4B Instruct on the device, recorded 2026-11-24 by `adk-vision`'s `video_local_live_tests` (`ADK_VISION_RECORD`) on an Apple M4 CPU, with no network (`HF_HUB_OFFLINE=1` inside a sandbox that denies it) | S13: the visibility pass's ten calls over 240 frames — the target, eight checks of four frames, the eighth finding the forklift, and the question over six frames, answered 12:50.0 (entry 12:45) — each with its images' SHA-256 and size, the model's answer, its token counts (9,208 in, 120 out) and its wall time (706 s in all; 61 s with `mistralrs-metal`, the same answers) |

S5, S7 and S8 replay adk-vision's own recordings (`adk-vision/tests/fixtures/cloud/recorded/`).
S17 replays adk-vision's live analyst run
(`adk-vision/tests/fixtures/video/recorded/analyst-warehouse-hour.claude-haiku-5-5.json`, written
by `analyst_live_tests`): a local server answers each Messages request with the recorded caption
of the chunk its time range overlaps most, or the recorded answer to its question.
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

then copy the records here under the names above. S15 records both of its answers in one run,
since the verifier's request carries the reasoner's answer:

```bash
python3 fixtures/recorded/proxy.py 18742 https://api.anthropic.com /tmp/rec-claude &
python3 fixtures/recorded/proxy.py 18743 https://integrate.api.nvidia.com /tmp/rec-nim-vlm &
VISION_RECORD_ANTHROPIC=127.0.0.1:18742 VISION_RECORD_NIM_VLM=127.0.0.1:18743 \
  ANTHROPIC_API_KEY=… NVIDIA_API_KEY=… cargo run -- s15_perceiver_reasoner_verifier
```

A recording replaces a scripted file whole, `scripted` flag included, and the scenario then says
it ran on a live recording.

S13's local path is recorded by `adk-vision`'s own live test rather than a proxy, since the
model runs in process. From the repository root, with Qwen3-VL-4B's weights in the cache and a
user-installed `ffmpeg`:

```bash
ADK_VISION_DEVICE=cpu ADK_VISION_CLIP=$PWD/examples/vision/fixtures/video/aisle-forklift-20min.mp4 \
  ADK_VISION_ENTRY_MS=765000 ADK_VISION_WIDTH=640 ADK_VISION_STRIDE=5 \
  ADK_VISION_FRAMES_PER_CHECK=4 ADK_VISION_FRAMES_PER_CALL=8 \
  ADK_VISION_RECORD=$PWD/examples/vision/fixtures/recorded/qwen3-vl-4b/s13-aisle-20min.json \
  cargo test -p adk-vision --features mistralrs --test video_local_live_tests \
  qwen3_vl_finds_the_entry_after_a_visibility_pass -- --ignored --nocapture
```
