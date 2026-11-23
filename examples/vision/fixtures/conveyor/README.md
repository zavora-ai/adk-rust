# Conveyor clips

S11 escalates a part whose crack stays uncertain to `sam-3.1` video with three seconds of that
part, cut once from the generated conveyor clip (`ConveyorSpec::default()`, ten frames a
second) so the scenario needs no encoder:

```bash
cargo run -- --write-s11-clips    # ffmpeg with libx264: ADK_VISION_FFMPEG, else on PATH
```

| Path | What it is |
|------|------------|
| `part-NNN.mp4` | H.264, 640×360, 30 frames: defective part NNN centred in view |
| `recorded/part-NNN.json` | `sam-3.1`'s live answer to that clip with the phrase "crack", recorded 2026-11-23 with `adk-vision/tests/fixtures/video/recorded/proxy.py`. Each holds no object (the drawn crack is 2.2 px wide) and is billed 50 frames for the clip's 30. Parts 8, 26, 57, 175 and 195 were sent by S11 through `SamVideoTracker`; the other seven through the same proxy with the body `SamVideoTracker` builds, so a change in which parts escalate still finds its answer |

The replay answers each request by the SHA-256 of the clip it carries.
