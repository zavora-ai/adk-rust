# Video and Tracking

`adk-vision` analyses video two ways: a `Tracker` associates detections across the frames
of a stream into anonymous tracks, and a `VideoAnalyzer` answers questions about a clip or
tracks objects through it. Every video call goes through `VisionRuntime::analyze_video`,
which holds the call's frames against the run's budget before anything leaves the machine,
releases a clip only through the egress gate, and returns every region in source pixels
with its track. This is phase B of [spec 3.7](../../design/spec-3.7-vision.md) (§7.13).

| Feature | Adds |
|---------|------|
| `track` | `ByteTracker`: ByteTrack through `trackforge` 0.5 (MIT, pure Rust) |
| `cloud` | `FramesAnalyzer`: `Ask` over sampled frames on any vision-language model |
| `gemini-video` | `GeminiVideoAnalyzer`: `Ask` over a clip with Gemini native video (implies `cloud`) |
| `sam` | `SamVideoTracker`: `Track` over a clip with Meta `sam-3.1` video |

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["track", "cloud", "gemini-video", "sam"] }
```

## Tracking Across Frames

A `Tracker` is pure CPU: it takes one frame's regions and gives each the `TrackId` of the
object it continues. `ByteTracker` runs ByteTrack: a Kalman filter predicts every track's
box, high-scoring detections match tracks by overlap first, low-scoring ones then recover
the tracks left over, and a lost track waits `max_missed` frames to be found again, so an
object that passes behind a pillar keeps its identifier.

```rust
use adk_vision::{ByteTrackConfig, ByteTracker, FrameMeta, Region, Tracker};

/// Gives one frame's detections their tracks; call it once per sampled frame.
fn track(tracker: &mut ByteTracker, frame: &FrameMeta, detections: Vec<Region>) -> Vec<Region> {
    let tracked = tracker.update(frame, detections);
    for region in &tracked {
        println!("{:?} is track {:?}", region.label, region.track);
    }
    tracked
}

// At 5 fps, ride out two seconds of occlusion.
let mut tracker = ByteTracker::new(ByteTrackConfig::default().with_max_missed(10));
```

| Setting | Default | Meaning |
|---------|---------|---------|
| `max_missed` | 30 | Frames a lost track is kept; hidden longer, the object starts a new track |
| `high_score` | 0.5 | Detections at or above it match first |
| `new_track_score` | 0.6 | The score an unmatched detection needs to start a track |
| `match_iou`, `low_match_iou` | 0.2, 0.5 | The overlap a high- or low-scoring detection needs to continue a track |

- **Labels apart.** Each label is tracked on its own, so a person and a forklift never swap
  identifiers.
- **Anonymous identifiers.** Counters shared across labels, which restart on `reset` when
  the source session ends (V-I8). No appearance model is used.
- **Untracked regions.** A detection that starts no track and continues none comes back
  with `track: None`.

The property tests hold identifiers stable for 60 frames while every box edge jitters by up
to 4 px, and an object hidden for up to `max_missed` frames is found again under its
identifier. `GreedyIouTracker` stays as the dependency-free reference.

## Clips

A `SourceClip` is an MP4 or MOV file read once. Its container's boxes give the codec, the
coded size, the duration and the frame count without decoding a frame; anything else is
refused before it goes anywhere:

```rust,no_run
use adk_vision::SourceClip;

let clip = SourceClip::decode(std::fs::read("aisle.mp4").unwrap())?;
let info = clip.info();
println!("{} frames, {} ms, {}x{}", info.frames, info.duration_ms, info.width, info.height);
# Ok::<(), adk_vision::VisionError>(())
```

| Accepted | Refused |
|----------|---------|
| MP4 and MOV with H.264, H.265 or MPEG-4 video, up to 2 GB | Other containers, VP9, AV1, ProRes; a container that states no duration or frame count (remux with `ffmpeg -c copy -movflags +faststart`) |

## Asking About a Video

`VideoRequest::Ask` answers in words, with the events it names as time ranges:

```rust,no_run
use std::sync::Arc;
use adk_model::GeminiModel;
use adk_vision::{
    GeminiVideoAnalyzer, MediaResolution, Sampling, SourceClip, VideoRequest, VideoSource,
    VisionCx, VisionRuntime,
};

# async fn run(gate: adk_vision::EgressGate) -> adk_vision::Result<()> {
let gemini = Arc::new(GeminiModel::new(std::env::var("GEMINI_API_KEY").unwrap(), "gemini-3.8-flash").unwrap());
let runtime = VisionRuntime::builder()
    .remote_video_analyzer(Arc::new(
        GeminiVideoAnalyzer::new(gemini).with_media_resolution(MediaResolution::Low),
    ))
    .egress_gate(gate)
    .build()?;
let clip = SourceClip::decode(std::fs::read("aisle.mp4").unwrap())?;
let ask = VideoRequest::Ask { question: "When does the forklift enter the aisle?".into() };
let out = runtime.analyze_video(&VisionCx::new(), VideoSource::clip(&clip, Sampling::at(1.0)), &ask).await?;
for event in &out.value.events {
    println!("{} ms to {} ms: {}", event.start_ms, event.end_ms, event.text);
}
println!("{:?}", out.value.usage);
# Ok(())
# }
```

### Gemini Native Video

`GeminiVideoAnalyzer` sends the clip itself:

- **Up to 20 MB** the clip goes inline.
- **Above 20 MB** it is uploaded once through the Gemini File API, waited on until Gemini
  has processed it, and sent as a `fileData` part; a second question about the same clip
  reuses the upload (files expire after 48 hours). `with_inline_limit(0)` always uploads.
- **Frame rate.** The call's `Sampling::fps` goes out as the part's `videoMetadata.fps`;
  Gemini's default is 1 fps.
- **Media resolution.** `with_media_resolution` sets `generationConfig.mediaResolution`.

| Media resolution | Tokens per frame | A second of video at 1 fps, with audio |
|------------------|------------------|----------------------------------------|
| `Low` | about 66 | about 100 tokens |
| default, `Medium`, `High` | about 258 | about 300 tokens |

The runtime holds the estimate — frames at the media resolution, 32 tokens a second of
audio, the question and the output cap — before the clip is released.

### Sampled Frames on Any Model

`FramesAnalyzer` asks any vision-language model about frames a frame source already
sampled. On this machine that is Qwen3-VL through mistral.rs (`LlmDescriber::mistralrs`,
feature `mistralrs`); with a remote model each frame passes the egress gate and its
redaction like any image. The frames go in batches (8 by default), each image numbered and
stamped with its time in the prompt; the model names events by image number, so their times
come from the frames' timestamps, not from the model's arithmetic.

```rust,no_run
use std::sync::Arc;
use adk_vision::{FramesAnalyzer, LlmDescriber, VisionRuntime};

# fn build(qwen: LlmDescriber<adk_vision::SentImage>) -> adk_vision::Result<()> {
let runtime = VisionRuntime::builder()
    .video_analyzer(Arc::new(FramesAnalyzer::local(&qwen).with_frames_per_call(8)))
    .build()?;
# Ok(())
# }
```

> **Note:** On the two-minute aisle fixture sampled once every 5 s, Qwen3-VL-2B on the CPU
> places the forklift's entry at 0:45.0 in the batch that shows it, but also names an event
> in batches that do not, and asked about all 24 frames at once it answers 0:35.0 (the entry
> is at 0:45). Use a larger model for questions that need one answer across a whole clip.

## Tracking Through a Clip with `sam-3.1`

`SamVideoTracker` answers `VideoRequest::Track` with Meta's `sam-3.1` video: masks with
stable object identifiers over a clip, the escalation for a track a local detector finds
ambiguous.

| Item | Behaviour |
|------|-----------|
| Request | One `POST /v1/responses` per concept phrase, with the clip as `input_video`: a data URL up to 20 MB, else a `file_id` uploaded once through `/v1/files`; box and point prompts in one request, on frame 0 |
| Answer | One line per frame with objects; each object a region with a `TrackId` stable across the frames of the analysis, objects of different concepts never sharing one |
| Shape | A mask when `SamConfig::with_mask_decoder` (`[sam.masks]`) chooses a decoder or `with_mask_decoder` installs one, otherwise the object's box, in pixels of the clip's frames |
| Propagation | `with_propagation(Propagation::Forward)` tracks from the earliest prompted frame onward only |
| Price | $0.20 per 1,000 frames, at least 50 frames per request: a 30-frame clip costs $0.01, thirty seconds at 30 fps $0.18 |
| Limits | 15,000 frames (about 10 minutes) and 16 objects per frame per request |

## Budgets, Egress and Usage

| Step | Video calls |
|------|-------------|
| Hold | The frames the backend analyses, as `UsageUnit::Frames`: every frame of the clip for `sam-3.1`, each request at its 50-frame minimum; the sampled frames for Gemini and `FramesAnalyzer`. A per-frame backend holds its price, a token-billed one its estimate, an on-device one zero |
| Release | Sampled frames are released one by one, cropped and redacted, as images are. A clip is released by `EgressGate::release_clip`, as data class `video` unless the call names another |
| Usage | `VideoAnalysis::usage` reports frames, input and output tokens, requests and cost per call, at zero cost on the device |

`EgressGate::release_clip` runs the gate's steps for a clip:

1. **Destination.** The adk-core egress policy decides each host.
2. **Redact.** A clip is not re-encoded on the device, so a configuration that requires any
   redaction class refuses every clip (V-I2). Analyse sampled frames instead; the gate
   redacts each one.
3. **Decide.** `deny`, `ask` or `allow` for the host and data class; `ask` holds the clip
   until a person approves its digest.
4. **Record.** The receipt names the clip's digest as source and egress digest, the
   destination and the estimated cost, before the clip is returned.

```toml
[egress]
default = "ask"

[[egress.rule]]
host = "generativelanguage.googleapis.com"
classes = ["video"]
action = "allow"
```

## Gemini Request Options in `adk-model`

The Gemini client in `adk-model` carries both settings through the provider-neutral
request types:

| Setting | Where | Goes out as |
|---------|-------|-------------|
| Media resolution | `GeminiRequestOptions` in `GenerateContentConfig::extensions["gemini"]` | `generationConfig.mediaResolution` |
| Video sampling | `video_part_annotations(&VideoMetadata)` on a `Part::InlineData` or `Part::FileData` | the part's `videoMetadata` |
| A file part | `Part::FileData` with an `https://`, `http://` or `gs://` URI | native `fileData`; any other scheme stays a text reference |
| Uploads | `GeminiModel::upload_file`, `wait_for_file`, `get_file`, `delete_file` | the File API (AI Studio) |
