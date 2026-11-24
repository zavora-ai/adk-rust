# The Live Camera Surface

The live camera surface is where a person watches a camera an agent watches and acts on it
(spec 3.7 §8.2): live frames with what was found drawn over them, the anonymous tracks, the
watch conditions and their zones, the frame governor's state — how often it looks, whether it
asks a cloud model about unclear tracks, today's frames against the limit — and the source
switch. A person switches a camera off, draws or edits a zone, stops the stream and raises a card
from the current frame. It is a web home view under the agent, and a phone screen.

| Live (light) | Live (dark) |
|---|---|
| ![The line 2 conveyor live: parts tracked, a seal tear on T43 measured at 14.3 mm, the held line stop with its frame](../images/surfaces/live-camera-light.png) | ![The same view, dark](../images/surfaces/live-camera-dark.png) |

## Where It Opens

| From | How |
|---|---|
| The agent's thread | The thread of an agent that watches a camera links to it from its header: the camera's name, or "Cameras · 4 sources · 3 on" when it has several |
| An address | `/ui/cameras/<camera>` watches one camera large; `/ui/cameras` shows every camera as a grid |
| The phone | The same addresses open the phone's camera screens; **Edit the zone** opens `/ui/cameras/<camera>?zone=edit` in the web home |

The back link, **‹ Inspector**, returns to the agent's thread.

## What It Shows

| Part | Shows |
|---|---|
| The bar | The camera's name, its state as a pill, how frames arrive ("RTSP · 1920 × 1080 · on the line 2 server"), **Camera on / Off** and **Stop the stream** |
| The picture | The newest frame the server holds, with boxes and tags, masks, each track's trail over the last second, the zones and a measured length; "Live · 10:41:09" and "Looking 5 a second · overlays 0.1 s behind" over it |
| Layers | **Boxes**, **Masks**, **Tracks** and **Zones**, each switched on or off |
| **Decisions** | The agent's open decisions on the camera's session, such as a held line stop with its frame on the card ([Approve with the Image](approve-with-image.md)) |
| **Watching for** | Each watch condition: the label, the score, the frames in a row, the zone, the wait before waking again, and how often it woke the agent today |
| **Frame budget** | **Looking** (the rate), **Asking sam-3.1 about unclear tracks** (escalations a minute, today's count and cost), **Frames today** (against the daily limit); the order the governor steps down in; once it has, each step with its time and the next |
| **Tracks now** | Each anonymous track, what it is and where; a track with a finding a watch looks for first. Tracks start again with the camera |
| **Sources** | Every camera, the one shown large marked; each tile follows its camera's state |

| State | Drawn |
|---|---|
| Live | The frame with its overlays |
| Saving frames | An amber banner — "Looking 2 times a second instead of 5, and no longer asking sam-3.1 about unclear tracks. Today's frames are 96% used." — amber meters, each step with its time, the next step named |
| Paused until midnight | The last frame frozen and dimmed under "Paused until midnight"; "Parts on line 2 are passing without inspection"; every step on record; **Change the daily frame limit** |
| Can't reach the camera | The last frame grey under "Can't reach the line 2 camera", the retry countdown and attempt, the error with credentials hidden, **Try again now** |
| Camera off | A black panel: "Nothing is captured or sent. You switched line 2 off at 11:05"; the watch paused, tracks cleared |
| Paused by Stop all | The last frame dimmed under "Paused by Stop all": nothing is looked at or sent until someone chooses **Resume all** (spec 3.7 V-I17) |
| Several sources | A grid of every camera with its state, a switch list with each camera's rate and share of its frames, a budget per camera; people blurred |

Everyone in a frame is covered before the frame reaches the surface: what the camera's privacy
hook hides, and every region its detector labels `person` or `face` (spec 3.7 §7.13, Q3). A kept
frame is covered again before it is written, whatever the picture showed (V-I9).

## Acting on a Camera

| Act | What happens |
|---|---|
| **Off** | The server closes the camera's gate first: every frame that arrives from then on is dropped, and the frames it held are forgotten. Then the act is recorded (`surface.camera.switch`) and the source stops capturing: a camera on the frame pipeline closes its stream before the answer — a local camera's ffmpeg exits and its light goes out, an RTSP connection closes — recorded as `vision.source.stopped` with reason `switchedOff`; a page sending this browser's camera stops it. A source switched off sends nothing, as on the voice screen |
| **Camera on** | Recorded first, then the gate opens and the source opens again (`vision.source.started`, reason `switchedOn`) |
| **Stop the stream** | The gate closes for good and the camera's pipeline stops (`surface.camera.stop`) |
| **Stop all** | Every camera's gate closes before the agents are stopped, and each camera's pipeline drops its frames before looking at them. Each stream camera's source closes as **Off** closes it — a local camera's ffmpeg exits, an RTSP connection closes — before the stop answers, recorded as `vision.source.stopped` with reason `stopAll`. **Resume all** opens the gates again and reopens each source whose camera is on (`vision.source.started`, reason `resumeAll`); a camera a person switched off stays closed (spec 3.7 V-I17) |
| **Try again now** | The source is tried again (`surface.camera.retry`) |
| **Draw a zone** / **Edit the zone** | Corner handles over the picture, the old outline dashed, the watch that uses the zone, its corners in the camera's pixels. Saving changes the watch from its next frame and is recorded (`surface.camera.zone`, with the old and new corners). A zone changed since the person started is refused, never overwritten |
| **Raise a card from this frame** | The frame on screen is frozen: the server holds it in memory while the person writes what the agent should look at. **Raise the card** keeps that one frame as evidence for 30 days, everyone in it covered first, and sends the card to the agent as the person's message naming the frame by digest and artifact, with the regions on it. It is recorded (`surface.camera.card`) before the frame is kept |

**No frame is stored unless it is evidence on a card.** The hub holds the latest frames of each
camera in memory, about two seconds' worth, and a frozen frame for up to 15 minutes; nothing is
written. A raised card's frame is kept as `vision-<hex>.png` in the agent's session, so the
annotated viewer and an approval card open it by its digest.

## Cameras on the Engine

A camera belongs to its agent's session: only that session's owner sees or acts on it (S-I8).
The engine reads cameras through a `CameraSource` and passes every frame through the `Cameras`
hub:

```rust
use std::sync::Arc;

use adk_session::InMemorySessionService;
use adk_surface::camera::{CameraFacts, CameraSource, Cameras, HeldFrame};
use adk_surface::{LocalSource, SurfaceEngine};

/// A pipeline's cameras: their facts, and a card's frame kept the way the pipeline keeps evidence.
struct Pipeline;

#[async_trait::async_trait]
impl CameraSource for Pipeline {
    async fn cameras(&self) -> adk_surface::Result<Vec<CameraFacts>> {
        Ok(vec![CameraFacts::new("line-2", "Line 2 camera", "inspector", "s-line")])
    }

    async fn keep_frame(
        &self,
        camera: &CameraFacts,
        frame: &HeldFrame,
    ) -> adk_surface::Result<adk_core::ImageEvidence> {
        let _ = (camera, frame);
        Err(adk_surface::SurfaceError::unsupported("keeping frames"))
    }
}

fn main() -> adk_surface::Result<()> {
    let cameras = Cameras::new(Arc::new(Pipeline));
    let source = LocalSource::builder("inspector")
        .sessions(Arc::new(InMemorySessionService::new()))
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(source)).cameras(cameras).build();
    let _ = engine;
    Ok(())
}
```

| Part | What |
|---|---|
| `CameraFacts` | What a source reports: the stream's state (`Live`, `Paused`, `Lost`), the governor's numbers (`GovernorFacts`, its steps and escalation), tracks, watches and zones. Numbers and times, never sentences: the surface says them |
| `Cameras::offer` | Every frame, with what was found on it (`Overlay`): dropped when the camera is off, stopped or paused by Stop all (`Offered::Dropped`), else held in memory and shown |
| `Cameras::ingest` | A frame a screen captured — the voice screen's camera, or the camera page's **Use this browser's camera** — handed to the camera's pipeline once it passes the same gate (`CameraSource::intake`), at most `MAX_SCREEN_FPS` (10) a second |
| `Cameras::freeze_all`, `thaw_all`, `with_governance` | Stop all and Resume all, which the engine calls; and a kill switch the gate also follows |
| `CameraSource::keep_frame` | Keeps a card's frame: everyone in it covered first, written once, returned as the `ImageEvidence` a card and a held call carry |
| `PipelineCameras` (`vision-camera`) | Each camera on adk-vision's frame pipeline, below |
| `ScriptedCameras` (`web`) | A source over the boards' fixture content, every board state reachable, for tests and screenshots |
| `camera::bridge` (`voice`) | `feed_realtime` sends a camera's frames to a realtime session under the voice screen's `FramePolicy`: one a second for Gemini Live, changed frames at most every five seconds for the OpenAI Realtime API; `with_camera` turns on Gemini Live's context window compression |

## On the Frame Pipeline

With the `vision-camera` feature, `PipelineCameras` runs each camera through adk-vision's
`StreamPipeline` — sampler, frame governor, privacy hook, detector, tracker — and is the
`CameraSource` the hub reads. What the surface shows is what the pipeline measured:

| The surface shows | From |
|---|---|
| The picture and its overlays | Each observation's redacted frame as a JPEG, with its tracked regions in the camera's pixels and how long after capture they were ready |
| **Looking**, **Frames today**, the steps | The pipeline's frame governor: its rate, its count against the period's quota, and each `vision.governor.step` it took |
| **Asking sam-3.1 about unclear tracks** | Escalations the governor granted through `PipelineCameras::escalate` |
| **Tracks now** | The tracker's anonymous identifiers on the newest frame, and where each is |
| **Watching for**, the zones | Each camera's `WatchCondition`s, held against every frame; a zone saved on the surface becomes the conditions' zone from the next frame, recorded as `vision.watch.registered`; each firing is `vision.watch.fired` |
| **Can't reach the camera** | The source failing to open or ending: retried after 2 s, then twice as long each time up to 30 s, or at once from **Try again now**; the pipeline, and its governor's count, carry on through the reconnection |

A stream camera switched off stops capturing at its source: its stream is dropped before the
switch answers, so a `DeviceSource`'s ffmpeg exits and an `RtspSource`'s connection closes, and
nothing opens it until the camera is on again; `PipelineCameras::is_capturing` says whether a
camera's source is open. Stop all closes every stream camera's source the same way, before it
answers, and Resume all opens again each one whose camera is on, so a freeze leaves no camera
capturing. The voice screen's camera captures in the browser, which stops its own track; Stop
all drops its frames. In the moment between a switch or a stop and the stream closing, frames
are dropped as they arrive, before the governor counts them or the detector sees them. A camera's frames are kept only as
a card's evidence or, with `PipelineCamerasBuilder::wakes`, as a watch event's wake payload,
through adk-vision's `FrameKeeper` (`vision.evidence.kept`).

```rust
use std::sync::Arc;

use adk_surface::camera::{CameraFacts, Ingest, LiveCamera, LiveWatch, PipelineCameras, ZoneFacts};
use adk_vision::stream::{FrameBudget, NoRedaction, RtspSource};
use adk_vision::{DetectQuery, FrameSourceId, GreedyIouTracker, VisionCx, VisionRuntime, WatchCondition};

fn cameras(
    detector: Arc<VisionRuntime>,
    artifacts: Arc<dyn adk_artifact::ArtifactService>,
) -> Result<adk_surface::camera::Cameras, Box<dyn std::error::Error>> {
    let line_2 = LiveCamera::stream(
        CameraFacts::new("line-2", "Line 2 camera", "inspector", "s-line")
            .with_place("line 2")
            .with_ingest(Ingest::rtsp(1920, 1080, "the line 2 server")),
        Arc::new(RtspSource::new(FrameSourceId("line-2".into()), "rtsp://10.0.4.21:554/stream1")?),
        Arc::new(NoRedaction),
    )
    .detector(detector, DetectQuery::Classes(vec!["part".into(), "crack".into()]))
    .tracker(|| Box::new(GreedyIouTracker::new(0.3, 3)))
    .budget(FrameBudget::new(5.0).with_min_local_fps(1.0).with_local_quota(300_000))
    .zone(ZoneFacts {
        id: "inspection-window".into(),
        name: "Inspection window".into(),
        corners: [760.0, 170.0, 1560.0, 910.0],
    })
    .watch(
        LiveWatch::new("crack", "Crack", WatchCondition::new("crack", ["crack"]).with_min_frames(3))
            .in_zone("inspection-window")
            .wakes_for("a defect"),
    );
    let this_computer = LiveCamera::surface(
        CameraFacts::new("this-computer", "This computer's camera", "inspector", "s-line")
            .with_ingest(Ingest::this_computer(1280, 720)),
        Arc::new(NoRedaction),
    );
    let live = PipelineCameras::builder("inspector", "ana")
        .artifacts(artifacts)
        .cx(VisionCx::new())
        .camera(line_2)
        .camera(this_computer)
        .build()?;
    Ok(live.start())
}
```

The hub `start` returns goes to `SurfaceEngineBuilder::cameras`. `LiveCamera::surface` is the
web home's own camera (spec 3.7 §7.13 `SurfaceCameraSource`): it starts off, says it comes "from
the web home", and its frames come through `Cameras::ingest` from the voice view or from the
camera page, with no realtime model. Switching the camera on the voice screen switches it here
too, recorded as the person's act.

## This Browser's Camera

A camera that takes a screen's frames — `LiveCamera::surface`, whose view sets
`actions.sendFrames` — shows **Use this browser's camera** beside **Raise a card from this
frame**. Choosing it switches the camera on, as the person's recorded act, opens this browser's
camera and sends a JPEG of it five times a second to `POST /surface/cameras/{camera}/frames`,
one at a time. Each frame passes the hub's gate and reaches the camera's pipeline, which shows
what it makes of the frame like any other camera's: nothing is held or written on the way.

| Rule | What happens |
|---|---|
| Off sends nothing | **Off** stops this browser's camera before the server is told, and its light goes out. Switched off anywhere else — another page, a phone, Stop all — the server answers the next frame `{"taken": false, "dropped": "off"}` (or `stopped`, `frozen`) and the page stops its camera. Leaving the page stops it too |
| Owner only | Only the owner of the camera's session sends to it, from a screen (`surface.not_found` otherwise, `surface.not_here` from voice or chat); a paired phone needs the decide scope, as switching on does |
| A screen's camera only | A camera that reads its own source — RTSP, a file, a local device — answers `surface.unsupported` |
| Images only | `image/jpeg`, `image/png` or `image/webp`, its first bytes matching the type; up to `MAX_SENT_FRAME_BYTES` (2 MiB); anything else is `400 surface.invalid`, a larger body `413` |
| At most 10 a second | A frame within a tenth of a second of the last one taken is `429 surface.rate_limited`; the page skips it and sends the next |

## The Web API

| Route (under `/api`) | Answer |
|---|---|
| `GET /surface/cameras` | `CamerasView`: every camera of the caller's sessions |
| `GET /surface/cameras/{camera}` | `CameraView`: one camera with its picture, decisions, watches, budget, tracks and sources |
| `GET /surface/cameras/{camera}/frames/{frame}` | A frame the hub holds, served only when its bytes hash to the digest; `Cache-Control: private, no-store`; nothing while the camera is off |
| `GET /surface/cameras/{camera}/feed` | Server-sent events: `frame` (a `StageView`) after each frame that passes the gate, `changed` when anything else changed |
| `POST /surface/cameras/{camera}/frames` | One frame this screen's camera captured (`image/jpeg`, `image/png` or `image/webp`) → `FrameTaken`: `{"taken": true}`, or `{"taken": false, "dropped": "off"}` with the reason |
| `POST /surface/cameras/{camera}/on`, `/off`, `/stop`, `/retry` | The act, recorded; answers the `CameraView` |
| `PUT /surface/cameras/{camera}/zones/{zone}` | `ZoneChange` → `ZoneSaved`; closed to paired devices |
| `POST /surface/cameras/{camera}/freeze` | `FreezeFrame` → the frozen frame's `StageView`, held in memory for its card |
| `POST /surface/cameras/{camera}/cards` | `RaiseCard` → `CardRaised`: the kept frame's digest, its regions, until when it is kept, and what sending the card did |

A paired phone switches a camera off and stops its stream with the stop scope, and switches it
on, sends its own camera's frames, freezes a frame and raises a card with the decide scope; it
edits no zone.

## On the Phone

The stream on top; **On / Off** and **Raise a card**; the agent's decision, what it watches for,
the budget as rows and the first three tracks; **Edit the zone**, which opens the zone in the web
home, and **Stop the stream** below. `/ui/cameras` lists every camera with its switch.

## Try It

`live_camera_home` serves adk-vision's generated conveyor clip over its in-process RTSP server to
two cameras on the frame pipeline. Line 2 watches for a crack inside its inspection window at five
frames a second; line 3 is held to 2,000 frames a day, so its governor steps down at once. Raising
a card from line 2's frame sends it to the Inspector, which asks to stop the line with that frame
as the call's evidence, held for you beside the camera:

```bash
cargo run -p adk-surface --features web,vision-camera --example live_camera_home -- 8814
```

Open `http://127.0.0.1:8814/ui/cameras/line-2`. `/ui/cameras/this-computer` is the web
home's own camera: **Use this browser's camera** feeds it from this browser, with no voice
screen. Its detector is a few lines of colour
segmentation that find the clip's parts and cracks, so it needs no model weights; any adk-vision
`Detector`, such as RF-DETR, drops in the same way.

`scripted_camera_home` serves the boards' four cameras from fixtures, every board state reachable:

```bash
cargo run -p adk-surface --features web --example scripted_camera_home -- 8794
```

## Related

- [Approve with the Image](approve-with-image.md) — the held line stop's card, with its frame
- [The Annotated Viewer](annotated-viewer.md) — a kept frame opened by its digest
- [Voice and Video](voice.md) — the camera on the voice screen, under the same switch rule
