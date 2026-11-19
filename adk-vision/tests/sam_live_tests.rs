//! Live `sam-3.1`: segments the drawn shapes of a 960×540 image and the twelve S5 widgets,
//! scoring boxes in source pixels and, with Meta's parser installed, masks by IoU against
//! the drawn shapes.
//!
//! `#[ignore]`d; needs `META_API_KEY`. `ADK_VISION_SAM_PARSER` names an installed
//! `@meta-sam/parser` package directory to decode masks with `node`;
//! `ADK_VISION_SAM_BASE` points the client at a recording proxy.
//!
//! ```text
//! META_API_KEY=... ADK_VISION_SAM_PARSER=.../node_modules/@meta-sam/parser \
//!     cargo test -p adk-vision --features sam --test sam_live_tests -- --ignored --nocapture
//! ```

use std::path::Path;
use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_core::{
    BudgetTracker, InMemorySpendLedger, ModelCallScope, RunBudget, SpendKey, SpendLedger,
    SpendPeriod, UsageUnit,
};
use adk_vision::sam::NodeMaskDecoder;
use adk_vision::{
    BBox, DecodeLimits, EgressAction, EgressConfig, EgressGate, InMemoryEgressRecorder, Mask,
    SamConfig, SamSegmenter, SegmentPrompt, Shape, SourceImage, VisionCx, VisionRuntime,
};
use serde_json::Value;

fn fixture(path: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(path)
}

/// The drawn shapes of `shapes-960x540.png` as masks: the rectangle, the circle and the
/// triangle, pixel centres inside each shape.
fn truth_masks() -> Vec<(&'static str, Mask)> {
    let (w, h) = (960u32, 540u32);
    let raster = |inside: &dyn Fn(f32, f32) -> bool| {
        let mut bits = vec![false; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                bits[(y * w + x) as usize] = inside(x as f32 + 0.5, y as f32 + 0.5);
            }
        }
        Mask::encode(w, h, &bits)
    };
    let tri = [[140.0f32, 480.0], [300.0, 480.0], [220.0, 330.0]];
    let in_tri = |x: f32, y: f32| {
        let sign =
            |a: [f32; 2], b: [f32; 2]| (x - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (y - b[1]);
        let (d1, d2, d3) = (sign(tri[0], tri[1]), sign(tri[1], tri[2]), sign(tri[2], tri[0]));
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    };
    vec![
        ("red rectangle", raster(&|x, y| (80.0..320.0).contains(&x) && (60.0..220.0).contains(&y))),
        ("blue circle", raster(&|x, y| (x - 640.0).powi(2) + (y - 150.0).powi(2) <= 90.0 * 90.0)),
        ("green triangle", raster(&in_tri)),
    ]
}

fn iou(a: &Mask, b: &Mask) -> f32 {
    let (a, b) = (a.decode(), b.decode());
    let inter = a.iter().zip(&b).filter(|(x, y)| **x && **y).count();
    let union = a.iter().zip(&b).filter(|(x, y)| **x || **y).count();
    if union == 0 { 0.0 } else { inter as f32 / union as f32 }
}

#[tokio::test]
#[ignore = "live: needs META_API_KEY"]
async fn sam_segments_the_drawn_shapes_and_the_s5_widgets() {
    let Some(key) = std::env::var("META_API_KEY").ok().filter(|k| !k.is_empty()) else { return };
    let mut config = SamConfig::new(key);
    let mut policy = EgressPolicy::public_only();
    if let Ok(base) = std::env::var("ADK_VISION_SAM_BASE") {
        config = config.with_base_url(base);
        policy = EgressPolicy::allow_private_networks();
    }
    let mut sam = SamSegmenter::new(config).unwrap();
    let decoding = std::env::var("ADK_VISION_SAM_PARSER").ok();
    if let Some(package) = &decoding {
        sam = sam.with_mask_decoder(Arc::new(NodeMaskDecoder::new(package)));
    }
    let gate = EgressGate::new(
        policy,
        EgressConfig::default().with_default(EgressAction::Allow),
        Arc::new(InMemoryEgressRecorder::new()),
    );
    let runtime =
        VisionRuntime::builder().remote_segmenter(Arc::new(sam)).egress_gate(gate).build().unwrap();
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let ledger = Arc::new(InMemorySpendLedger::default());
    let cx = VisionCx::new().with_scope(
        ModelCallScope::new().with_budget(budget.clone()).with_spend_ledger(ledger.clone(), "live"),
    );
    let load = |p: &str| {
        SourceImage::decode(&std::fs::read(fixture(p)).unwrap(), &DecodeLimits::default()).unwrap()
    };

    // Shapes: one concept per request.
    let shapes = load("cloud/shapes-960x540.png");
    let truth: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture("cloud/shapes-960x540.groundtruth.json")).unwrap(),
    )
    .unwrap();
    let masks = truth_masks();
    let prompts: Vec<SegmentPrompt> = masks
        .iter()
        .map(|(name, _)| SegmentPrompt::Concept { text: (*name).to_string() })
        .collect();
    let out = runtime.segment(&cx, &shapes, &prompts).await.unwrap();
    for (name, truth_mask) in &masks {
        let region = out.value.iter().find(|r| r.label.as_deref() == Some(*name));
        let Some(region) = region else {
            println!("LIVE sam-3.1 shapes {name}: not found");
            continue;
        };
        let want = truth["objects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["label"] == *name)
            .map(|o| {
                let b: Vec<f32> = o["bbox"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|n| n.as_f64().unwrap() as f32)
                    .collect();
                BBox::new(b[0], b[1], b[2], b[3])
            })
            .unwrap();
        let got = region.shape.bbox().unwrap();
        let err = [got.x0 - want.x0, got.y0 - want.y0, got.x1 - want.x1, got.y1 - want.y1]
            .into_iter()
            .map(f32::abs)
            .fold(0f32, f32::max);
        let mask_iou = match &region.shape {
            Shape::Mask(m) => format!("{:.4}", iou(m, truth_mask)),
            _ => "n/a (no decoder)".into(),
        };
        println!("LIVE sam-3.1 shapes {name}: box_err_px {err:.1} mask_iou {mask_iou} box {got:?}");
    }
    let calls = out.calls.len();
    let units = out.calls.iter().map(|c| c.units.get(UsageUnit::Images)).sum::<u64>();

    // S5: the twelve widgets, one concept each.
    let s5 = load("s5/s5-screen.png");
    let fixture_json: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture("s5/s5.json")).unwrap()).unwrap();
    let targets: Vec<(String, BBox)> = fixture_json["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let b: Vec<f32> =
                t["box"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
            (t["name"].as_str().unwrap().to_string(), BBox::new(b[0], b[1], b[2], b[3]))
        })
        .collect();
    // One request for every drawn widget; each target takes the region that overlaps it most.
    let out = runtime
        .segment(&cx, &s5, &[SegmentPrompt::Concept { text: "rectangle".into() }])
        .await
        .unwrap();
    let mut errors = Vec::new();
    for (name, want) in &targets {
        let best = out
            .value
            .iter()
            .filter_map(|r| r.shape.bbox())
            .max_by(|a, b| a.iou(want).total_cmp(&b.iou(want)))
            .filter(|b| b.iou(want) > 0.5);
        let err = best.map_or(f32::NAN, |got| {
            [got.x0 - want.x0, got.y0 - want.y0, got.x1 - want.x1, got.y1 - want.y1]
                .into_iter()
                .map(f32::abs)
                .fold(0f32, f32::max)
        });
        errors.push((name.clone(), err));
    }
    println!("LIVE sam-3.1 s5: {} regions for `rectangle`", out.value.len());
    let cost = ledger.spent(&SpendKey::org("live"), SpendPeriod::Day).await.unwrap();
    println!(
        "LIVE sam-3.1 s5: {errors:?}\nLIVE sam-3.1 totals: shapes calls {calls} units {units}; \
         ledger {cost} µUSD; budget images {}; masks decoded {}",
        budget.units().get(UsageUnit::Images),
        decoding.is_some()
    );
}
