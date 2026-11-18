//! Property tests (spec 3.7 §7.3, §11): transform chains round-trip within 0.5 px, regions
//! never lie outside their image after normalisation, RLE masks round-trip exactly, and no
//! image leaves the egress gate unredacted under random policies.

use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_vision::{
    AxisOrder, BBox, BackendDescriptor, BackendId, CoordFrame, DataClass, DecodedImage, Digest,
    EgressAction, EgressConfig, EgressGate, InMemoryEgressRecorder, Letterbox, Locality, Mask,
    RawRegion, RedactionClass, Redactor, RegionNormalizer, ReleaseRequest, SentImage, Shape,
    SourceImage, Transform, TransformChain, VisionCx, VisionError,
};
use proptest::prelude::*;

/// A random chain over a random source, every step valid for the size it applies to.
fn chain() -> impl Strategy<Value = TransformChain> {
    let source = (16u32..4000, 16u32..4000);
    let steps = prop::collection::vec(
        (0u8..5, any::<u32>(), any::<u32>(), any::<u32>(), any::<u32>()),
        0..7,
    );
    (source, steps).prop_map(|((w, h), steps)| {
        let mut chain = TransformChain::new(w, h);
        for (kind, a, b, c, d) in steps {
            let (cw, ch) = chain.output_size();
            let step = match kind {
                0 if cw > 2 && ch > 2 => {
                    let width = 1 + a % (cw - 1);
                    let height = 1 + b % (ch - 1);
                    let x = c % (cw - width + 1);
                    let y = d % (ch - height + 1);
                    Transform::Crop { x, y, width, height }
                }
                1 => Transform::Resize { width: 8 + a % 4000, height: 8 + b % 4000 },
                2 => Transform::Pad { left: a % 64, top: b % 64, right: c % 64, bottom: d % 64 },
                3 => Transform::Rotate { quarter_turns: (a % 4) as u8 },
                _ => Transform::Redact,
            };
            chain.push(step).expect("generated steps are valid");
        }
        chain
    })
}

fn frame() -> impl Strategy<Value = CoordFrame> {
    prop_oneof![
        Just(CoordFrame::SentPixels),
        Just(CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx }),
        (
            prop_oneof![
                Just(Letterbox::Stretch),
                Just(Letterbox::Center),
                Just(Letterbox::TopLeft)
            ],
            32u32..1024,
            32u32..1024
        )
            .prop_map(|(letterbox, width, height)| CoordFrame::ModelInput {
                width,
                height,
                letterbox
            }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn source_points_round_trip_within_half_a_pixel(chain in chain(), fx in 0.0f64..1.0, fy in 0.0f64..1.0) {
        let p = [fx * f64::from(chain.source_width), fy * f64::from(chain.source_height)];
        let back = chain.to_source(chain.to_target(p));
        prop_assert!((back[0] - p[0]).abs() <= 0.5 && (back[1] - p[1]).abs() <= 0.5, "{p:?} -> {back:?}");
    }

    #[test]
    fn backend_points_round_trip_within_half_a_pixel(chain in chain(), fx in 0.0f64..1.0, fy in 0.0f64..1.0) {
        let (w, h) = chain.output_size();
        let q = [fx * f64::from(w), fy * f64::from(h)];
        let back = chain.to_target(chain.to_source(q));
        prop_assert!((back[0] - q[0]).abs() <= 0.5 && (back[1] - q[1]).abs() <= 0.5, "{q:?} -> {back:?}");
    }

    #[test]
    fn frames_round_trip_within_half_a_pixel(frame in frame(), w in 16u32..4000, h in 16u32..4000, fx in 0.0f64..1.0, fy in 0.0f64..1.0) {
        let p = [fx * f64::from(w), fy * f64::from(h)];
        let back = frame.point_to_sent(frame.point_from_sent(p, (w, h)), (w, h));
        prop_assert!((back[0] - p[0]).abs() <= 0.5 && (back[1] - p[1]).abs() <= 0.5);
    }

    #[test]
    fn normalised_regions_never_lie_outside_their_image(
        chain in chain(),
        frame in frame(),
        corners in prop::array::uniform4(-0.5f32..1.5),
        point in prop::array::uniform2(-0.5f32..1.5),
    ) {
        let sent = chain.output_size();
        let (sw, sh) = (chain.source_width, chain.source_height);
        let normalizer = RegionNormalizer::new(Digest::of(b"src"), chain, frame, BackendId::new("p"));
        // Raw coordinates up to half an image beyond each edge, in the backend's frame.
        let span = match frame {
            CoordFrame::Normalized { scale, .. } => (scale as f32, scale as f32),
            CoordFrame::ModelInput { width, height, .. } => (width as f32, height as f32),
            _ => (sent.0 as f32, sent.1 as f32),
        };
        let bbox = BBox::new(corners[0] * span.0, corners[1] * span.1, corners[2] * span.0, corners[3] * span.1);
        let shapes = [
            Shape::Box(bbox),
            Shape::Point(adk_vision::Point2 { x: point[0] * span.0, y: point[1] * span.1 }),
            Shape::Polygon(adk_vision::Polygon { points: vec![[bbox.x0, bbox.y0], [bbox.x1, bbox.y0], [bbox.x1, bbox.y1]] }),
        ];
        for shape in shapes {
            if let Some(region) = normalizer.region(RawRegion::shape(shape)).unwrap() {
                prop_assert!(region.shape.is_within(sw, sh), "{:?} outside {sw}x{sh}", region.shape);
            }
        }
    }

    #[test]
    fn rle_masks_round_trip_exactly(w in 1u32..48, h in 1u32..48, seed in any::<u64>(), density in 0u64..100) {
        let mut state = seed | 1;
        let bits: Vec<bool> = (0..w * h)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state % 100 < density
            })
            .collect();
        let mask = Mask::encode(w, h, &bits);
        prop_assert_eq!(mask.decode(), bits.clone());
        prop_assert_eq!(mask.counts.iter().map(|&c| u64::from(c)).sum::<u64>(), u64::from(w * h));
        prop_assert_eq!(mask.area(), bits.iter().filter(|b| **b).count() as u64);
        let json = serde_json::to_string(&mask).unwrap();
        prop_assert_eq!(serde_json::from_str::<Mask>(&json).unwrap(), mask.clone());
        let expected = BBox::covering(
            bits.iter().enumerate().filter(|(_, b)| **b).flat_map(|(i, _)| {
                let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
                [[x, y], [x + 1.0, y + 1.0]]
            }),
        );
        prop_assert_eq!(mask.bbox(), expected);
    }

    #[test]
    fn masks_survive_a_chain_to_the_source(scale in 1u32..4, x in 0u32..20, y in 0u32..20, bw in 1u32..12, bh in 1u32..12) {
        // A box mask in a downscaled image maps back to the box at source resolution.
        let (sw, sh) = (32 * scale, 32 * scale);
        let chain = TransformChain::new(sw, sh).then(Transform::Resize { width: 32, height: 32 }).unwrap();
        let target = BBox::new(x as f32, y as f32, (x + bw).min(32) as f32, (y + bh).min(32) as f32);
        let mask = Mask::from_box(32, 32, &target);
        let Shape::Mask(source) = chain.shape_to_source(&Shape::Mask(mask)).unwrap() else { unreachable!() };
        let expected = chain.bbox_to_source(&target);
        prop_assert_eq!(source.bbox(), Some(expected));
    }
}

struct BoxRedactor(RedactionClass, BBox);

#[async_trait::async_trait]
impl Redactor for BoxRedactor {
    fn class(&self) -> RedactionClass {
        self.0.clone()
    }

    async fn find(&self, _cx: &VisionCx, _image: &SentImage) -> adk_vision::Result<Vec<BBox>> {
        Ok(vec![self.1])
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever the rules, the required classes and the installed redactors: an image either
    /// leaves with every required class filled, or does not leave.
    #[test]
    fn no_release_without_redaction(
        default in prop_oneof![Just(EgressAction::Allow), Just(EgressAction::Ask), Just(EgressAction::Deny)],
        require_faces in any::<bool>(),
        require_text in any::<bool>(),
        install_faces in any::<bool>(),
        install_text in any::<bool>(),
        crop in any::<bool>(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let faces = BBox::new(10.0, 10.0, 30.0, 30.0);
        let text = BBox::new(40.0, 5.0, 60.0, 20.0);
        let mut config = EgressConfig::default().with_default(default);
        if require_faces { config = config.redacting(RedactionClass::faces()); }
        if require_text { config = config.redacting(RedactionClass::personal_text()); }
        let recorder = Arc::new(InMemoryEgressRecorder::new());
        let mut gate = EgressGate::new(EgressPolicy::public_only(), config, recorder.clone());
        if install_faces { gate = gate.with_redactor(Arc::new(BoxRedactor(RedactionClass::faces(), faces))); }
        if install_text { gate = gate.with_redactor(Arc::new(BoxRedactor(RedactionClass::personal_text(), text))); }
        let source = SourceImage::from_pixels(DecodedImage::filled(64, 64, [200, 200, 200, 255])).unwrap();
        let backend = BackendDescriptor::new("cloud", Locality::Remote { hosts: vec!["vision.example".into()] }, CoordFrame::SentPixels);
        let mut request = ReleaseRequest::new(&source, &backend, DataClass::new("photo"));
        if crop { request = request.with_crop(BBox::new(0.0, 0.0, 64.0, 32.0)); }
        let result = rt.block_on(gate.release(&VisionCx::new(), request));
        match result {
            Ok(released) => {
                prop_assert_eq!(default, EgressAction::Allow);
                let pixels = released.pixels();
                if require_faces {
                    prop_assert!(install_faces);
                    prop_assert_eq!(pixels.pixel(20, 20), [0, 0, 0, 255]);
                    prop_assert_eq!(released.receipt().redactions.get(&RedactionClass::faces()), Some(&1));
                }
                if require_text {
                    prop_assert!(install_text);
                    prop_assert_eq!(pixels.pixel(50, 10), [0, 0, 0, 255]);
                }
                prop_assert_eq!(Digest::of(released.bytes()), released.receipt().egress);
                prop_assert_eq!(recorder.released().len(), 1);
            }
            Err(VisionError::RedactionUnavailable { .. }) => {
                prop_assert!((require_faces && !install_faces) || (require_text && !install_text));
                prop_assert!(recorder.released().is_empty());
            }
            Err(VisionError::EgressHeld { .. } | VisionError::EgressRefused { .. }) => {
                prop_assert_ne!(default, EgressAction::Allow);
                prop_assert!(recorder.released().is_empty());
            }
            Err(other) => prop_assert!(false, "unexpected {other}"),
        }
    }
}
