//! The conformance cases. Each panics with a message naming what the backend got wrong.

use std::time::{Duration, Instant};

use adk_core::{AdkError, ErrorCategory, GOVERNANCE_FROZEN_CODE, UsageUnit};

use super::EmbedderTarget;
use super::probe::{AnyProbe, Fixture};
use crate::backend::Oversize;
use crate::egress::EgressRecord;
use crate::error::VisionError;
use crate::geometry::{RegionSet, Transform};
use crate::media::{DecodeLimits, DecodedImage, SourceImage};
use crate::runtime::CallOptions;

/// Regions are in source pixels within the fixture's tolerance, belong to the fixture image
/// and lie inside it, and carry the backend, its frame and the transform chain.
pub async fn coordinates(probe: &AnyProbe, fixture: &Fixture) {
    let descriptor = probe.descriptor();
    let harness = probe.harness(true);
    let cx = harness.cx();
    let output = match probe.call(&harness, &cx, fixture, &fixture.image).await {
        Ok(output) => output,
        Err(VisionError::Unsupported(_)) if !probe.returns_regions() => return,
        Err(err) => panic!("`{}` failed on the fixture: {err}", descriptor.id),
    };
    if !probe.returns_regions() {
        return;
    }
    let (w, h) = fixture.image.size();
    let set = RegionSet::new(fixture.image.digest(), w, h, output.regions.clone());
    set.validate().unwrap_or_else(|err| {
        panic!("`{}` returned a region outside the image: {err}", descriptor.id)
    });
    let expected_resize = descriptor.limits.fit((w, h)).is_some();
    for region in &output.regions {
        assert_eq!(region.origin.backend, descriptor.id, "a region names another backend");
        assert_eq!(
            region.origin.frame, descriptor.frame,
            "a region's frame is not the descriptor's"
        );
        assert_eq!(
            (region.origin.chain.source_width, region.origin.chain.source_height),
            (w, h),
            "a region's transform chain does not start at the source image"
        );
        let resized =
            region.origin.chain.steps.iter().any(|s| matches!(s, Transform::Resize { .. }));
        assert_eq!(
            resized, expected_resize,
            "`{}`: the chain records a resize exactly when the backend's limit needs one",
            descriptor.id
        );
    }
    for object in &fixture.objects {
        let found = output.regions.iter().any(|region| {
            let Some(b) = region.shape.bbox() else { return false };
            let t = fixture.tolerance_px;
            (b.x0 - object.bbox.x0).abs() <= t
                && (b.y0 - object.bbox.y0).abs() <= t
                && (b.x1 - object.bbox.x1).abs() <= t
                && (b.y1 - object.bbox.y1).abs() <= t
                && (object.label.is_none()
                    || region.label.is_none()
                    || region.label == object.label)
        });
        assert!(
            found,
            "`{}` found no region within {} px of {:?} in source pixels; it returned {:?}",
            descriptor.id,
            fixture.tolerance_px,
            object,
            output.regions.iter().map(|r| (&r.label, r.shape.bbox())).collect::<Vec<_>>()
        );
    }
}

/// Every call records its images as units, at zero cost for a local backend and at the
/// unit price for a per-unit one, in the budget and in its call record.
pub async fn usage(probe: &AnyProbe, fixture: &Fixture) {
    let descriptor = probe.descriptor();
    let harness = probe.harness(true);
    let cx = harness.cx();
    let output = probe
        .call(&harness, &cx, fixture, &fixture.image)
        .await
        .unwrap_or_else(|err| panic!("`{}` failed on the fixture: {err}", descriptor.id));
    assert!(!output.calls.is_empty(), "`{}`: no call was recorded", descriptor.id);
    let images: u64 = output.calls.iter().map(|c| c.units.get(UsageUnit::Images)).sum();
    assert!(images >= 1, "`{}`: the call recorded no image unit", descriptor.id);
    assert_eq!(
        harness.budget.units().get(UsageUnit::Images),
        images,
        "`{}`: the run budget did not count the images its calls recorded",
        descriptor.id
    );
    let recorded: u64 = output.calls.iter().filter_map(|c| c.cost_micro_usd).sum();
    match &descriptor.price {
        None => {
            assert_eq!(recorded, 0, "`{}`: a local backend recorded a cost", descriptor.id);
            assert_eq!(harness.budget.usage().cost_micro_usd, 0, "a local backend spent budget");
        }
        Some(price) => {
            if let Some(unit_cost) = price.request_cost(1) {
                assert!(
                    recorded >= unit_cost,
                    "`{}` recorded less than its unit price",
                    descriptor.id
                );
            }
        }
    }
}

/// A call whose context is already cancelled ends with `Cancelled`.
pub async fn cancellation(probe: &AnyProbe, fixture: &Fixture) {
    let harness = probe.harness(true);
    let cx = harness.cx();
    cx.cancellation().cancel();
    let err = probe.call(&harness, &cx, fixture, &fixture.image).await.err();
    assert!(
        matches!(err, Some(VisionError::Cancelled)),
        "`{}`: a cancelled call returned {err:?}",
        probe.descriptor().id
    );
}

/// A slow backend's call ends with `Cancelled` soon after cancellation, within one call.
pub async fn cancellation_within_one_call(probe: &AnyProbe, fixture: &Fixture) {
    let harness = probe.harness(true);
    let cx = harness.cx();
    let token = cx.cancellation().clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });
    let started = Instant::now();
    let err = probe.call(&harness, &cx, fixture, &fixture.image).await.err();
    assert!(matches!(err, Some(VisionError::Cancelled)), "a cancelled slow call returned {err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "cancellation took {:?}",
        started.elapsed()
    );
}

/// A frozen kill switch refuses the call before the backend runs, and nothing leaves.
pub async fn kill_switch(probe: &AnyProbe, fixture: &Fixture) {
    let harness = probe.harness(true);
    let cx = harness.cx();
    harness.governance.freeze("conformance: kill switch");
    let err = probe
        .call(&harness, &cx, fixture, &fixture.image)
        .await
        .err()
        .unwrap_or_else(|| panic!("`{}` ran while frozen", probe.descriptor().id));
    let adk: AdkError = err.into();
    assert_eq!(adk.code, GOVERNANCE_FROZEN_CODE, "a frozen call failed with {adk}");
    assert!(harness.recorder.released().is_empty(), "an image was released while frozen");
}

/// A local backend leaves the egress recorder empty; a remote backend's every image is a
/// recorded release, matching its call record.
pub async fn egress(probe: &AnyProbe, fixture: &Fixture) {
    let descriptor = probe.descriptor();
    let harness = probe.harness(true);
    let cx = harness.cx();
    let output = probe
        .call(&harness, &cx, fixture, &fixture.image)
        .await
        .unwrap_or_else(|err| panic!("`{}` failed on the fixture: {err}", descriptor.id));
    let released = harness.recorder.released();
    if probe.is_remote() {
        let egress: Vec<_> = output.calls.iter().flat_map(|c| c.egress.iter().copied()).collect();
        assert!(!egress.is_empty(), "`{}`: a remote call released nothing", descriptor.id);
        assert_eq!(
            released.iter().map(|r| r.egress).collect::<Vec<_>>(),
            egress,
            "`{}`: every released image is in a call record, and only those",
            descriptor.id
        );
        for receipt in &released {
            assert_eq!(receipt.source, fixture.image.digest(), "a release names another source");
        }
    } else {
        assert!(
            harness.recorder.records().is_empty(),
            "`{}` is local but the egress recorder holds {:?}",
            descriptor.id,
            harness.recorder.records()
        );
        assert!(output.calls.iter().all(|c| c.egress.is_empty()), "a local call recorded egress");
    }
}

/// A remote backend without an egress gate is refused before it runs, as `Forbidden`.
pub async fn unreleased(probe: &AnyProbe, fixture: &Fixture) {
    if !probe.is_remote() {
        return;
    }
    let harness = probe.harness(false);
    let cx = harness.cx();
    let err =
        probe.call(&harness, &cx, fixture, &fixture.image).await.err().unwrap_or_else(|| {
            panic!("remote `{}` ran without an egress gate", probe.descriptor().id)
        });
    assert!(matches!(err, VisionError::NotReleased { .. }), "expected NotReleased, got {err}");
    assert_eq!(AdkError::from(err).category, ErrorCategory::Forbidden);
}

/// An image over the backend's limit is refused with the limit in the message, or
/// downscaled with the resize recorded, as the descriptor says; an image over the decode
/// cap is refused before decoding.
pub async fn oversized(probe: &AnyProbe, fixture: &Fixture) {
    let descriptor = probe.descriptor();
    let limits = descriptor.limits;
    let size = match (limits.max_long_edge, limits.max_pixels) {
        (Some(edge), _) if edge < 16_384 => Some((edge + 1, 2)),
        (_, Some(px)) if px < 16_000_000 => Some(((px / 2 + 1) as u32, 2)),
        _ => None,
    };
    if let Some((w, h)) = size {
        let image = SourceImage::from_pixels(DecodedImage::filled(w, h, [255, 255, 255, 255]))
            .expect("in-memory PNG encoding");
        let harness = probe.harness(true);
        let cx = harness.cx();
        let result = probe.call(&harness, &cx, fixture, &image).await;
        match limits.oversize {
            Oversize::Refuse => {
                let err = result
                    .err()
                    .unwrap_or_else(|| panic!("`{}` took an oversized image", descriptor.id));
                assert!(
                    matches!(err, VisionError::InputTooLarge { .. })
                        && err.to_string().contains(&limits.describe()),
                    "an oversized image failed without naming the limit: {err}"
                );
            }
            Oversize::Downscale => {
                result.unwrap_or_else(|err| {
                    panic!("`{}` refused a downscaled image: {err}", descriptor.id)
                });
                if probe.is_remote() {
                    let resized = harness.recorder.released().iter().all(|r| {
                        r.chain.steps.iter().any(|s| matches!(s, Transform::Resize { .. }))
                    });
                    assert!(resized, "the release of an oversized image records no resize");
                }
            }
        }
    }
    let png = DecodedImage::filled(64, 64, [0, 0, 0, 255]).encode_png().expect("PNG");
    let err =
        SourceImage::decode(&png, &DecodeLimits::default().with_max_pixels(1_000)).unwrap_err();
    assert!(
        err.to_string().contains("1000"),
        "the decode cap refusal does not name the limit: {err}"
    );
}

/// A rate-limited backend's error maps to `RateLimited` with a retry hint.
pub async fn rate_limited(probe: &AnyProbe, fixture: &Fixture) {
    let harness = probe.harness(true);
    let cx = harness.cx();
    let err = probe
        .call(&harness, &cx, fixture, &fixture.image)
        .await
        .err()
        .unwrap_or_else(|| panic!("the rate-limited `{}` succeeded", probe.descriptor().id));
    let adk: AdkError = err.into();
    assert_eq!(adk.category, ErrorCategory::RateLimited, "a rate limit mapped to {adk}");
    assert!(adk.retry.should_retry, "a rate limit carries no retry hint");
}

/// Image and text vectors have the space's dimensions.
pub async fn embedder_dimensions(target: EmbedderTarget, fixture: &Fixture) {
    let space = match &target {
        super::Target::Local(b) => b.space().clone(),
        super::Target::Remote(b) => b.space().clone(),
    };
    let probe = AnyProbe::from(target);
    let harness = probe.harness(true);
    let cx = harness.cx();
    let options = CallOptions::default();
    let images = harness
        .runtime
        .embed_images(&cx, &[&fixture.image, &fixture.image], &options)
        .await
        .unwrap_or_else(|err| panic!("embedding images failed: {err}"));
    assert_eq!(images.value.len(), 2);
    assert!(images.value.iter().all(|v| v.len() == space.dimensions));
    let texts = harness
        .runtime
        .embed_texts(&cx, &["a red box", "a blue box"], &options)
        .await
        .unwrap_or_else(|err| panic!("embedding texts failed: {err}"));
    assert_eq!(texts.value.len(), 2);
    assert!(texts.value.iter().all(|v| v.len() == space.dimensions));
}

/// An edit is stored as a new image derived from the source, and the same edit of the same
/// image names the same operation.
pub async fn editor_provenance(probe: &AnyProbe, fixture: &Fixture) {
    let harness = probe.harness(true);
    let cx = harness.cx();
    let options = CallOptions::default();
    let first = harness
        .runtime
        .edit(&cx, &fixture.image, &fixture.edit, &options)
        .await
        .unwrap_or_else(|err| panic!("the edit failed: {err}"));
    let second = harness
        .runtime
        .edit(&cx, &fixture.image, &fixture.edit, &options)
        .await
        .unwrap_or_else(|err| panic!("the edit failed: {err}"));
    let (
        crate::media::Provenance::Derived { from, operation },
        crate::media::Provenance::Derived { operation: again, .. },
    ) = (&first.value.image.provenance, &second.value.image.provenance)
    else {
        panic!("an edit is not stored with Provenance::Derived");
    };
    assert_eq!(*from, fixture.image.digest(), "the edit names another source");
    assert_eq!(operation, again, "the same edit named two operations");
    assert!(
        !matches!(harness.recorder.records().first(), Some(EgressRecord::Refused(_))),
        "the edit was refused at the egress gate"
    );
}

/// Tracker cases.
pub mod tracker {
    use chrono::Utc;

    use crate::Digest;
    use crate::backend::BackendId;
    use crate::geometry::{BBox, CoordFrame, Region, RegionOrigin, Shape, TransformChain};
    use crate::media::{FrameMeta, FrameSourceId};
    use crate::traits::Tracker;

    fn frame(index: u64) -> FrameMeta {
        FrameMeta {
            source: FrameSourceId("conformance".into()),
            index,
            pts_ms: index * 200,
            captured_at: Utc::now(),
        }
    }

    fn region(bbox: BBox, label: &str) -> Region {
        Region {
            image: Digest::of(b"frame"),
            shape: Shape::Box(bbox),
            label: Some(label.into()),
            score: Some(0.9),
            track: None,
            origin: RegionOrigin {
                backend: BackendId::new("conformance"),
                manifest: None,
                frame: CoordFrame::SentPixels,
                chain: TransformChain::new(640, 480),
                escalated_from: None,
            },
        }
    }

    /// An object moving a few pixels per frame keeps its identifier.
    pub fn stable_ids<T: Tracker>(mut tracker: T) {
        let mut ids = Vec::new();
        for i in 0..10u64 {
            let x = 100.0 + i as f32 * 4.0;
            let out = tracker
                .update(&frame(i), vec![region(BBox::new(x, 100.0, x + 80.0, 160.0), "part")]);
            ids.push(out[0].track.expect("every tracked region carries a track"));
        }
        assert!(
            ids.windows(2).all(|w| w[0] == w[1]),
            "a moving object changed identifiers: {ids:?}"
        );
    }

    /// Two objects get two identifiers, and a new object a new one.
    pub fn new_objects_get_new_ids<T: Tracker>(mut tracker: T) {
        let a = region(BBox::new(10.0, 10.0, 60.0, 60.0), "part");
        let b = region(BBox::new(300.0, 300.0, 360.0, 360.0), "part");
        let first = tracker.update(&frame(0), vec![a.clone(), b.clone()]);
        assert_ne!(first[0].track, first[1].track, "two objects share an identifier");
        let c = region(BBox::new(500.0, 50.0, 560.0, 110.0), "part");
        let second = tracker.update(&frame(1), vec![a, b, c]);
        let ids: std::collections::BTreeSet<_> = second.iter().map(|r| r.track).collect();
        assert_eq!(ids.len(), 3, "a new object did not get a new identifier");
    }

    /// After a reset a tracker answers as a new one would: tracks are anonymous and end
    /// with their source session.
    pub fn reset_restarts_ids<T: Tracker>(make: impl Fn() -> T) {
        let mut used = make();
        for i in 0..3u64 {
            let x = 10.0 + i as f32 * 100.0;
            used.update(&frame(i), vec![region(BBox::new(x, 10.0, x + 50.0, 60.0), "part")]);
        }
        used.reset();
        let probe = vec![region(BBox::new(10.0, 10.0, 60.0, 60.0), "part")];
        let after = used.update(&frame(3), probe.clone());
        let fresh = make().update(&frame(3), probe);
        assert_eq!(
            after.iter().map(|r| r.track).collect::<Vec<_>>(),
            fresh.iter().map(|r| r.track).collect::<Vec<_>>(),
            "a reset tracker did not answer as a new one"
        );
    }
}
