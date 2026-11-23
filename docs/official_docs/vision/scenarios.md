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

The first run fetches PP-OCRv6 tiny, RF-DETR Nano, SAM 2.1 Tiny q4, YuNet and SigLIP 2 base
quantized (about 545 MB, 381 MB of it SigLIP 2) by revision and SHA-256 into the Hugging Face
cache; set `HF_HUB_CACHE` to keep them elsewhere.
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

## Phase C: the Screen Copilot

S14 copies customer C-1042 from a web CRM into a desktop form on a display at device pixel
ratio 2 — a 3024×1964 screenshot of 1512×982 points — through a harness in Ask mode with the
browser and computer-use packs. The CRM is served in process over HTTP; the browser tools read
it from a stand-in WebDriver server. The desktop shows three windows: the CRM's browser, the
customer form, and a mail window the copilot may not act in. The form has two identical Save
buttons, one under its notes and one in its footer.

| Scenario | What passes | Runs on |
|----------|-------------|---------|
| S14 Screen copilot | Every click lands inside its target: the CRM link, chosen from `browser_elements` marks at ratio 2, is clicked by selector inside its element, and every desktop click, given in the pixels of the screenshot fitted to Claude's limits (2380×1546) and grounded through `ScreenSpace`, lands inside its target in pixels and in points. Every desktop action happens inside an approved `computer_use_execute` whose reference graph previews, holds, executes once and verifies it; every browser action is a governed browser call. The copilot grounds "Save" by label and reading order and picks the notes' Save; the verifier finds no "Customer saved" on the fresh screenshot, and the copilot grounds Save again on the footer's, which saves once. The verifier reads each typed value back in its field. A step aimed at Mail's Send is blocked at preview and never runs, and the browser loads only the CRM's pages. The saved customer matches the CRM's record field for field | PP-OCRv6 tiny marks per window on the device; the harness, its browser and computer-use packs, `MarkSet`, `ScreenPlan`, `FittedActuator`, target evidence and `check_change` |

Each step's target evidence binds the screenshot's digest and states logical points at ratio 2
(V-I14). The eleven form steps are previewed on one screenshot as one `ScreenPlan` in the form's
window; before each runs, it is grounded again on a fresh screenshot and checked with
`ScreenPlan::check_call`. Each `computer_use_execute` is approved on its own: the harness card
that answers a whole plan from one decision is not wired yet ([Screen grounding](screen.md)).

### What Stands In for What in S14

| Spec 3.7 asks for | The scenario uses | Why |
|-------------------|-------------------|-----|
| A browser showing the web CRM | A stand-in WebDriver server (`examples/vision/src/crm.rs`) that loads each page from the CRM over HTTP, lays it out from the layout block the CRM embeds — the same CSS rectangles the page places its elements at — and answers the W3C commands and marked scripts `adk-browser` sends, with screenshots at ratio 2 | The PR tier runs without a browser; the browser tools, the browser pack and the CRM are the real ones |
| A desktop form app driven through `computer-use-mcp` | A stand-in display (`examples/vision/src/screen.rs`) drawn by `fixtures/screen/generate.py`, whose form app focuses the input it is clicked in, draws typed text from a glyph strip and saves from its footer; `ScreenRuntime`, the `ComputerUseRuntime` the computer-use pack drives, grounds and verifies on it | No released `computer-use-mcp` serves the governed preview and execute calls (see `adk-computer-use`); the governed graph, the grounding and the verifier are the real ones |
| A model choosing marks | A stand-in planner that names each mark by its label and grounds "Save" by label and reading order | The seeded mis-grounding needs a grounder that cannot tell the two Saves apart; Claude's toolset coordinates on a fitted DPR-2 screenshot are proven live in `adk-vision`'s `screen_live_tests` |

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

## Phase C: the Inspector as a Team, Visual Memory and the Video Analyst

| Scenario | What passes | Runs on |
|----------|-------------|---------|
| S15 Perceiver, reasoner, verifier | The Inspector as a `VisionTeam` on a conveyor frame with a seeded crack, a short scratch and an operator above the belt. The perceiver holds only local vision tools, each declaring no network use, and is refused a cloud model, vision tools with a cloud backend and a tool that uploads. Only the ambiguous scratch leaves, as one crop to each model, cut and screened for faces by the egress gate, never the frame; YuNet finds the operator's face in the frame and none in what left. The reasoner and the verifier disagree, so one card holds both opinions, the stored frame with the claimed region and the crop; one approval resolves it on the resumed run with no provider asked again, and the audit records bind the card and the approval to both digests | The Inspector's detector and YuNet on the device; Claude Haiku 5.5 as the reasoner and NVIDIA NIM `nemotron-3.5-super-vl` as the verifier's model, through their real clients |
| S16 Visual memory | A week of Inspector frames and document pages, stored by one run a day, embedded with SigLIP 2 into an `adk-rag` store, captions in `adk-memory` and cameras in graph memory. "Torn seals from Tuesday" by text and by an example image each reach Recall@5 at or above the baseline in `fixtures/baselines.json` (3/3 and 2/3); every hit loads from its artifact with its digest, its caption names that artifact, and the `vision.memory.indexed` record of the run it names holds its digest | SigLIP 2 base quantized on the device |
| S17 Video analyst | An hour of warehouse video with ten labelled events, cut into 30-second chunks, each captioned from six frames, indexed in `adk-rag`, kept in `adk-memory` with its keyframe, the clip in graph memory. Every frame sent is an egress gate release; at least eight of ten answers cite a chunk whose time range holds the labelled event (nine, as the live run); the cost per hour of video is reported (32,587 µUSD) and the ledger holds the captions' and answers' cost | `VideoAnalyst` with `FramesAnalyzer`; Claude Haiku 5.5 recorded live, replayed through the real Anthropic client |

### What Stands In for What in Phase C

| Spec 3.7 asks for | The scenario uses | Why |
|-------------------|-------------------|-----|
| Live answers from the reasoner's and the verifier's models (S15) | Answers scripted in each provider's wire format, served to the real Anthropic and OpenAI-compatible clients; each fixture holds the request its client sent and says `scripted: true` | The session that wrote the scenario could not reach the provider keys. `VISION_RECORD_ANTHROPIC` and `VISION_RECORD_NIM_VLM` record both in one run (`examples/vision/fixtures/recorded/README.md`) |
| The fine-tune's crack class (S15) | `BeltDetector`, as in S9; the verifier runs it with a 0.75 score floor | COCO weights cannot see a crack |
| An hour of recorded warehouse video (S17) | `WarehouseClip`, drawn frame by frame from its time at 480×270 | The fixture needs no file and its truth is exact |
| Captioned locally (S17) | Claude Haiku 5.5, every frame through the egress gate | Qwen3-VL-4B's 8.9 GB of weights need 14 GiB free; `VideoAnalyst` takes a local `FramesAnalyzer` the same way |

## Related

- [Getting started: a Document desk](getting-started.md)
- [Streams](streams.md)
- [Watch conditions and visual wakes](wake.md)
- [Calibration and measurement](measure.md)
- [Video and tracking](video.md)
- [Screen grounding](screen.md)
- [Teams: perceiver, reasoner, verifier](teams.md)
- [Visual memory](memory.md)
- [The live camera surface](../surfaces/live-camera.md)
