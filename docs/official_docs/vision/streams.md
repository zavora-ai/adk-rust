# Streams

`adk-vision`'s `stream` module turns cameras, video files, screens, browser pages and the
web home's camera into decoded frames, and runs each stream through one fixed pipeline:
sampling, the frame governor, a privacy step, detection, tracking, and an observation sink.
Frames stay in memory; only the frames a wake, a card or a person keeps are stored, after
their privacy step, for 30 days by default. It implements spec 3.7 §7.9 and §7.13.

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["stream", "rtsp", "ffmpeg", "device"] }
```

## Sources

| Source | Feature | Ingest | Decode |
|--------|---------|--------|--------|
| `RtspSource` | `rtsp` | RTSP cameras through `retina` (MIT OR Apache-2.0): H.264, H.265, MJPEG | MJPEG in process; H.264 and H.265 through a user-installed `ffmpeg` (`ffmpeg` feature) |
| `FileSource` | `ffmpeg` | MP4 and MOV files | A user-installed `ffmpeg`, spawned through `ffmpeg-sidecar` |
| `DeviceSource` | `device` | A camera on this machine: AVFoundation (macOS), v4l2 (Linux), DirectShow (Windows) | The same user-installed `ffmpeg` |
| `ScreenSource` | `screen` | A `ComputerActuator` screenshot, or a computer-use-mcp observation through a `VisualObserver` — `RuntimeObserver` and `screen_source` (with feature `computer-use`) over any `adk-computer-use` `ComputerUseRuntime` — at an interval | PNG, JPEG |
| `BrowserSource` | `browser` | `adk-browser`'s `browser_screenshot` tool at an interval | PNG |
| `SurfaceCameraSource` | `surface-camera` | Frames the web home pushes through a `CameraFeed` | JPEG |

Every source implements `FrameSource`:

```rust
use adk_vision::stream::FrameStream;
use adk_vision::{FrameSourceId, Result, Sampling};

#[async_trait::async_trait]
pub trait FrameSource: Send + Sync {
    /// Names the source in frames, records and wakes; never holds a credential.
    fn id(&self) -> &FrameSourceId;
    /// Decoded frames at the requested rate and size.
    async fn frames(&self, sampling: Sampling) -> Result<FrameStream>;
}
```

A source holds at most four decoded frames for a consumer that falls behind and drops the
oldest, so the consumer always sees the newest frame. A video file is the exception: it
waits for its consumer, so an analysis of a file sees every sampled frame. `FileSource::paced`
plays a file at its own rate instead, like a camera. Dropping a `FrameStream` stops the
source and kills any `ffmpeg` it spawned.

### `ffmpeg`

H.264, H.265, video files and local cameras decode through an `ffmpeg` binary you install.
Nothing is vendored, linked or downloaded: `ffmpeg-sidecar` is built with its download feature
off. `ADK_VISION_FFMPEG` names the binary; `ffmpeg` on the `PATH` is used otherwise. A missing
binary fails the source with the commands that install it (`brew install ffmpeg`,
`sudo apt install ffmpeg`, `winget install ffmpeg`).

### Local cameras

`DeviceSource` (feature `device`, which implies `ffmpeg`) captures a camera on this machine
through the platform's capture interface:

| Platform | ffmpeg input | A device is | Listed by |
|----------|--------------|-------------|-----------|
| macOS | `-f avfoundation` | an index or a name | `ffmpeg -f avfoundation -list_devices true -i ""` |
| Linux | `-f v4l2` | `/dev/videoN`, its index `N`, or its card name | `ffmpeg -sources v4l2` (v4l2 has no `-list_devices`) |
| Windows | `-f dshow` | a name, DirectShow's alternative name, or an index among the video devices | `ffmpeg -f dshow -list_devices true -i dummy` |

`DeviceSource::list` runs the listing and returns each video device's index, name, kind
(`Camera`, or `Screen` for AVFoundation's `Capture screen N`) and the `DeviceSelector` that
opens it; audio devices are left out. Listing needs no camera permission.

```rust
use adk_vision::stream::{DeviceSelector, DeviceSource, FrameSource};
use adk_vision::{FrameSourceId, Sampling};
use futures::StreamExt;

async fn desk_camera() -> adk_vision::Result<()> {
    let devices = tokio::task::spawn_blocking(DeviceSource::list).await.expect("listing ran")?;
    for device in &devices {
        tracing::info!(device.index = device.index, device.name = %device.name, "camera found");
    }
    let camera = DeviceSource::new(FrameSourceId("desk".into()), DeviceSelector::Index(0))
        .with_frame_rate(30.0)
        .with_video_size(1280, 720);
    let mut frames = camera.frames(Sampling { fps: 2.0, max_long_edge: Some(640) }).await?;
    while let Some(frame) = frames.next().await {
        let frame = frame?;
        tracing::info!(frame.index = frame.index, "frame");
    }
    Ok(())
}
```

- **Capture mode.** The camera opens at `with_frame_rate` (30 a second by default; AVFoundation's
  own default, 29.97, is a rate few webcams offer exactly) and `with_video_size` (the camera's
  choice by default). Both must be a mode the camera offers.
- **Sampling.** ffmpeg samples at the stream's rate when it is below the capture rate and scales
  down to `max_long_edge`, so only the kept frames cross the pipe; sampling at or above the
  capture rate delivers every captured frame and repeats none. Presentation times count from
  the start of capture; `captured_at` is when each frame arrived.
- **Backpressure.** A consumer that falls behind sees the newest frames: the source holds four
  and drops the oldest, and ffmpeg keeps capturing.
- **Stopping.** Dropping the stream kills the `ffmpeg` child.
- **A stand-in.** `DeviceSource::test_pattern` captures ffmpeg's `testsrc2` at the requested size
  and rate, paced like a camera, for tests and demos on a machine with no camera.

> **Note:** on macOS the first capture asks for camera permission for the application that
> runs the process, usually the terminal. Until it is allowed in System Settings → Privacy &
> Security → Camera, the camera refuses to open and the error says so; run again once it is
> allowed. Capturing a `Capture screen N` device needs Screen Recording permission instead.

`adk-rust vision sources list` and `sources test <device>` run both from the terminal
([CLI](cli.md#vision-sources)).

### Camera credentials

A user name and password in an RTSP URL move into the source when it is built and leave
the URL, so they never appear in the source's identifier, its `Debug` output, logs, audit
records or errors. `RtspSource::with_credential_broker` asks a `CredentialBroker` for the
password at every session start instead.

```rust
use adk_vision::FrameSourceId;
use adk_vision::stream::RtspSource;

fn camera() -> adk_vision::Result<RtspSource> {
    let camera = RtspSource::new(FrameSourceId("line-1".into()), "rtsp://admin:hunter2@10.0.0.8/stream1")?;
    assert_eq!(camera.url().as_str(), "rtsp://10.0.0.8/stream1");
    assert!(!format!("{camera:?}").contains("hunter2"));
    Ok(camera)
}
```

### The web home camera

`adk-vision` does not depend on `adk-surface`. `SurfaceCameraSource::new` returns the
source and a `CameraFeed`, which the surface's frame handler calls: `push_frame` with each
JPEG the browser sends, `switch` with the camera switch. While the camera is off, or no
stream reads the source, a pushed frame is dropped on the server at once, and switching
off drops the frames already queued.

## Sampling

`Sampling { fps, max_long_edge }` is what the source delivers. `Sampler` keeps frames on a
grid of one slot per `1 / fps` by presentation time, with an eighth of an interval of slack
for jitter, and scales frames down — never up — to `max_long_edge`. A source more than one
slot behind re-anchors the grid rather than bursting to catch up. MJPEG cameras are sampled
before decoding; H.264 and H.265 decode every access unit, since later frames depend on
earlier ones, and are sampled after.

## The Frame Governor

A stream with a `FrameBudget` runs through a `FrameGovernor`:

| Field | Caps |
|-------|------|
| `local_fps`, `min_local_fps` | The local rate, and the lowest rate before the source pauses |
| `local_frames_per_period` | Frames processed locally per period |
| `escalations_per_minute` | Escalation requests in any 60 seconds |
| `escalated_frames_per_period` | Frames billed for escalations per period, each request at least `min_frames_per_escalation` (50, `sam-3.1` video's minimum) |
| `period_secs` | The quota period, aligned to the Unix epoch: a day starts at 00:00 UTC |

Under pressure it steps down in a fixed order:

1. **Lower the local rate.** When the local quota would not last the rest of the period at
   the current rate, the rate halves until it would, down to `min_local_fps`.
2. **Stop escalating.** At the floor with the quota still short, or when the escalation
   quota cannot cover another request.
3. **Pause the source.** When the local quota is spent. A source is never paused above the
   floor or with escalation on: reaching the quota steps through both first.

When the next period starts, the governor resumes the source, restores the rate and resumes
escalation, each as its own step. Every step writes a `vision.governor.step` record; a pause
and a resume also write `vision.source.paused` and `vision.source.resumed`. The governor
meters the time frames were captured, so a video file is metered by its own media time.

Escalations go through the same governor, from whatever escalates ambiguous tracks:

```rust
use adk_vision::stream::{EscalationVerdict, StreamPipeline};
use adk_vision::VisionCx;

async fn escalate(pipeline: &StreamPipeline, cx: &VisionCx, frames: u64) -> adk_vision::Result<bool> {
    let governor = pipeline.governor().expect("the pipeline has a budget");
    Ok(matches!(governor.request_escalation(cx, frames).await?, EscalationVerdict::Granted { .. }))
}
```

## The Privacy Step

Every frame passes a `PrivacyHook` before any other stage sees it. The builder requires one:
there is no default, so a pipeline that redacts nothing says so with `NoRedaction`, and kept
frames record `none` as their privacy step. `RedactorPrivacy` runs any on-device egress
`Redactor` and hides what it finds:

| `RedactionMethod` | Hides each box by |
|-------------------|-------------------|
| `Fill` | Solid black |
| `Blur { sigma }` | A Gaussian of `sigma` pixels |
| `ScaledBlur { fraction }` | A Gaussian whose standard deviation is `fraction` of the box's longer side; `FACE_BLUR` is a quarter |
| `Pixelate { block }` | Squares of `block` pixels |

`RedactorPrivacy::blur_faces(fetcher)` (feature `onnx`) is the face blur of spec 3.7 S18 and
the hook a camera that can see people uses: `FaceRedactor::on_device` — YuNet for faces,
RF-DETR Nano for each person YuNet finds no face for — under `FACE_BLUR`. A face is blurred
and the rest of its person stays; a person seen from behind is blurred whole. Its name, and
the privacy step kept frames record, is `redactor:faces`. The models load on the first frame,
each verified against its SHA-256; see [Image Privacy](../security/image-privacy.md#what-is-redacted)
for the redactor and the face detector's licence.

```rust
use std::sync::Arc;

use adk_vision::stream::{RedactorPrivacy, RtspSource, StreamPipeline};
use adk_vision::{FetchConfig, InMemoryAcceptanceStore, ModelFetcher};

fn blurred(camera: Arc<RtspSource>) -> adk_vision::Result<StreamPipeline> {
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    StreamPipeline::builder(camera, Arc::new(RedactorPrivacy::blur_faces(fetcher))).build()
}
```

YuNet finds faces from about 10 px to most of the frame (the hook runs it at the full input
and at half of it), and on drawn portraits 60 to 1,400 px tall the blur leaves no face that
YuNet finds again from a score of 0.3. Scenario S18 of `examples/vision` streams two drawn
passers-by across the conveyor and checks the store: every face was blurred, YuNet finds none
in any stored frame, each image was written after its frame's blur, and no frame that showed
a face reached the store as captured.

`FramePrivacy::apply` is the only way to make a `RedactedFrame`, and `FrameKeeper` stores
only `RedactedFrame`s, so a frame is redacted before any artifact write and an unredacted
frame is never persisted. A hook that fails drops its frame: nothing downstream sees it.

## Retention

Frames are ephemeral unless they become a wake payload, evidence on a card, or a sample a
person kept. `FrameKeeper` stores those for one source in one scope:

| `KeepReason` | Default retention |
|--------------|-------------------|
| `WakePayload` | 30 days |
| `CardEvidence` | 30 days |
| `PersonKept` | 30 days |

`RetentionPolicy::with_reason` changes one. A frame is stored as PNG by its digest, so a
frame kept for a wake and then for a card is one artifact with both reasons, expiring at the
later date. The keeper's index is an artifact in the same scope, so a restarted process
still purges what an earlier one kept. `purge_expired` deletes what has run out and `erase`
deletes everything the source kept in the scope; each keep writes `vision.evidence.kept` and
each deletion `vision.evidence.purged`, with the frame's digest, never its pixels.

## The Pipeline

```rust
use std::sync::Arc;

use adk_vision::stream::{
    FrameBudget, NoRedaction, Observation, ObservationSink, RtspSource, StreamPipeline,
};
use adk_vision::{DetectQuery, FrameSourceId, GreedyIouTracker, Sampling, VisionCx, VisionRuntime};

/// Prints what the detector found on each frame.
struct Print;

#[async_trait::async_trait]
impl ObservationSink for Print {
    async fn observe(&self, _cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        for region in &observation.regions {
            println!("frame {}: {:?} track {:?}", observation.frame.index, region.label, region.track);
        }
        Ok(())
    }
}

async fn inspect(runtime: Arc<VisionRuntime>, url: &str) -> adk_vision::Result<()> {
    let camera = Arc::new(RtspSource::new(FrameSourceId("line-1".into()), url)?);
    let summary = StreamPipeline::builder(camera, Arc::new(NoRedaction))
        .sampling(Sampling { fps: 5.0, max_long_edge: Some(960) })
        .budget(FrameBudget::new(5.0).with_min_local_fps(1.0).with_local_quota(200_000))
        .detector(runtime, DetectQuery::Classes(vec!["person".into()]))
        .tracker(Box::new(GreedyIouTracker::new(0.3, 5)))
        .sink(Arc::new(Print))
        .build()?
        .run(&VisionCx::new())
        .await?;
    println!("{} frames observed", summary.observations);
    Ok(())
}
```

| Stage | What it does |
|-------|--------------|
| Source and sampler | Frames at the sampling rate and size |
| Frame governor | Thins or pauses frames to the budget; records each change |
| Privacy | Redacts every frame first; a failing hook drops the frame |
| Detector | `VisionRuntime::detect_with`: metered in `UsageUnit::Frames`, regions in source pixels |
| Tracker | Anonymous track identifiers, reset when the stream starts and when it stops |
| Sink | `ObservationSink::observe` with the frame, its digest, regions and calls |

An `Observation` carries the source session (one per run, the scope its track identifiers
reset with), the redacted frame (`image`), the digest of its PNG encoding — the regions' image
and the digest the frame is stored under if kept — the regions with their track identifiers,
the tracks the tracker still holds (`tracks`: seen in the frame or kept through a brief miss,
`Tracker::live_tracks`), and the detector's metered calls. `ObservationSink::source_event`
receives `Started`, each `Governor` step, `Frozen`, `Thawed` and `Stopped`.

The run writes `vision.source.started` and `vision.source.stopped` to the audit trail of the
`VisionCx` it runs under. The kill switch pauses a stream at its next frame: while the run's
governance control is frozen, frames are dropped unprocessed, and the pipeline records
`vision.source.paused`, then `vision.source.resumed` after the thaw. A run that starts frozen
is refused.

### Metering

Each detector call the pipeline makes is metered as one frame (`UsageUnit::Frames`), not as
an image, so a stream counts against frame caps — `RunBudget::max_units(UsageUnit::Frames,
..)` and the kernel's daily caps — and leaves image caps to the vision tools (spec 3.7 X3,
§7.9). An on-device detector records its frames at zero cost; a backend priced per image is
billed one unit per frame. Any runtime call can be metered the same way with
`CallOptions::as_frames()`. Units are counted in the budget (`BudgetTracker::units`); the
spend ledger counts money.

### Watch Conditions and Wakes

`StreamWatch` is the pipeline's watch stage: an `ObservationSink` that holds watch
conditions against every observation (`Observation::tracked`, which lists the tracker's held
tracks so a frame the detector misses does not end a track) and, for each event that passes,
keeps the frame through a `FrameKeeper` as its wake payload — stored redacted, once however
many events it carries — names the kept artifact in the event, writes `vision.watch.fired`,
and delivers the event to a `WatchSink` such as `adk-kernel`'s `VisionWakeSource`:

```rust
use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_vision::stream::{FrameKeeper, NoRedaction, RtspSource, StreamPipeline, StreamWatch};
use adk_vision::{
    DetectQuery, FrameSourceId, GreedyIouTracker, MediaScope, Sampling, VisionCx, VisionRuntime,
    WatchCondition, WatchEvaluator, WatchSink,
};

async fn watch(
    runtime: Arc<VisionRuntime>,
    url: &str,
    wakes: Arc<dyn WatchSink>,
) -> adk_vision::Result<()> {
    let line = FrameSourceId("conveyor-1".into());
    // Wake frames are kept where the woken agent's vision tools load them.
    let keeper = FrameKeeper::new(
        Arc::new(InMemoryArtifactService::new()),
        MediaScope::new("inspector", "line-1", "shift-1"),
        line.clone(),
    );
    let conditions = vec![WatchCondition::new("wall-clock", ["clock"]).with_min_frames(3)];
    let watch = StreamWatch::new(WatchEvaluator::new(conditions)?, Arc::new(keeper), wakes);
    StreamPipeline::builder(Arc::new(RtspSource::new(line, url)?), Arc::new(NoRedaction))
        .sampling(Sampling::at(2.0))
        .detector(runtime, DetectQuery::Classes(vec!["clock".into()]))
        .tracker(Box::new(GreedyIouTracker::new(0.3, 4)))
        .sink(Arc::new(watch))
        .build()?
        .run(&VisionCx::new())
        .await?;
    Ok(())
}
```

Conditions can be added and removed while the stream runs (`StreamWatch::add_condition`,
`remove_condition`); the others keep their streaks. `StreamWatch::with_observer` hands every
observation on to a second sink, such as the live camera surface.

## Frame Sources and `vision_watch`

`FrameSources` is the registry an operator fills and an agent watches through the
`vision_watch` tool. The operator registers each source with a `SourceSpec` — the source,
its privacy hook, the keeper, the wake sink, and optionally a description, the detector and
its query, a `TrackerFactory`, the sampling, a budget and an observer — and the agent names
it; the camera URL and its credentials never reach a tool call, a record or a wake.

```rust
use std::sync::Arc;

use adk_vision::stream::{FrameKeeper, FrameSource, FrameSources, NoRedaction, SourceSpec};
use adk_vision::{
    DetectQuery, FrameSourceId, GreedyIouTracker, Sampling, Tracker, VisionCx, VisionRuntime,
    WatchCondition, WatchSink,
};

async fn register(
    camera: Arc<dyn FrameSource>,
    keeper: Arc<FrameKeeper>,
    wakes: Arc<dyn WatchSink>,
    runtime: Arc<VisionRuntime>,
    streams: VisionCx,
) -> adk_vision::Result<FrameSources> {
    let sources = FrameSources::new(streams);
    sources.register(
        SourceSpec::new(camera, Arc::new(NoRedaction), keeper, wakes)
            .describe("Line 1 conveyor, top camera")
            .detector(runtime, DetectQuery::Classes(vec!["person".into(), "clock".into()]))
            .tracker(Arc::new(|| Box::new(GreedyIouTracker::new(0.3, 4)) as Box<dyn Tracker>))
            .sampling(Sampling::at(2.0)),
    )?;
    // An operator can watch directly; an agent does the same through `vision_watch`.
    let condition = WatchCondition::new("wall-clock", ["clock"]);
    sources.start_watch(&VisionCx::new(), &FrameSourceId("conveyor-1".into()), condition).await?;
    Ok(sources)
}
```

| Behaviour | How |
|-----------|-----|
| Streams only while watched | The first condition on a source starts its pipeline, with a `StreamWatch` as the sink; later ones join it; removing the last stops it |
| Runs under the registry's context | Each stream runs under the `VisionCx` the registry was made with — its audit trail, metering scope and kill switch — with its own cancellation token, so it outlives the tool call that started it, and a freeze pauses it at its next frame |
| Idempotent | Registering the same condition again changes nothing; a different condition under a registered name is refused |
| Checked | A condition naming a label the source's detector never reports, or one on a source without a detector, is refused |
| Ends on its own | A file's last frame ends the stream; its conditions stay registered and the next registration starts it again; the status keeps how it ended |

`VisionToolsetBuilder::frame_sources` adds `vision_watch` to the toolset, and the
`adk-harness` vision pack takes the same registry through `VisionPack::frame_sources`:

| Argument | Action | Meaning |
|----------|--------|---------|
| `action` | all | `list`, `start` or `stop` |
| `source` | `start`, `stop` | The source's name, as `list` gives it |
| `name` | `start`, `stop` | The condition's name: part of every wake key |
| `labels`, `minScore`, `minFrames` | `start` | As `WatchCondition` takes them |
| `zone`, `zoneAnchor` | `start` | A polygon of `[x, y]` points in the frame's pixels; `center` or `bottomCenter` |
| `cooldownSeconds` | `start` | The least time between two wakes of the condition |

`list` answers each source's name, description, labels, whether it streams, its conditions,
the events its stream delivered and how its last stream ended; `start` and `stop` answer what
changed (`registered`, `unchanged`, `removed`, `notWatched`) and the source after it. The tool
is `Idempotent`; the harness classes `list` as `Read` and `start` and `stop` as `External`,
`Idempotent`, from a trusted origin.

## Fixtures

The `stream-fixtures` feature holds what the tests and the phase B scenarios stream:

| Fixture | What it is |
|---------|------------|
| `ConveyorClip` | 640×360 at 10 fps, about five minutes: 200 printed parts, 12 with cracks of seeded length (4–14 mm), a 24 mm fiducial in the belt's plane, and a wall clock above the line; drawn per frame from a seed, with its ground truth |
| `RtspTestServer` | An RTSP server on `127.0.0.1` in the test process: MJPEG (RFC 2435) encoded in process, or H.264 (RFC 6184) access units; real-time or faster, looping, Basic credentials |
| `H264Stream`, `write_mp4` | H.264 and MP4 encodings of any frames, made by a user-installed `ffmpeg` with libx264 |
| `draw_portrait` | A drawn head and shoulders over any frame, returning its face's box: a generated face for the privacy scenarios, not anyone's likeness |

No clip is committed or downloaded: every frame is drawn at test time.

```rust
use std::sync::Arc;

use adk_vision::stream::fixtures::{ConveyorClip, ConveyorSpec, RtspTestServer, TestVideo};

async fn serve() -> adk_vision::Result<RtspTestServer> {
    let clip = Arc::new(ConveyorClip::new(ConveyorSpec::default()));
    let render = Arc::clone(&clip);
    RtspTestServer::start(TestVideo::Mjpeg {
        render: Arc::new(move |index| render.render(index)),
        frames: clip.frame_count(),
        fps: clip.truth().spec.fps,
        quality: 85,
    })
    .await
}
```

## Audit Records

| Action | Written when | Fields |
|--------|--------------|--------|
| `vision.source.started` | A pipeline starts reading a source | source, sampling, budget, privacy hook |
| `vision.source.paused`, `.resumed` | The governor pauses or resumes the source, or the kill switch freezes or thaws | source, reason, quota state |
| `vision.source.stopped` | The source ends, the run is cancelled or a stage fails | source, cause, summary |
| `vision.governor.step` | Any governor change | source, time, change, quota state |
| `vision.evidence.kept` | A frame is kept | digest, reason, retention, expiry, scope, artifact, privacy hook, boxes redacted |
| `vision.evidence.purged` | A kept frame is deleted | digest, cause, reasons, kept and expiry times, scope, artifact |
| `vision.watch.registered` | A watch condition is registered on a source (on the caller's trail when it has one) | source, condition, condition digest, labels, minimum score and frames, zone, cooldown |
| `vision.watch.stopped` | A watch condition is removed (on the caller's trail when it has one) | source, condition, condition digest |
| `vision.watch.fired` | A condition passed and its frame was kept | source, session, condition and its digest, event, track, frame digest and index, artifact |
