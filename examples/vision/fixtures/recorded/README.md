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

S5, S7 and S8 replay adk-vision's own recordings (`adk-vision/tests/fixtures/cloud/recorded/`).

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
