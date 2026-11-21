//! Live video analysis on the generated aisle clips (`tests/fixtures/video/labels.json`):
//! `sam-3.1` video tracking the forklift across the crossing clip, and Gemini native video
//! answering when the forklift enters the aisle through the File API (S13).
//!
//! `#[ignore]`d; each needs its key. `ADK_VISION_SAM_BASE` and `ADK_VISION_GEMINI_BASE` point
//! the clients at a recording proxy (`tests/fixtures/video/recorded/proxy.py`).
//!
//! ```text
//! META_API_KEY=... cargo test -p adk-vision --features cloud,sam,gemini-video \
//!     --test video_live_tests -- --ignored --nocapture
//! ```

use std::path::Path;
use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_core::{
    BudgetTracker, InMemorySpendLedger, ModelCallScope, RunBudget, SpendKey, SpendLedger,
    SpendPeriod, UsageUnit,
};
use adk_model::GeminiModel;
use adk_vision::sam::{SamConfig, SamVideoTracker};
use adk_vision::{
    EgressAction, EgressConfig, EgressGate, GeminiVideoAnalyzer, InMemoryEgressRecorder,
    MediaResolution, Sampling, SegmentPrompt, SourceClip, VideoRequest, VideoSource, VisionCx,
    VisionRuntime,
};

fn clip(name: &str) -> SourceClip {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/video").join(name);
    SourceClip::decode(std::fs::read(path).unwrap()).unwrap()
}

fn key(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|k| !k.is_empty())
}

fn metered() -> (VisionCx, Arc<BudgetTracker>, Arc<InMemorySpendLedger>) {
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let cx = VisionCx::new().with_scope(
        ModelCallScope::new().with_budget(budget.clone()).with_spend_ledger(ledger.clone(), "acme"),
    );
    (cx, budget, ledger)
}

fn gate(policy: EgressPolicy) -> EgressGate {
    EgressGate::new(
        policy,
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    )
}

#[tokio::test]
#[ignore = "live: needs META_API_KEY"]
async fn sam_video_tracks_the_forklift_across_the_crossing() {
    let Some(key) = key("META_API_KEY") else { return };
    let mut config = SamConfig::new(key);
    let mut policy = EgressPolicy::public_only();
    if let Ok(base) = std::env::var("ADK_VISION_SAM_BASE") {
        config = config.with_base_url(base);
        policy = EgressPolicy::allow_private_networks();
    }
    let runtime = VisionRuntime::builder()
        .remote_video_analyzer(Arc::new(SamVideoTracker::new(config).unwrap()))
        .egress_gate(gate(policy))
        .build()
        .unwrap();
    let crossing = clip("forklift-crossing-3s.mp4");
    let (cx, budget, ledger) = metered();
    let request = VideoRequest::Track {
        prompts: vec![SegmentPrompt::Concept {
            // "orange forklift" matches nothing in the drawn scene (recorded 2026-11-21).
            text: std::env::var("ADK_VISION_SAM_CONCEPT")
                .unwrap_or_else(|_| "orange rectangle".into()),
        }],
    };
    let out = runtime
        .analyze_video(&cx, VideoSource::clip(&crossing, Sampling::at(10.0)), &request)
        .await
        .unwrap();
    let frames = &out.value.frames;
    println!("frames with the forklift: {}", frames.len());
    for frame in frames.iter().step_by(7) {
        let r: Vec<_> = frame.regions.iter().map(|r| (r.track, r.shape.bbox())).collect();
        println!("frame {} at {:?} ms: {r:?}", frame.index, frame.pts_ms);
    }
    let spent = ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap();
    println!(
        "ledger {spent} µUSD, frames {}, usage {:?}",
        budget.units().get(UsageUnit::Frames),
        out.value.usage
    );
    assert_eq!(frames.len(), 30, "the forklift is in every frame");
    // The forklift's orange body: left edge at -40 + 4 px per frame (clipped at 0), 141 px
    // wide, from y = 175 to 261. Its track keeps one identifier through all 30 frames.
    let body = |index: u64| {
        let left = -40.0 + 4.0 * index as f32;
        adk_vision::BBox::new(left.max(0.0), 175.0, left + 141.0, 261.0)
    };
    let forklift = frames[0]
        .regions
        .iter()
        .find(|r| r.shape.bbox().is_some_and(|b| b.iou(&body(0)) > 0.8))
        .and_then(|r| r.track)
        .expect("a region over the forklift's body");
    for frame in frames {
        let region = frame.regions.iter().find(|r| r.track == Some(forklift)).expect("tracked");
        let b = region.shape.bbox().unwrap();
        let want = body(frame.index);
        assert!(b.iou(&want) > 0.9, "frame {}: {b:?} against {want:?}", frame.index);
        assert!((b.x0 - want.x0).abs() <= 2.0, "frame {}: {b:?}", frame.index);
    }
    assert_eq!(spent, 10_000, "30 frames bill the 50-frame minimum");
}

#[tokio::test]
#[ignore = "live: needs GEMINI_API_KEY (quota-limited)"]
async fn gemini_answers_when_the_forklift_enters_the_aisle() {
    let Some(key) = key("GEMINI_API_KEY").or_else(|| key("GOOGLE_API_KEY")) else { return };
    let name =
        std::env::var("ADK_VISION_GEMINI_MODEL").unwrap_or_else(|_| "gemini-3.8-flash".into());
    let model = match std::env::var("ADK_VISION_GEMINI_BASE") {
        Ok(base) => GeminiModel::new_with_base_url(key, &name, base).unwrap(),
        Err(_) => GeminiModel::new(key, &name).unwrap(),
    };
    let analyzer = GeminiVideoAnalyzer::new(Arc::new(model))
        .with_media_resolution(MediaResolution::Low)
        .with_inline_limit(0);
    let runtime = VisionRuntime::builder()
        .remote_video_analyzer(Arc::new(analyzer))
        .egress_gate(gate(EgressPolicy::public_only()))
        .build()
        .unwrap();
    let aisle = clip("aisle-forklift-120s.mp4");
    let (cx, _, ledger) = metered();
    let ask = VideoRequest::Ask { question: "When does the forklift enter the aisle?".into() };
    let out = match runtime
        .analyze_video(&cx, VideoSource::clip(&aisle, Sampling::at(1.0)), &ask)
        .await
    {
        Ok(out) => out,
        Err(err) => panic!("gemini: {err}"),
    };
    println!("answer: {:?}", out.value.answer);
    println!("events: {:?}", out.value.events);
    println!("usage: {:?}", out.value.usage);
    println!(
        "ledger {} µUSD",
        ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap()
    );
    let first = out.value.events.first().expect("an event");
    assert!(first.start_ms.abs_diff(45_000) <= 5_000, "{first:?} is not within 5 s of 0:45");
}
