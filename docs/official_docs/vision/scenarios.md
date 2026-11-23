# The Vision Scenarios

`examples/vision` (package `vision-scenarios`) runs spec 3.7's demonstration scenarios as one
program. Every pass condition is asserted on what a run leaves behind — egress records, audit
records, approvals, the spend ledger, the frame governor's steps, stored frames — never on
what a model wrote. The cloud backends answer from recorded responses served to their real
clients by a local server, so no key is needed, and the PR tier runs every scenario in the
`vision-scenarios` job.

```bash
cargo run --manifest-path examples/vision/Cargo.toml                          # every scenario
cargo run --manifest-path examples/vision/Cargo.toml -- s9_inspector          # one scenario
cargo run --manifest-path examples/vision/Cargo.toml -- --report scenarios.md # the pass table
```

The first run fetches PP-OCRv6 tiny, RF-DETR Nano and SAM 2.1 Tiny q4 (about 165 MB) by
revision and SHA-256 into the Hugging Face cache; set `HF_HUB_CACHE` to keep them elsewhere.
[Getting started](getting-started.md) walks through S1–S8, the Document desk.

## Phase B: the Inspector, Wakes, Budgets, Cameras and Video

S9–S12 run on the conveyor clip: 200 printed parts on a belt, twelve with cracks of seeded
length, and a fiducial of known size in the belt's plane, drawn frame by frame from a seed
(`adk_vision::stream::fixtures::ConveyorClip`) and served over RTSP by the in-process test
server.

| Scenario | What passes | Runs on |
|----------|-------------|---------|
| S9 Inspector | Each of the twelve defects is one track and one wake; no wake on a defect-free part; only the woken frames are stored; SAM 2.1 masks the fiducial and each crack, the fiducial calibrates the plane, and every crack measures within 5% of its seeded length | The clip at 1280×720 over RTSP; `BeltDetector` on the stream; RF-DETR Nano from a user-written manifest; SAM 2.1 Tiny q4 |
| S10 Wake on what matters | A harness process asleep in the kernel wakes once; its payload names the frame's artifact and digest, the track and its regions within 4 KiB; the woken turn loads the frame through `vision_detect` by its digest; its line stop is held with the frame on the card, and one approval lets it run once | The kernel, the harness with the vision pack, `FrameSources`, `VisionWakeSource` |
| S11 A budget per frame | Thirty minutes of two cameras across midnight: escalation to `sam-3.1` video stops at the daily quota and resumes at midnight; every request is billed 50 frames in the ledger; a small local quota lowers the rate to its floor, stops escalation, then pauses the source, each change a `vision.governor.step` record | `StreamPipeline` and `FrameGovernor` at the frames' capture times; `sam-3.1` video on recorded live answers |
| S12 Live camera surface | Each frame is shown with its detections as overlays within one frame interval of the detection; switched off, the server drops frames before the pipeline looks at them; nothing is stored until a card keeps exactly its frame; the Inspector's line stop is held on a decision card showing that frame, and the operator's approval stops the line once | `PipelineCameras` and the surface engine, without a browser |
| S13 Native video, two ways | "When does the forklift enter the aisle?" over twenty minutes: Gemini uploads the clip once through the File API and answers from a file part; Qwen3-VL answers over frames sampled every five seconds after a visibility pass; both within ±5 s of 12:45, tokens and cost reported per path, nothing leaving the machine on the local path | `GeminiVideoAnalyzer`; `FramesAnalyzer` with a `VisibilityPass` |

### What Stands In for What

| Spec 3.7 asks for | The scenario uses | Why |
|-------------------|-------------------|-----|
| A user's RF-DETR fine-tune in a manifest the user wrote (S9) | A manifest the scenario writes into a user manifest directory, naming the built-in RF-DETR Nano's file by revision and SHA-256; it is loaded with `ManifestSet::with_dir`, verified on fetch, run on ONNX Runtime on each woken frame, where it finds the clip's wall clock | No fine-tune is published, and the project hosts no weights (Q6). A user's export and labels go in the same manifest |
| The fine-tune's crack class (S9–S12) | `BeltDetector`, colour segmentation that finds the printed parts, the dark cracks across them and the fiducial | COCO weights cannot see a crack |
| An hour of camera video above 20 MB (S13) | A 227 KB, twenty-minute clip of the same scene, sent through the File API with the inline limit set to 0 | The File API path is what a real clip takes; the fixture stays small |
| Local Qwen3-VL-4B (S13) | A scripted transcript of the visibility pass's requests and answers in Qwen3-VL's form | The 8.9 GB weights need 14 GiB free; the machine that wrote the scenario had less. `adk-vision`'s `video_local_live_tests` runs the model itself |

### Recording Again

The recordings live in `examples/vision/fixtures/recorded/` and `fixtures/conveyor/recorded/`,
written by the proxies beside them, which keep no request header and refuse to save a record
holding a credential. A scenario sends to a proxy instead of its local server when
`VISION_RECORD_<NAME>` names one:

```bash
python3 -I adk-vision/tests/fixtures/video/recorded/proxy.py 18761 https://api.meta.ai /tmp/rec-sam &
python3 -I adk-vision/tests/fixtures/video/recorded/proxy.py 18762 https://generativelanguage.googleapis.com /tmp/rec-gemini &
VISION_RECORD_META=127.0.0.1:18761 VISION_RECORD_GEMINI=127.0.0.1:18762 META_API_KEY=… GEMINI_API_KEY=… \
  cargo run --manifest-path examples/vision/Cargo.toml -- s11_a_budget_per_frame s13_native_video_two_ways
```

S11's escalation clips are cut from the conveyor clip once with a user-installed ffmpeg and
committed, so the scenario needs no encoder:

```bash
cargo run --manifest-path examples/vision/Cargo.toml -- --write-s11-clips
```

## Related

- [Getting started: a Document desk](getting-started.md)
- [Streams](streams.md)
- [Watch conditions and visual wakes](wake.md)
- [Calibration and measurement](measure.md)
- [Video and tracking](video.md)
- [The live camera surface](../surfaces/live-camera.md)
