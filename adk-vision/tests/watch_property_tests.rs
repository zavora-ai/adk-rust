//! Watch conditions (spec 3.7 §7.10): zone containment, consecutive-frame counting, cooldown,
//! idempotent event keys and sessions, as properties over random streams, and the S10 rule
//! that one torn seal seen for forty frames fires once.

use std::collections::HashSet;
use std::time::Duration;

use adk_vision::{
    BBox, BackendId, CoordFrame, Digest, FrameRef, FrameSourceId, Region, RegionOrigin, Shape,
    TrackId, TrackedObservation, TransformChain, WatchCondition, WatchEvaluator, WatchEvent, Zone,
    ZoneAnchor,
};
use chrono::{DateTime, TimeZone, Utc};
use proptest::prelude::*;

const FRAME_MS: i64 = 100;

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap()
}

fn region(track: u64, label: &str, score: f32, bbox: BBox) -> Region {
    Region {
        image: Digest::of(b"frame"),
        shape: Shape::Box(bbox),
        label: Some(label.into()),
        score: Some(score),
        track: Some(TrackId(track)),
        origin: RegionOrigin {
            backend: BackendId::new("rf-detr"),
            manifest: None,
            frame: CoordFrame::SentPixels,
            chain: TransformChain::new(1280, 720),
            escalated_from: None,
        },
    }
}

fn seal(track: u64) -> Region {
    region(track, "torn seal", 0.9, BBox::new(400.0, 200.0, 440.0, 230.0))
}

fn observation(
    session: &str,
    index: u64,
    regions: Vec<Region>,
    held: &[u64],
) -> TrackedObservation {
    let at = start() + chrono::Duration::milliseconds(FRAME_MS * index as i64);
    let frame = FrameRef::new(index, at, Digest::of(&index.to_be_bytes()), 1280, 720);
    TrackedObservation::new(FrameSourceId("conveyor-1".into()), session, frame, regions)
        .with_tracks(held.iter().copied().map(TrackId))
}

fn seal_watch(min_frames: u32, cooldown_ms: u64) -> WatchEvaluator {
    WatchEvaluator::new(vec![
        WatchCondition::new("torn-seal", ["torn seal"])
            .with_min_score(0.5)
            .with_min_frames(min_frames)
            .with_cooldown(Duration::from_millis(cooldown_ms)),
    ])
    .unwrap()
}

/// The frame at which a run of `n` presences first completes.
fn first_streak(pattern: &[bool], n: usize) -> Option<usize> {
    let mut run = 0;
    for (index, present) in pattern.iter().enumerate() {
        run = if *present { run + 1 } else { 0 };
        if run == n {
            return Some(index);
        }
    }
    None
}

#[test]
fn s10_one_torn_seal_seen_for_forty_frames_fires_once() {
    let mut watch = seal_watch(3, 0);
    let events: Vec<WatchEvent> = (0..40)
        .flat_map(|i| watch.observe(&observation("shift-1", i, vec![seal(7)], &[7])))
        .collect();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!((event.frame.index, event.frames, event.track), (2, 3, TrackId(7)));
    assert_eq!(event.key(), "shift-1:7:torn-seal:detected");
    assert_eq!(event.frame.digest, Digest::of(&2u64.to_be_bytes()));
}

#[test]
fn a_new_session_starts_afresh_with_its_own_keys() {
    let mut watch = seal_watch(2, 0);
    let mut keys = Vec::new();
    for session in ["shift-1", "shift-2"] {
        for index in 0..5 {
            for event in watch.observe(&observation(session, index, vec![seal(1)], &[1])) {
                keys.push(event.key());
            }
        }
    }
    assert_eq!(keys, ["shift-1:1:torn-seal:detected", "shift-2:1:torn-seal:detected"]);
}

#[test]
fn untracked_regions_labels_scores_and_zones_decide_what_counts() {
    // The left lane's floor: the left half of the frame below y = 40.
    let lane = Zone::new(vec![[0.0, 40.0], [640.0, 40.0], [640.0, 720.0], [0.0, 720.0]])
        .unwrap()
        .with_anchor(ZoneAnchor::BottomCenter);
    let mut watch = WatchEvaluator::new(vec![
        WatchCondition::new("left-lane", ["Forklift"]).with_min_frames(1).with_zone(lane),
    ])
    .unwrap();
    let mut untracked = region(0, "forklift", 0.9, BBox::new(10.0, 10.0, 50.0, 50.0));
    untracked.track = None;
    let regions = vec![
        untracked,
        region(1, "forklift", 0.3, BBox::new(10.0, 10.0, 50.0, 50.0)),
        region(2, "person", 0.9, BBox::new(10.0, 10.0, 50.0, 50.0)),
        region(3, "forklift", 0.9, BBox::new(700.0, 10.0, 760.0, 50.0)),
        // Its centre is above the floor, its bottom centre on it.
        region(4, "FORKLIFT", 0.8, BBox::new(600.0, 10.0, 660.0, 50.0)),
        region(5, "forklift", 0.95, BBox::new(100.0, 10.0, 160.0, 50.0)),
    ];
    let events = watch.observe(&observation("s1", 0, regions, &[]));
    let tracks: Vec<u64> = events.iter().map(|e| e.track.0).collect();
    assert_eq!(tracks, [5, 4]);
    // Each event lists its own track's region first, then the others that counted, an
    // untracked one included: it gives context, though it never fires.
    let listed: Vec<Option<u64>> = events[1].regions.iter().map(|r| r.track.map(|t| t.0)).collect();
    assert_eq!(listed, [Some(4), Some(5), None]);
}

#[test]
fn conditions_that_cannot_key_an_event_are_refused() {
    assert!(WatchEvaluator::new(vec![WatchCondition::new("a:b", ["x"])]).is_err());
    assert!(WatchEvaluator::new(vec![WatchCondition::new("a", ["x"]).with_min_frames(0)]).is_err());
    assert!(
        WatchEvaluator::new(vec![WatchCondition::new("a", ["x"]).with_min_score(1.5)]).is_err()
    );
    assert!(
        WatchEvaluator::new(vec![WatchCondition::new("a", ["x"]), WatchCondition::new("a", ["y"])])
            .is_err()
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A zone drawn as a rectangle holds exactly what its box holds, whatever vertex it starts
    /// at and whichever way it winds.
    #[test]
    fn a_rectangular_zone_holds_what_its_box_holds(
        x0 in -500.0f32..500.0, y0 in -500.0f32..500.0, w in 1.0f32..800.0, h in 1.0f32..800.0,
        shift in 0usize..4, reverse in any::<bool>(),
        px in -1000.0f32..1500.0, py in -1000.0f32..1500.0,
    ) {
        let (x1, y1) = (x0 + w, y0 + h);
        let mut points = vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        points.rotate_left(shift);
        if reverse {
            points.reverse();
        }
        let zone = Zone::new(points).unwrap();
        prop_assert_eq!(zone.contains([px, py]), BBox::new(x0, y0, x1, y1).contains([px, py]));
        prop_assert!(zone.contains([x0, y0]), "the top-left corner is inside, half-open");
        prop_assert!(!zone.contains([x1, y1]), "the bottom-right corner is outside");
    }

    /// Any polygon: where its vertex list starts changes nothing, and nothing beyond its
    /// bounds is inside.
    #[test]
    fn a_zone_ignores_where_its_vertices_start(
        points in proptest::collection::vec((-100.0f32..100.0, -100.0f32..100.0), 3..12),
        shift in 0usize..12,
        px in -150.0f32..150.0, py in -150.0f32..150.0,
    ) {
        let points: Vec<[f32; 2]> = points.into_iter().map(|(x, y)| [x, y]).collect();
        let mut shifted = points.clone();
        shifted.rotate_left(shift % points.len());
        let zone = Zone::new(points.clone()).unwrap();
        prop_assert_eq!(zone.contains([px, py]), Zone::new(shifted).unwrap().contains([px, py]));
        let bounds = BBox::covering(points.iter().copied()).unwrap();
        if px < bounds.x0 || px > bounds.x1 || py < bounds.y0 || py > bounds.y1 {
            prop_assert!(!zone.contains([px, py]));
        }
    }

    /// A track fires exactly once, on the frame its first run of `min_frames` consecutive
    /// sightings completes; a frame without it starts the count again.
    #[test]
    fn a_track_fires_when_its_first_full_streak_completes(
        pattern in proptest::collection::vec(any::<bool>(), 1..80),
        min_frames in 1u32..6,
    ) {
        let mut watch = seal_watch(min_frames, 0);
        let mut events = Vec::new();
        for (index, present) in pattern.iter().enumerate() {
            let regions = if *present { vec![seal(1)] } else { Vec::new() };
            // The tracker holds the track through its misses.
            events.extend(watch.observe(&observation("s1", index as u64, regions, &[1])));
        }
        match first_streak(&pattern, min_frames as usize) {
            Some(at) => {
                prop_assert_eq!(events.len(), 1);
                prop_assert_eq!(events[0].frame.index, at as u64);
                prop_assert_eq!(events[0].frames, min_frames);
            }
            None => prop_assert!(events.is_empty()),
        }
    }

    /// Firings of one condition are at least a cooldown apart, each track fires at most once,
    /// and without a cooldown every track in view long enough fires.
    #[test]
    fn firings_are_a_cooldown_apart_and_once_per_track(
        tracks in proptest::collection::vec((0u64..80, 1u64..40), 1..8),
        cooldown_ms in prop_oneof![Just(0u64), 1u64..3_000],
    ) {
        let mut watch = seal_watch(2, cooldown_ms);
        let mut events = Vec::new();
        for index in 0..140u64 {
            let mut regions = Vec::new();
            let mut held = Vec::new();
            for (id, (from, length)) in tracks.iter().enumerate() {
                if (*from..from + length).contains(&index) {
                    regions.push(seal(id as u64));
                    held.push(id as u64);
                }
            }
            events.extend(watch.observe(&observation("s1", index, regions, &held)));
        }
        let fired: Vec<u64> = events.iter().map(|e| e.track.0).collect();
        let unique: HashSet<u64> = fired.iter().copied().collect();
        prop_assert_eq!(unique.len(), fired.len(), "once per track: {:?}", fired);
        for pair in events.windows(2) {
            let gap = (pair[1].frame.captured_at - pair[0].frame.captured_at).num_milliseconds();
            prop_assert!(gap as u64 >= cooldown_ms, "{} ms apart under a {} ms cooldown", gap, cooldown_ms);
        }
        if cooldown_ms == 0 {
            let long_enough = tracks.iter().filter(|(_, length)| *length >= 2).count();
            prop_assert_eq!(fired.len(), long_enough);
        }
    }

    /// Delivering observations twice, or a stale one late, changes no event; every key in a
    /// session is distinct.
    #[test]
    fn redelivery_and_late_frames_change_nothing(
        pattern in proptest::collection::vec(proptest::collection::vec(any::<bool>(), 3), 1..60),
        replays in proptest::collection::vec(any::<bool>(), 60),
        stale in proptest::collection::vec(0usize..60, 60),
    ) {
        let frame = |index: usize| {
            let regions: Vec<Region> = pattern[index]
                .iter()
                .enumerate()
                .filter(|(_, present)| **present)
                .map(|(track, _)| seal(track as u64))
                .collect();
            observation("s1", index as u64, regions, &[0, 1, 2])
        };
        let mut clean = seal_watch(2, 0);
        let expected: Vec<WatchEvent> = (0..pattern.len()).flat_map(|i| clean.observe(&frame(i))).collect();

        let mut noisy = seal_watch(2, 0);
        let mut got = Vec::new();
        for index in 0..pattern.len() {
            got.extend(noisy.observe(&frame(index)));
            if replays[index] {
                got.extend(noisy.observe(&frame(index)));
            }
            got.extend(noisy.observe(&frame(stale[index] % (index + 1))));
        }
        prop_assert_eq!(&got, &expected);
        let keys: HashSet<String> = got.iter().map(WatchEvent::key).collect();
        prop_assert_eq!(keys.len(), got.len());
    }
}
