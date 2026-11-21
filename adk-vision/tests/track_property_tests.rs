//! `ByteTracker` (feature `track`): the conformance suite, and property tests for identifier
//! stability under detector jitter and re-association after an occlusion within the
//! tracker's window.

use adk_vision::{
    BBox, ByteTrackConfig, ByteTracker, CoordFrame, Digest, FrameMeta, FrameSourceId, Region,
    RegionOrigin, Shape, TrackId, Tracker, TransformChain,
};
use proptest::prelude::*;

mod conformance {
    adk_vision::tracker_conformance_tests! {
        make = adk_vision::ByteTracker::default(),
    }
}

fn frame(index: u64) -> FrameMeta {
    FrameMeta {
        source: FrameSourceId("property".into()),
        index,
        pts_ms: index * 200,
        captured_at: chrono::Utc::now(),
    }
}

fn region(bbox: BBox, label: &str, score: f32) -> Region {
    Region {
        image: Digest::of(b"frame"),
        shape: Shape::Box(bbox),
        label: Some(label.into()),
        score: Some(score),
        track: None,
        origin: RegionOrigin {
            backend: adk_vision::BackendId::new("property"),
            manifest: None,
            frame: CoordFrame::SentPixels,
            chain: TransformChain::new(1920, 1080),
            escalated_from: None,
        },
    }
}

/// One object: where it starts, how it moves per frame, and its size.
#[derive(Clone, Debug)]
struct Mover {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    w: f32,
    h: f32,
}

impl Mover {
    fn at(&self, t: u64) -> BBox {
        let (x, y) = (self.x + self.vx * t as f32, self.y + self.vy * t as f32);
        BBox::new(x, y, x + self.w, y + self.h)
    }
}

/// Up to four objects in separate 400-pixel cells, each moving at most 3 px a frame, so
/// their paths never meet over 60 frames.
fn movers() -> impl Strategy<Value = Vec<Mover>> {
    prop::collection::vec(
        (0.0f32..40.0, 0.0f32..40.0, -3.0f32..3.0, -3.0f32..3.0, 60.0f32..160.0, 60.0f32..160.0),
        1..=4,
    )
    .prop_map(|cells| {
        cells
            .into_iter()
            .enumerate()
            .map(|(i, (dx, dy, vx, vy, w, h))| Mover {
                x: 200.0 + 400.0 * i as f32 + dx,
                y: 400.0 + dy,
                vx,
                vy,
                w,
                h,
            })
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Objects whose detected boxes jitter by up to 4 px on every edge, with scores between
    /// 0.6 and 1.0 and in a shuffled order, keep one identifier each for 60 frames, and no
    /// two objects share one.
    #[test]
    fn identifiers_are_stable_under_jitter(
        movers in movers(),
        jitter in prop::collection::vec((-4.0f32..4.0, -4.0f32..4.0, -4.0f32..4.0, -4.0f32..4.0, 0.6f32..1.0), 240),
        order in prop::collection::vec(any::<prop::sample::Index>(), 60),
    ) {
        let mut tracker = ByteTracker::default();
        let mut ids: Vec<Option<TrackId>> = vec![None; movers.len()];
        for t in 0..60u64 {
            let mut detections: Vec<(usize, Region)> = movers
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let b = m.at(t);
                    let (a, c, d, e, score) = jitter[(t as usize * 4 + i) % jitter.len()];
                    (i, region(BBox::new(b.x0 + a, b.y0 + c, b.x1 + d, b.y1 + e), "part", score))
                })
                .collect();
            let n = detections.len();
            detections.swap(0, order[t as usize].index(n));
            let (owners, regions): (Vec<usize>, Vec<Region>) = detections.into_iter().unzip();
            let out = tracker.update(&frame(t), regions);
            prop_assert_eq!(out.len(), n);
            for (owner, region) in owners.iter().zip(&out) {
                let id = region.track;
                prop_assert!(id.is_some(), "object {} lost its track at frame {}", owner, t);
                match ids[*owner] {
                    None => ids[*owner] = id,
                    Some(first) => prop_assert_eq!(Some(first), id, "object {} changed identifier at frame {}", owner, t),
                }
            }
        }
        let mut distinct: Vec<TrackId> = ids.into_iter().flatten().collect();
        distinct.sort();
        distinct.dedup();
        prop_assert_eq!(distinct.len(), movers.len());
    }

    /// An object hidden for up to `max_missed` frames reappears, where its motion puts it,
    /// under the identifier it had; hidden for longer, it starts a new track.
    #[test]
    fn an_occluded_object_is_found_again_within_the_window(
        max_missed in 1u32..15,
        vx in -2.0f32..2.0,
        vy in -2.0f32..2.0,
        size in 70.0f32..140.0,
        before in 5u64..20,
        overshoot in 1u32..10,
    ) {
        let mover = Mover { x: 600.0, y: 400.0, vx, vy, w: size, h: size };
        let run = |gap: u32| -> (Option<TrackId>, Option<TrackId>) {
            let mut tracker = ByteTracker::new(ByteTrackConfig::default().with_max_missed(max_missed));
            let mut first = None;
            for t in 0..before {
                first = tracker.update(&frame(t), vec![region(mover.at(t), "part", 0.9)])[0].track;
            }
            for t in before..before + u64::from(gap) {
                assert_no_regions(tracker.update(&frame(t), Vec::new()).is_empty());
            }
            let t = before + u64::from(gap);
            let again = tracker.update(&frame(t), vec![region(mover.at(t), "part", 0.9)])[0].track;
            (first, again)
        };
        for gap in 1..=max_missed {
            let (first, again) = run(gap);
            prop_assert!(first.is_some());
            prop_assert_eq!(first, again, "a gap of {} frames within a window of {} lost the identifier", gap, max_missed);
        }
        let (first, again) = run(max_missed + overshoot);
        prop_assert!(again.is_some());
        prop_assert_ne!(first, again, "a track outlived its window of {} frames", max_missed);
    }
}

fn assert_no_regions(empty: bool) {
    assert!(empty, "an empty frame returned regions");
}

#[test]
fn identifiers_restart_after_a_reset_and_are_anonymous_counters() {
    let mut tracker = ByteTracker::default();
    let a = region(BBox::new(10.0, 10.0, 90.0, 90.0), "person", 0.9);
    let b = region(BBox::new(400.0, 10.0, 480.0, 90.0), "forklift", 0.9);
    let out = tracker.update(&frame(0), vec![a.clone(), b.clone()]);
    // Lanes step in label order: forklift before person.
    assert_eq!(out[0].track, Some(TrackId(1)));
    assert_eq!(out[1].track, Some(TrackId(0)));
    tracker.reset();
    let out = tracker.update(&frame(0), vec![a, b]);
    assert_eq!((out[0].track, out[1].track), (Some(TrackId(1)), Some(TrackId(0))));
}
