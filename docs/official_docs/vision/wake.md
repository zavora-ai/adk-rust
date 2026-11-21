# Watch Conditions and Visual Wakes

An agent that watches a camera, a screen or a browser should cost nothing until something it
cares about appears. A stream's pipeline holds **watch conditions** against every tracked
object on the device, and the [Agent Kernel](../kernel/index.md) sees only the events that
pass: `VisionWakeSource` turns each into a wake that rouses a sleeping process, or starts a
triggered one, with the frame reference, track and regions in its payload. This page
implements spec 3.7 §7.10 ([spec](../../design/spec-3.7-vision.md)).

```toml
[dependencies]
adk-vision = "3.0.0"
adk-kernel = { version = "3.0.0", features = ["vision"] }
```

> **Note:** the spec's open-question defaults are in force; for streams, frames are never
> stored by default and evidence frames are kept 30 days (Q13).

## How a Frame Becomes a Wake

| Step | Where | What happens |
|------|-------|--------------|
| 1. Track | The stream pipeline | Detector and tracker give each object an anonymous `TrackId` in source pixels |
| 2. Watch | `WatchEvaluator::observe` | Each `WatchCondition` is held against every track; a `WatchEvent` comes out only when one passes |
| 3. Keep | `StreamWatch` | The event's frame becomes evidence: it is stored redacted as an artifact through a `FrameKeeper`, its locator attached (`WatchEvent::with_frame_artifact`), and `vision.watch.fired` written |
| 4. Wake | `VisionWakeSource` | The event becomes a `WakeEvent` keyed `queue:<source>:<session>:<track>:<condition>:<event>` |
| 5. Route | The kernel | A process asleep on `Wake::Queue { source }` wakes; with none, a `Trigger` on the same wake starts a `Triggered` process |
| 6. Read | The woken turn | The payload arrives as delimited data, not instructions; the agent loads the frame through the [vision tools](governed-tools.md) by the digest the payload names |

The pipeline, the watch stage and the registry an agent starts watches through are on
[Streams](streams.md#watch-conditions-and-wakes): `StreamWatch` runs steps 2 to 4 inside the
pipeline, and `FrameSources` with the `vision_watch` tool lets an agent list the operator's
sources and start or stop a condition on one.

## Watch Conditions

A region meets a condition when its label is one of the condition's labels (compared ignoring
ASCII case; no labels takes any), its score is at least the minimum (a region without a score
has none), and, with a zone, its anchor point lies in the zone. A track fires once it has met
the condition in `min_frames` consecutive observations.

| Field | Default | Meaning |
|-------|---------|---------|
| `name` | — | Part of every event key: 1 to 64 characters of `[A-Za-z0-9._-]` |
| `labels` | any | The labels that count |
| `min_score` | `0.5` | The lowest score that counts, in `[0, 1]` |
| `min_frames` | `3` | Consecutive observations a track must meet the condition in; a frame without it starts the count again |
| `zone` | the whole frame | A polygon in source pixels, and the point of each region that must lie in it: `center` or `bottomCenter` |
| `cooldown` (`cooldownMs`) | zero | The least time between two firings of the condition, by capture time |

```rust
use std::time::Duration;
use adk_vision::{WatchCondition, WatchEvaluator, Zone, ZoneAnchor};

// Torn seals on the belt, seen in three frames in a row, at most one wake every two seconds.
let belt = Zone::new(vec![[0.0, 300.0], [1280.0, 300.0], [1280.0, 720.0], [0.0, 720.0]])
    .unwrap()
    .with_anchor(ZoneAnchor::BottomCenter);
let watch = WatchEvaluator::new(vec![
    WatchCondition::new("torn-seal", ["torn seal"])
        .with_min_score(0.6)
        .with_min_frames(3)
        .with_zone(belt)
        .with_cooldown(Duration::from_secs(2)),
])
.unwrap();
assert_eq!(watch.conditions().len(), 1);
```

| Property | How it holds |
|----------|--------------|
| One track, one event | A track fires once per condition and source session, however long it stays in view: one torn seal seen for forty frames fires once |
| Cooldown | A condition with a cooldown fires at most once per cooldown; the best qualifying track fires (highest score, then lowest identifier) and the others fire after it if they still qualify. Without a cooldown every qualifying track fires |
| Idempotent | An observation whose frame index is not past the last one seen for its session — a redelivery or a late frame — changes nothing |
| Sessions | Track identifiers are anonymous and reset with their source session (V-I8): a new `session` forgets the source's state, and its events have their own keys |
| Bounded | State is kept only for tracks the observation lists as held (`TrackedObservation::with_tracks`) or shows; regions without a track are not watched |
| Pure | No clock and no I/O: time is each frame's capture time, so a recorded stream replays to the same events |

## Observations and Events

`TrackedObservation` is what the pipeline hands the evaluator for one frame:

| Field | Holds |
|-------|-------|
| `source` | The `FrameSourceId`, such as `conveyor-1` |
| `session` | The source session the track identifiers belong to |
| `frame` | `FrameRef`: index, capture time, digest of the encoded bytes, size, and the artifact once kept |
| `regions` | The frame's regions in source pixels, each with the `track` the tracker set |
| `tracks` | The tracks the tracker still holds, seen in this frame or briefly missed |

The stream pipeline's `Observation::tracked` fills `tracks` from the tracker's
`Tracker::live_tracks`, so an object the detector misses for a frame or two keeps its track
and does not fire again when it is seen once more. `WatchEvaluator::add` and `remove` change
the conditions while a stream runs; the others keep their streaks.

A `WatchEvent` names its source, session, condition, kind (`detected`), track, label, score,
the consecutive frames counted, the frame, and the regions that met the condition, the firing
track's first. `WatchEvent::key()` is `<session>:<track>:<condition>:<kind>`.
`WatchEvent::audit_draft` gives the `vision.watch.fired` record and
`watch_registered_draft` the `vision.watch.registered` one: condition digest, source, track
and frame digest, never pixels.

## The Wake Source

`VisionWakeSource` (in `adk-kernel`) is a `WakeSource` per stream, named after it. It needs no
new `Wake` variant: its events are keyed under `queue:`, which `Wake::Queue { source }`
matches. Feature `vision` of `adk-kernel` makes it an `adk_vision::WatchSink`, so the
pipeline delivers events straight to it:

```rust,no_run
use std::sync::Arc;
use adk_kernel::{Trigger, VisionWakeSource, Wake};
use adk_vision::stream::{FrameKeeper, FrameSource, FrameSources, NoRedaction, SourceSpec};
use adk_vision::{DetectQuery, FrameSourceId, MediaScope, VisionCx, VisionRuntime, WatchCondition};

# async fn run(
#     builder: adk_kernel::KernelBuilder,
#     template: adk_core::NewRun,
#     camera: Arc<dyn FrameSource>,
#     artifacts: Arc<dyn adk_artifact::ArtifactService>,
#     detector: Arc<VisionRuntime>,
# ) -> adk_vision::Result<()> {
let wakes = Arc::new(VisionWakeSource::new("conveyor-1").unwrap());
// A process asleep on the stream takes each event; with none asleep, this trigger starts one.
let builder = builder
    .wake_source(wakes.clone())
    .trigger(Trigger::new("inspect", Wake::Queue { source: "conveyor-1".into() }, template));
# let _ = builder;
// Keep wake frames in the session the sleeping process runs in.
let keeper = FrameKeeper::new(
    artifacts,
    MediaScope::new("inspector", "line-1", "shift-1"),
    FrameSourceId("conveyor-1".into()),
);
let sources = FrameSources::new(VisionCx::new());
sources.register(
    SourceSpec::new(camera, Arc::new(NoRedaction), Arc::new(keeper), wakes)
        .detector(detector, DetectQuery::Classes(vec!["clock".into()])),
)?;
let seal = WatchCondition::new("wall-clock", ["clock"]);
sources.start_watch(&VisionCx::new(), &FrameSourceId("conveyor-1".into()), seal).await?;
# Ok(())
# }
```

Without the feature, the source takes the kernel's own type, `VisualWake`, through
`VisionWakeSource::publish`, so a default kernel build compiles no perception code and any
perception stack can wake the kernel.

| Rule | Why |
|------|-----|
| A source name holds no `:`, `#` or whitespace | The kernel reads a key's source up to its first `:` |
| One source per stream | A watch event from another stream is refused |
| No principal | Standing rules and Auto stay inert for the run a camera starts, so a consequential call it makes, such as stopping the line, is held for a person |
| The frame loads by digest | The image tools resolve `sha256:<digest>` from the session's artifacts, so the keeper's `MediaScope` is the session the woken process runs in; a `Trigger` that starts a process in another session needs a keeper in that session |

### What the Woken Turn Reads

The payload is compact JSON within the kernel's `WAKE_PAYLOAD_LIMIT` (4096 bytes), delivered
between `<<<wake-payload` and `wake-payload>>>` lines as data from the event's source, not
instructions ([What a woken turn is told](../kernel/index.md#what-a-woken-turn-is-told)):

```json
{
  "kind": "vision",
  "source": "conveyor-1",
  "session": "shift-1",
  "condition": "torn-seal",
  "event": "detected",
  "label": "torn seal",
  "score": 0.91,
  "track": 7,
  "frames": 3,
  "frame": {
    "index": 1203,
    "capturedAt": "2026-11-21T09:00:00.000Z",
    "digest": "sha256:5d41…",
    "width": 1280,
    "height": 720,
    "artifact": { "appName": "inspector", "userId": "line-1", "sessionId": "shift-1", "fileName": "frame-1203.jpg", "version": 0 }
  },
  "regions": [
    { "box": [412.0, 220.5, 448.0, 251.0], "label": "torn seal", "score": 0.91, "track": 7 },
    { "box": [820.0, 300.0, 846.0, 322.0], "label": "torn seal", "score": 0.62 }
  ],
  "moreRegions": 0
}
```

| Bound | How |
|-------|-----|
| 4096 bytes | Texts are cut to 64 characters (the digest to 80) with control characters replaced, boxes rounded to a tenth of a pixel, and the regions kept are the longest prefix that fits; `moreRegions` counts the rest |
| No pixels | Masks travel as their boxes; the frame is named by digest and artifact (V-I15) |
| Once | The kernel keeps one `kernel.wake` run per key, so a redelivered event, or one track re-published on every frame, wakes once |

## Tests

| Suite | Proves |
|-------|--------|
| `adk-vision/tests/watch_property_tests.rs` | Zone containment (a rectangle holds what its box holds at any start vertex and winding; nothing beyond a polygon's bounds is inside), consecutive-frame counting against an oracle, firings a cooldown apart and once per track, redelivered and late frames change nothing, every key in a session distinct; forty frames of one seal fire once |
| `adk-vision/tests/stream_watch_tests.rs` | The pipeline through `StreamWatch`: one wake per track on its third frame, a frame the detector misses ending nothing, the kept frame loading back by its digest, the `vision.watch.fired` and `vision.evidence.kept` records, and every detector call metered as a frame; `FrameSources` streaming from the first watch to the last; `vision_watch` listing, starting and stopping a watch through `Runner` and `LlmAgent` with a scripted planner |
| `adk-harness/tests/vision_wake_tests.rs` | A kernel process asleep on a stream is woken once by a watched belt; its harness turn reads the payload and loads the kept frame through the vision pack's `vision_detect` by its digest. The ignored live run does the same with adk-vision's conveyor clip over the in-process RTSP server and RF-DETR Nano |
| `adk-kernel/tests/vision_wake.rs` | S10: a process asleep on the stream wakes once for forty frames of one seal and reads frame, digest, artifact, track and regions within 4 KiB; with no sleeper the trigger starts a `Triggered` process; a redelivery wakes nothing. With feature `vision`, the same through `WatchEvaluator` and `WatchSink`. The payload stays within the limit for any regions and texts (property test) |

## Related

- [Calibration and Measurement](measure.md): `vision_measure` on a woken frame
- [Governed Vision Tools](governed-tools.md)
- [The Agent Kernel](../kernel/index.md): sleep, wake sources and triggers
- [Vision overview](index.md)
