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
| `ScreenSource` | `screen` | A `ComputerActuator` screenshot, or a computer-use-mcp observation through a `VisualObserver`, at an interval | PNG, JPEG |
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
`Redactor` and fills, blurs or pixelates what it finds; over `PersonRedactor` it is the
interim face policy, every detected person's whole box. The face blur of phase C implements
the same trait.

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
| Detector | `VisionRuntime::detect_with`: metered like any vision call, regions in source pixels |
| Tracker | Anonymous track identifiers, reset when the stream starts and when it stops |
| Sink | `ObservationSink::observe` with the frame, its digest, regions and calls |

An `Observation` carries the source session (one per run, the scope its track identifiers
reset with), the redacted frame (`image`), the digest of its PNG encoding — the regions' image
and the digest the frame is stored under if kept — the regions with their track identifiers,
and the detector's metered calls. `ObservationSink::source_event`
receives `Started`, each `Governor` step, `Frozen`, `Thawed` and `Stopped`.

The run writes `vision.source.started` and `vision.source.stopped` to the audit trail of the
`VisionCx` it runs under. The kill switch pauses a stream at its next frame: while the run's
governance control is frozen, frames are dropped unprocessed, and the pipeline records
`vision.source.paused`, then `vision.source.resumed` after the thaw. A run that starts frozen
is refused.

### Watch Conditions and Wakes

`Observation::tracked` gives the `TrackedObservation` a `WatchEvaluator` takes. A sink that
holds watch conditions keeps the frame of each event as its wake payload — so the frame is
stored redacted, under the digest the event names — and hands the event, with the frame's
artifact, to a `WatchSink` such as `adk-kernel`'s `VisionWakeSource`:

```rust
use std::sync::{Arc, Mutex};

use adk_vision::stream::{FrameKeeper, KeepReason, Observation, ObservationSink};
use adk_vision::{VisionCx, WatchEvaluator, WatchSink};

struct Watch {
    evaluator: Mutex<WatchEvaluator>,
    keeper: FrameKeeper,
    wakes: Arc<dyn WatchSink>,
}

#[async_trait::async_trait]
impl ObservationSink for Watch {
    async fn observe(&self, cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        let events = self.evaluator.lock().unwrap().observe(&observation.tracked());
        for event in events {
            let kept = self.keeper.keep(cx, &observation.image, KeepReason::WakePayload).await?;
            self.wakes.deliver(&event.with_frame_artifact(kept.image.artifact)).await?;
        }
        Ok(())
    }
}
```

## Fixtures

The `stream-fixtures` feature holds what the tests and the phase B scenarios stream:

| Fixture | What it is |
|---------|------------|
| `ConveyorClip` | 640×360 at 10 fps, about five minutes: 200 printed parts, 12 with cracks of seeded length (4–14 mm), a 24 mm fiducial in the belt's plane, and a wall clock above the line; drawn per frame from a seed, with its ground truth |
| `RtspTestServer` | An RTSP server on `127.0.0.1` in the test process: MJPEG (RFC 2435) encoded in process, or H.264 (RFC 6184) access units; real-time or faster, looping, Basic credentials |
| `H264Stream`, `write_mp4` | H.264 and MP4 encodings of any frames, made by a user-installed `ffmpeg` with libx264 |

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
