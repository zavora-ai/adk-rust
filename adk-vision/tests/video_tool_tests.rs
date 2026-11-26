//! `vision_video` through `Runner` and `LlmAgent` with a scripted planner: frames named by
//! artifact or digest analysed as one sampled video, each frame released through the egress
//! gate, held with every frame under a rule that asks, event times from the frames' times,
//! tracked regions tied to their frame, and the argument refusals.

mod governed;
mod support;

use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_artifact::InMemoryArtifactService;
use adk_core::egress::EgressPolicy;
use adk_core::{Evidence, RunConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DecodedImage, Digest, EgressReceipt,
    InMemoryEgressRecorder, Locality, MediaStore, RawRegion, ReleasedImage, Shape, TrackId,
    VideoAnalysis, VideoAnalyzer, VideoEvent, VideoFrame, VideoInput, VideoRequest, VisionConfig,
    VisionCx, VisionRuntime, VisionToolset,
};
use governed::{
    APP, PlannerModel, RecordingApprover, SESSION, USER, run_turn, save_artifact, tool_responses,
};
use serde_json::{Value, json};

const FRAMES: [&str; 4] = ["aisle-0.png", "aisle-1.png", "aisle-2.png", "aisle-3.png"];
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;

/// What the stand-in video model was handed for one frame.
#[derive(Clone, Debug)]
struct Seen {
    pts_ms: u64,
    bytes: Vec<u8>,
    receipt: EgressReceipt,
}

/// A remote video model standing in for Claude over sampled frames: it records each released
/// frame, answers an `Ask` with an event over frames 1 to 2, and tracks one forklift that
/// moves 100 px a frame.
struct RecordingVideo {
    descriptor: BackendDescriptor,
    seen: Mutex<Vec<Seen>>,
}

impl RecordingVideo {
    fn claude() -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "claude",
                Locality::Remote { hosts: vec!["api.anthropic.com".into()] },
                CoordFrame::SentPixels,
            ),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl VideoAnalyzer<ReleasedImage> for RecordingVideo {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn analyze(
        &self,
        _cx: &VisionCx,
        video: &VideoInput<ReleasedImage>,
        request: &VideoRequest,
    ) -> adk_vision::Result<VideoAnalysis<RawRegion>> {
        self.seen.lock().unwrap().extend(video.frames.iter().map(|(meta, image)| Seen {
            pts_ms: meta.pts_ms,
            bytes: image.bytes().to_vec(),
            receipt: image.receipt().clone(),
        }));
        let mut analysis = VideoAnalysis::default();
        match request {
            VideoRequest::Ask { .. } => {
                analysis.answer = Some("A forklift crosses the aisle.".into());
                analysis.events = vec![
                    VideoEvent::new(
                        video.frames[1].0.pts_ms,
                        video.frames[2].0.pts_ms,
                        "The forklift crosses from the left.",
                    )
                    .with_labels(["forklift"]),
                ];
            }
            VideoRequest::Track { .. } => {
                analysis.frames = video
                    .frames
                    .iter()
                    .map(|(meta, _)| {
                        let x = 20.0 + 100.0 * meta.index as f32;
                        VideoFrame {
                            index: meta.index,
                            pts_ms: Some(meta.pts_ms),
                            regions: vec![RawRegion {
                                shape: Shape::Box(BBox::new(x, 100.0, x + 80.0, 220.0)),
                                label: Some("forklift".into()),
                                score: Some(0.9),
                                track: Some(TrackId(7)),
                            }],
                        }
                    })
                    .collect();
            }
            _ => analysis.events = vec![VideoEvent::new(0, 2_000, "Nothing happens.")],
        }
        Ok(analysis)
    }
}

struct Aisle {
    artifacts: Arc<InMemoryArtifactService>,
    recorder: Arc<InMemoryEgressRecorder>,
    video: Arc<RecordingVideo>,
    toolset: VisionToolset,
    digests: Vec<Digest>,
}

/// Four frames of an aisle in the session, each a different shade so each has its own digest.
async fn aisle(toml: &str) -> Aisle {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let mut digests = Vec::new();
    for (index, name) in FRAMES.iter().enumerate() {
        let shade = 40 * (index as u8 + 1);
        let png =
            DecodedImage::filled(WIDTH, HEIGHT, [shade, shade, shade, 255]).encode_png().unwrap();
        digests.push(Digest::of(&png));
        save_artifact(&artifacts, name, &png).await;
    }
    let store = MediaStore::new(artifacts.clone());
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = adk_vision::EgressGate::new(
        EgressPolicy::public_only(),
        VisionConfig::from_toml_str(toml).unwrap().egress,
        recorder.clone(),
    )
    .with_media_store(store.clone());
    let video = Arc::new(RecordingVideo::claude());
    let runtime = VisionRuntime::builder()
        .remote_video_analyzer(video.clone())
        .egress_gate(gate)
        .media_store(store)
        .build()
        .unwrap();
    let toolset = VisionToolset::builder(Arc::new(runtime)).build().unwrap();
    Aisle { artifacts, recorder, video, toolset, digests }
}

async fn runner(aisle: &Aisle, calls: Vec<(&str, Value)>) -> (Runner, Arc<RecordingApprover>) {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let agent = LlmAgentBuilder::new("aisle")
        .model(Arc::new(PlannerModel::calling(calls)))
        .toolset(Arc::new(aisle.toolset.clone()))
        .before_tool_callback(aisle.toolset.evidence_callback())
        .build()
        .unwrap();
    let approver = Arc::new(RecordingApprover::default());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions)
        .artifact_service(aisle.artifacts.clone())
        .run_config(RunConfig::builder().tool_confirmation_handler(approver.clone()).build())
        .tool_policy(aisle.toolset.egress_policy())
        .build()
        .unwrap();
    (runner, approver)
}

fn ask(times: Value) -> Value {
    json!({
        "frames": FRAMES,
        "timesMs": times,
        "question": "Does a forklift cross the aisle?",
    })
}

#[tokio::test]
async fn the_tool_is_offered_only_when_the_runtime_analyses_video() {
    let aisle = aisle("").await;
    assert!(aisle.toolset.is_tool("vision_video"));
    assert!(adk_vision::VISION_TOOL_NAMES.contains(&"vision_video"));

    let store = MediaStore::new(Arc::new(InMemoryArtifactService::new()));
    let runtime = VisionRuntime::builder()
        .editor(Arc::new(adk_vision::DeterministicEditor::new()))
        .media_store(store)
        .build()
        .unwrap();
    let toolset = VisionToolset::builder(Arc::new(runtime)).build().unwrap();
    assert!(!toolset.is_tool("vision_video"));
}

#[tokio::test]
async fn an_allowed_call_releases_every_frame_and_times_events_by_the_frames() {
    let aisle = aisle("").await;
    let (runner, approver) =
        runner(&aisle, vec![("vision_video", ask(json!([0, 500, 1_000, 1_500])))]).await;

    let events = run_turn(&runner, "Watch the aisle frames").await;

    assert!(approver.seen.lock().unwrap().is_empty(), "allow is the default: nothing held");
    let seen = aisle.video.seen();
    assert_eq!(seen.iter().map(|s| s.pts_ms).collect::<Vec<_>>(), vec![0, 500, 1_000, 1_500]);
    for (seen, source) in seen.iter().zip(&aisle.digests) {
        assert_eq!(seen.receipt.source, *source, "frames are released in order");
        assert_eq!(Digest::of(&seen.bytes), seen.receipt.egress);
    }
    assert_eq!(aisle.recorder.released().len(), 4, "one egress record per frame");

    let response = &tool_responses(&events, "vision_video")[0];
    assert_eq!(response["text"], "A forklift crosses the aisle.");
    assert_eq!(
        response["events"],
        json!([{
            "startMs": 500,
            "endMs": 1_000,
            "text": "The forklift crosses from the left.",
            "labels": ["forklift"],
        }])
    );
    let frames: Vec<String> = aisle.digests.iter().map(ToString::to_string).collect();
    assert_eq!(response["frames"], json!(frames));
    assert_eq!(response["image"], json!(frames[0]));
    assert_eq!(response["calls"][0]["backend"], "claude");
    assert_eq!(response["calls"][0]["remote"], true);
}

#[tokio::test]
async fn under_a_rule_that_asks_the_call_is_held_with_every_frame_it_would_send() {
    let aisle = aisle("[egress]\ndefault = \"ask\"\n").await;
    let (runner, approver) =
        runner(&aisle, vec![("vision_video", ask(json!([0, 1_000, 2_000, 3_000])))]).await;

    run_turn(&runner, "Watch the aisle frames").await;

    let held = approver.seen.lock().unwrap().clone();
    assert_eq!(held.len(), 1, "one held call for the whole video");
    assert_eq!(held[0].tool_name, "vision_video");
    let evidence: Vec<Digest> = held[0]
        .evidence
        .iter()
        .map(|evidence| match evidence {
            Evidence::Image(image) => image.digest.parse().unwrap(),
            other => panic!("image evidence, not {other:?}"),
        })
        .collect();
    assert_eq!(evidence.len(), 4, "each frame is on the card");
    let sent: Vec<Digest> = aisle.video.seen().iter().map(|s| Digest::of(&s.bytes)).collect();
    assert_eq!(sent, evidence, "approval releases exactly the frames it showed");
}

#[tokio::test]
async fn tracked_regions_name_their_frame_and_track_in_source_pixels() {
    let aisle = aisle("").await;
    let args = json!({
        "frames": [FRAMES[0], FRAMES[1], FRAMES[2]],
        "intervalMs": 200,
        "concepts": ["forklift"],
    });
    let (runner, _) = runner(&aisle, vec![("vision_video", args)]).await;

    let events = run_turn(&runner, "Follow the forklift").await;

    let seen = aisle.video.seen();
    assert_eq!(seen.iter().map(|s| s.pts_ms).collect::<Vec<_>>(), vec![0, 200, 400]);
    let response = &tool_responses(&events, "vision_video")[0];
    let tracked = response["tracked"].as_array().unwrap();
    assert_eq!(tracked.len(), 3);
    for (index, frame) in tracked.iter().enumerate() {
        let x = 20.0 + 100.0 * index as f64;
        assert_eq!(frame["frame"], index);
        assert_eq!(frame["atMs"], 200 * index as u64);
        assert_eq!(frame["image"], json!(aisle.digests[index].to_string()));
        assert_eq!(
            frame["regions"],
            json!([{ "box": [x, 100.0, x + 80.0, 220.0], "label": "forklift", "score": 0.9, "track": 7 }])
        );
    }
}

#[tokio::test]
async fn a_call_names_one_request_and_one_time_per_frame() {
    let refusals = [
        (
            json!({ "frames": FRAMES, "question": "What happens?", "concepts": ["forklift"] }),
            "exactly one of `question`",
        ),
        (json!({ "frames": FRAMES }), "exactly one of `question`"),
        (json!({ "frames": FRAMES, "chunkSeconds": 0 }), "`chunkSeconds` is a number"),
        (
            json!({ "frames": FRAMES, "timesMs": [0, 1_000], "question": "What happens?" }),
            "2 times for 4 frames",
        ),
        (
            json!({ "frames": FRAMES, "timesMs": [0, 2_000, 1_000, 3_000], "question": "What happens?" }),
            "must not decrease",
        ),
    ];
    for (args, expected) in refusals {
        let aisle = aisle("").await;
        let (runner, _) = runner(&aisle, vec![("vision_video", args.clone())]).await;
        let events = run_turn(&runner, "Watch").await;
        let response = tool_responses(&events, "vision_video");
        let text = response.first().map(Value::to_string).unwrap_or_default();
        assert!(text.contains(expected), "{args} answered {text}");
        assert!(aisle.video.seen().is_empty(), "{args} sent nothing");
    }

    let aisle = aisle("").await;
    let many: Vec<&str> = FRAMES.iter().copied().cycle().take(65).collect();
    let args = json!({ "frames": many, "question": "What happens?" });
    let (runner, _) = runner(&aisle, vec![("vision_video", args)]).await;
    let events = run_turn(&runner, "Watch").await;
    let text = tool_responses(&events, "vision_video").first().map(Value::to_string);
    assert!(text.unwrap_or_default().contains("1 to 64 images"), "65 frames are refused");
    assert!(aisle.video.seen().is_empty());
}
