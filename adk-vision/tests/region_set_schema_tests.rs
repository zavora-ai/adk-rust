//! The published `RegionSet` JSON Schema (`schema/region-set.json`) accepts every region
//! the crate serializes and refuses malformed ones, so surfaces can draw region sets
//! without depending on this crate (spec 3.7 §7.3).

use adk_vision::{
    AxisOrder, BBox, BackendId, CoordFrame, Digest, EscalatedFrom, EscalationReason, Keypoint,
    Keypoints, Letterbox, Mask, Point2, Polygon, Region, RegionOrigin, RegionSet, Shape, TrackId,
    Transform, TransformChain,
};
use serde_json::{Value, json};

const SCHEMA: &str = include_str!("../schema/region-set.json");

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&serde_json::from_str(SCHEMA).unwrap()).unwrap()
}

fn sample() -> RegionSet {
    let image = Digest::of(b"page");
    let chain = TransformChain::new(640, 480)
        .then(Transform::Crop { x: 10, y: 20, width: 600, height: 400 })
        .unwrap()
        .then(Transform::Resize { width: 300, height: 200 })
        .unwrap()
        .then(Transform::Pad { left: 0, top: 50, right: 0, bottom: 50 })
        .unwrap()
        .then(Transform::Rotate { quarter_turns: 1 })
        .unwrap()
        .then(Transform::Redact)
        .unwrap();
    let origin = |frame| RegionOrigin {
        backend: BackendId::new("backend"),
        manifest: Some("rf-detr-m".into()),
        frame,
        chain: chain.clone(),
        escalated_from: None,
    };
    let region = |shape, frame| Region {
        image,
        shape,
        label: Some("seal".into()),
        score: Some(0.42),
        track: Some(TrackId(7)),
        origin: origin(frame),
    };
    let mut escalated = region(Shape::Box(BBox::new(1.0, 2.0, 3.0, 4.0)), CoordFrame::SentPixels);
    escalated.origin.escalated_from = Some(EscalatedFrom {
        backend: BackendId::new("local"),
        reason: EscalationReason::LowConfidence { threshold: 0.5 },
        label: Some("seal".into()),
        score: Some(0.2),
        text: Some("old".into()),
    });
    let mut disputed = region(Shape::Point(Point2 { x: 5.0, y: 6.0 }), CoordFrame::SentPixels);
    disputed.origin.escalated_from = Some(EscalatedFrom {
        backend: BackendId::new("local"),
        reason: EscalationReason::Disagreement { with: BackendId::new("second") },
        label: None,
        score: None,
        text: None,
    });
    RegionSet::new(
        image,
        640,
        480,
        vec![
            escalated,
            disputed,
            region(
                Shape::Polygon(Polygon { points: vec![[1.0, 1.0], [5.0, 1.0], [5.0, 5.0]] }),
                CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx },
            ),
            region(
                Shape::Mask(Mask::from_box(640, 480, &BBox::new(10.0, 10.0, 20.0, 20.0))),
                CoordFrame::ModelInput { width: 640, height: 640, letterbox: Letterbox::Center },
            ),
            region(
                Shape::Keypoints(Keypoints {
                    skeleton: "coco-17".into(),
                    points: vec![Keypoint { x: 1.0, y: 2.0, visible: true, score: Some(0.9) }],
                }),
                CoordFrame::ModelInput { width: 384, height: 384, letterbox: Letterbox::TopLeft },
            ),
        ],
    )
}

#[test]
fn every_serialized_region_matches_the_published_schema() {
    let set = sample();
    let value = serde_json::to_value(&set).unwrap();
    let validator = validator();
    let errors: Vec<String> = validator.iter_errors(&value).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:#?}\n{value:#}");
    assert_eq!(serde_json::from_value::<RegionSet>(value).unwrap(), set);
}

#[test]
fn the_wire_form_is_camel_case_with_kind_and_op_tags() {
    let value = serde_json::to_value(sample()).unwrap();
    let first = &value["regions"][0];
    assert_eq!(first["shape"], json!({"kind": "box", "x0": 1.0, "y0": 2.0, "x1": 3.0, "y1": 4.0}));
    assert_eq!(first["origin"]["chain"]["sourceWidth"], 640);
    assert_eq!(first["origin"]["chain"]["steps"][3], json!({"op": "rotate", "quarterTurns": 1}));
    assert_eq!(first["origin"]["escalatedFrom"]["reason"]["kind"], "lowConfidence");
    assert_eq!(
        value["regions"][2]["origin"]["frame"],
        json!({"kind": "normalized", "scale": 1000, "order": "yx"})
    );
}

#[test]
fn malformed_region_sets_are_refused() {
    let validator = validator();
    let good: Value = serde_json::to_value(sample()).unwrap();
    let mut no_origin = good.clone();
    no_origin["regions"][0].as_object_mut().unwrap().remove("origin");
    let mut bad_digest = good.clone();
    bad_digest["image"] = json!("md5:abc");
    let mut unknown_shape = good.clone();
    unknown_shape["regions"][0]["shape"] = json!({"kind": "circle", "r": 3});
    let mut unknown_frame = good;
    unknown_frame["regions"][0]["origin"]["frame"] = json!({"kind": "screenPoints"});
    for bad in [no_origin, bad_digest, unknown_shape, unknown_frame] {
        assert!(!validator.is_valid(&bad), "{bad:#}");
    }
}
