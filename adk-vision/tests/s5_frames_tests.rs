//! S5, one region in four frames: recorded answers for twelve targets from Gemini
//! (`box_2d` on 0-1000), Claude (pixels of the image it processed), `sam-3.1` (source
//! pixels) and RF-DETR (model-input pixels with a letterbox) all land within 2 px of the
//! truth in source pixels, and every click point mapped to screen points falls inside its
//! target (spec 3.7 S5, G3, V-I13, V-I14).
//!
//! The answers in `tests/fixtures/s5/s5.json` are synthetic, written by
//! `tests/fixtures/s5/generate.py` from the targets with each provider's quantisation.

use adk_vision::{
    AxisOrder, BBox, BackendId, CoordFrame, Digest, InputLimits, Letterbox, RawRegion,
    RegionNormalizer, ScreenSpace, Transform, TransformChain,
};
use serde_json::Value;

const FIXTURE: &str = include_str!("fixtures/s5/s5.json");

struct Truth {
    width: u32,
    height: u32,
    screen: ScreenSpace,
    targets: Vec<(String, BBox)>,
}

fn quad(value: &Value) -> [f32; 4] {
    let v: Vec<f32> =
        value.as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
    [v[0], v[1], v[2], v[3]]
}

fn truth(fixture: &Value) -> Truth {
    let image = &fixture["image"];
    Truth {
        width: image["width"].as_u64().unwrap() as u32,
        height: image["height"].as_u64().unwrap() as u32,
        screen: ScreenSpace::new(
            image["display"].as_str().unwrap(),
            [0.0, 0.0],
            image["devicePixelRatio"].as_f64().unwrap() as f32,
        ),
        targets: fixture["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let [x0, y0, x1, y1] = quad(&t["box"]);
                (t["name"].as_str().unwrap().to_string(), BBox::new(x0, y0, x1, y1))
            })
            .collect(),
    }
}

/// Maps one provider's answers to source pixels and checks them against the truth.
fn check(
    provider: &str,
    truth: &Truth,
    frame: CoordFrame,
    chain: TransformChain,
    answers: &[(String, [f32; 4])],
) -> usize {
    let normalizer =
        RegionNormalizer::new(Digest::of(b"screenshot"), chain, frame, BackendId::new(provider));
    let mut checked = 0;
    for (name, raw) in answers {
        let (_, target) = truth.targets.iter().find(|(n, _)| n == name).unwrap();
        let region = normalizer
            .region(RawRegion::boxed(frame.box_from_raw(*raw), name.clone(), 1.0))
            .unwrap()
            .unwrap();
        let found = region.shape.bbox().unwrap();
        for (got, want) in [
            (found.x0, target.x0),
            (found.y0, target.y0),
            (found.x1, target.x1),
            (found.y1, target.y1),
        ] {
            assert!((got - want).abs() <= 2.0, "{provider} {name}: {found:?} vs {target:?}");
        }
        assert!(found.is_within(truth.width, truth.height));

        // Click through ScreenSpace: the centre, in points, lands inside the target in points.
        let click = truth.screen.to_points(found.center());
        let target_points = truth.screen.bbox_to_points(target);
        assert!(
            target_points.contains(click),
            "{provider} {name}: click {click:?} misses {target_points:?}"
        );
        checked += 1;
    }
    checked
}

fn answers(list: &Value, key: &str) -> Vec<(String, [f32; 4])> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|a| (a["name"].as_str().unwrap().to_string(), quad(&a[key])))
        .collect()
}

#[test]
fn forty_eight_regions_in_four_frames_land_within_two_pixels() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let truth = truth(&fixture);
    let source = TransformChain::new(truth.width, truth.height);
    let mut total = 0;

    // Gemini: [ymin, xmin, ymax, xmax] on 0-1000 of the image as sent (here, unresized).
    total += check(
        "gemini",
        &truth,
        CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx },
        source.clone(),
        &answers(&fixture["gemini"], "box_2d"),
    );

    // Claude: the runtime resized to its 1568 px limit before sending, so Claude's pixels
    // are pixels of the image as sent and the resize is in the chain.
    let sent = InputLimits::long_edge(1568).fit((truth.width, truth.height)).unwrap();
    let recorded = &fixture["claude"]["sent"];
    assert_eq!(
        sent,
        (recorded[0].as_u64().unwrap() as u32, recorded[1].as_u64().unwrap() as u32),
        "the runtime's resize and the recording's agree"
    );
    let claude_chain =
        source.clone().then(Transform::Resize { width: sent.0, height: sent.1 }).unwrap();
    total += check(
        "claude",
        &truth,
        CoordFrame::SentPixels,
        claude_chain,
        &answers(&fixture["claude"]["answers"], "box"),
    );

    // sam-3.1: source pixels of the image as sent.
    total += check(
        "sam-3.1",
        &truth,
        CoordFrame::SentPixels,
        source.clone(),
        &answers(&fixture["sam"], "box"),
    );

    // RF-DETR: 640x640 model input, the screenshot letterboxed in the centre.
    total += check(
        "rf-detr",
        &truth,
        CoordFrame::ModelInput { width: 640, height: 640, letterbox: Letterbox::Center },
        source,
        &answers(&fixture["rfdetr"]["answers"], "box"),
    );

    assert_eq!(total, 48);
}

#[test]
fn a_dpr_two_screenshot_halves_to_points() {
    let screen = ScreenSpace::new("built-in", [0.0, 0.0], 2.0);
    assert_eq!(screen.to_points([3024.0, 1964.0]), [1512.0, 982.0]);
    assert_eq!(screen.to_pixels([1512.0, 982.0]), [3024.0, 1964.0]);
    // A second display to the right starts at its own origin in the global point space.
    let right = ScreenSpace::new("external", [1512.0, 0.0], 1.0);
    assert_eq!(right.to_points([100.0, 50.0]), [1612.0, 50.0]);
}

#[test]
fn quantisation_alone_would_miss_without_the_frame() {
    // Reading Gemini's box_2d as pixels, or skipping the Claude resize, misses by far more
    // than 2 px: the frame is what makes the answers agree.
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let truth = truth(&fixture);
    let gemini = answers(&fixture["gemini"], "box_2d");
    let (name, raw) = &gemini[7];
    let (_, target) = truth.targets.iter().find(|(n, _)| n == name).unwrap();
    let as_pixels = BBox::new(raw[1], raw[0], raw[3], raw[2]);
    assert!((as_pixels.x1 - target.x1).abs() > 100.0);
}

#[test]
fn the_documented_crop_example_maps_back_to_the_screenshot() {
    // docs/official_docs/vision/index.md, "Coordinate Frames".
    let chain = TransformChain::new(3024, 1964)
        .then(Transform::Crop { x: 1900, y: 1160, width: 220, height: 120 })
        .unwrap();
    let gemini = CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx };
    let normalizer =
        RegionNormalizer::new(Digest::of(b"screenshot"), chain, gemini, BackendId::new("gemini"));
    let raw = RawRegion::boxed(gemini.box_from_raw([167.0, 91.0, 833.0, 909.0]), "save", 0.9);
    let b = normalizer.region(raw).unwrap().unwrap().shape.bbox().unwrap();
    assert!((b.x0 - 1920.0).abs() < 1.0 && (b.y0 - 1180.0).abs() < 1.0, "{b:?}");
    assert!((b.x1 - 2100.0).abs() < 1.0 && (b.y1 - 1260.0).abs() < 1.0, "{b:?}");
    let screen = ScreenSpace::new("built-in", [0.0, 0.0], 2.0);
    assert_eq!(screen.to_points([2010.0, 1220.0]), [1005.0, 610.0]);
}
