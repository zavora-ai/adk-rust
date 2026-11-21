//! [`ByteTracker`]: ByteTrack over `trackforge` 0.5 (spec 3.7 §7.13), feature `track`.

use std::collections::{BTreeMap, HashMap};

use trackforge::trackers::byte_track::{ByteTrack, ByteTrackParams};
use trackforge::trackers::common::CommonParams;

use crate::geometry::{Region, TrackId};
use crate::media::FrameMeta;
use crate::traits::Tracker;

/// Settings for [`ByteTracker`]: when a detection starts or continues a track, how much it
/// must overlap a track to continue it, and how long a lost track waits to be found again.
///
/// # Example
///
/// ```
/// use adk_vision::ByteTrackConfig;
///
/// // Ride out occlusions of up to two seconds of a 5 fps stream.
/// let config = ByteTrackConfig::default().with_max_missed(10);
/// assert_eq!(config.max_missed, 10);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ByteTrackConfig {
    /// Frames a track that stopped matching is kept, so an object hidden for up to this
    /// many frames keeps its identifier when it reappears.
    pub max_missed: u32,
    /// Detections scoring at least this are matched first; lower ones only continue
    /// tracks the first pass left unmatched (ByteTrack's recovery step).
    pub high_score: f32,
    /// The score an unmatched detection needs to start a new track.
    pub new_track_score: f32,
    /// The overlap (intersection over union) a high-scoring detection needs with a track's
    /// predicted box to continue it.
    pub match_iou: f32,
    /// The overlap a low-scoring detection needs to continue a track.
    pub low_match_iou: f32,
}

impl Default for ByteTrackConfig {
    /// ByteTrack's reference settings: 30 frames, 0.5 and 0.6 score thresholds, matches at
    /// 0.2 overlap for high-scoring detections and 0.5 for low-scoring ones.
    fn default() -> Self {
        Self {
            max_missed: 30,
            high_score: 0.5,
            new_track_score: 0.6,
            match_iou: 0.2,
            low_match_iou: 0.5,
        }
    }
}

impl ByteTrackConfig {
    /// Keeps a lost track for `frames` frames.
    pub fn with_max_missed(mut self, frames: u32) -> Self {
        self.max_missed = frames;
        self
    }

    /// Sets the high-score and new-track thresholds.
    pub fn with_scores(mut self, high_score: f32, new_track_score: f32) -> Self {
        self.high_score = high_score;
        self.new_track_score = new_track_score;
        self
    }

    /// Sets the overlaps a high- and a low-scoring detection need to continue a track.
    pub fn with_match_iou(mut self, high: f32, low: f32) -> Self {
        self.match_iou = high;
        self.low_match_iou = low;
        self
    }

    fn params(&self) -> ByteTrackParams {
        ByteTrackParams {
            common: CommonParams::new(self.max_missed as usize, 1),
            track_thresh: self.high_score,
            // trackforge takes the largest IoU distance (one minus IoU) that still matches.
            match_thresh: 1.0 - self.match_iou.clamp(0.0, 1.0),
            det_thresh: self.new_track_score,
            second_match_thresh: 1.0 - self.low_match_iou.clamp(0.0, 1.0),
        }
    }
}

/// A detection as trackforge takes it, with its index in the frame's regions.
type Detection = (usize, [f32; 4], f32);

/// One label's ByteTrack, and the anonymous identifiers its tracks were given.
struct Lane {
    tracker: ByteTrack,
    /// trackforge's track id to this tracker's identifier and the update it last matched.
    ids: HashMap<u64, (TrackId, u64)>,
}

/// ByteTrack (Zhang et al., 2022) through `trackforge` 0.5 (MIT): a Kalman filter predicts
/// each track's box, high-scoring detections match tracks first by overlap, low-scoring
/// detections then recover the tracks left over, and a lost track is kept for
/// [`max_missed`](ByteTrackConfig::max_missed) frames so an occluded object keeps its
/// identifier when it reappears.
///
/// Each label is tracked on its own, so a person and a forklift never swap identifiers.
/// Identifiers are anonymous counters shared across labels that restart on
/// [`reset`](Tracker::reset) (V-I8). A detection that starts no track and continues none —
/// below [`new_track_score`](ByteTrackConfig::new_track_score) with nothing to recover —
/// comes back with `track: None`, as does a region with no area. A region with no score
/// counts as certain. Call [`update`](Tracker::update) once per sampled frame: the motion
/// model steps one frame per call.
///
/// # Example
///
/// ```
/// use adk_vision::{ByteTracker, Tracker};
///
/// let mut tracker = ByteTracker::default();
/// tracker.reset();
/// ```
pub struct ByteTracker {
    config: ByteTrackConfig,
    lanes: BTreeMap<Option<String>, Lane>,
    next: u64,
    updates: u64,
}

impl std::fmt::Debug for ByteTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ByteTracker")
            .field("config", &self.config)
            .field("labels", &self.lanes.len())
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

impl Default for ByteTracker {
    fn default() -> Self {
        Self::new(ByteTrackConfig::default())
    }
}

impl ByteTracker {
    /// A tracker with `config`.
    pub fn new(config: ByteTrackConfig) -> Self {
        Self { config, lanes: BTreeMap::new(), next: 0, updates: 0 }
    }

    /// The settings.
    pub fn config(&self) -> &ByteTrackConfig {
        &self.config
    }
}

impl Tracker for ByteTracker {
    fn update(&mut self, _frame: &FrameMeta, mut detections: Vec<Region>) -> Vec<Region> {
        self.updates += 1;
        // Each label's detections: index in `detections`, TLWH box, score.
        let mut groups: BTreeMap<Option<String>, Vec<Detection>> = BTreeMap::new();
        for (i, region) in detections.iter_mut().enumerate() {
            region.track = None;
            let Some(b) = region.shape.bbox() else { continue };
            let (w, h) = (b.x1 - b.x0, b.y1 - b.y0);
            if !(w > 0.0 && h > 0.0 && b.x0.is_finite() && b.y0.is_finite()) {
                continue;
            }
            let score = region.score.unwrap_or(1.0);
            groups.entry(region.label.clone()).or_default().push((i, [b.x0, b.y0, w, h], score));
        }
        for label in groups.keys() {
            if !self.lanes.contains_key(label) {
                let lane = Lane {
                    tracker: ByteTrack::from_params(self.config.params()),
                    ids: HashMap::new(),
                };
                self.lanes.insert(label.clone(), lane);
            }
        }
        // Every lane steps each frame, so an absent label ages its lost tracks too; lanes
        // step in label order, so identifiers are handed out the same way on every run.
        let horizon = u64::from(self.config.max_missed) + 1;
        let (updates, next) = (self.updates, &mut self.next);
        for (label, lane) in &mut self.lanes {
            let group = groups.remove(label).unwrap_or_default();
            let input = group.iter().map(|(_, tlwh, score)| (*tlwh, *score, 0i64)).collect();
            for track in lane.tracker.update(input) {
                let Some(&(index, _, _)) = track.det_ind.and_then(|d| group.get(d)) else {
                    continue;
                };
                let entry = lane.ids.entry(track.track_id).or_insert_with(|| {
                    let id = TrackId(*next);
                    *next += 1;
                    (id, updates)
                });
                entry.1 = updates;
                detections[index].track = Some(entry.0);
            }
            lane.ids.retain(|_, (_, seen)| updates - *seen <= horizon);
        }
        self.lanes.retain(|_, lane| !lane.ids.is_empty());
        detections
    }

    fn reset(&mut self) {
        self.lanes.clear();
        self.next = 0;
        self.updates = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Digest;
    use crate::backend::BackendId;
    use crate::geometry::{BBox, CoordFrame, RegionOrigin, Shape, TransformChain};
    use crate::media::FrameSourceId;

    fn frame(index: u64) -> FrameMeta {
        FrameMeta {
            source: FrameSourceId("test".into()),
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
                backend: BackendId::new("test"),
                manifest: None,
                frame: CoordFrame::SentPixels,
                chain: TransformChain::new(640, 480),
                escalated_from: None,
            },
        }
    }

    #[test]
    fn labels_are_tracked_apart() {
        let mut tracker = ByteTracker::default();
        let same = BBox::new(100.0, 100.0, 200.0, 200.0);
        let out = tracker
            .update(&frame(0), vec![region(same, "person", 0.9), region(same, "forklift", 0.9)]);
        assert_ne!(out[0].track, out[1].track);
        let again = tracker
            .update(&frame(1), vec![region(same, "forklift", 0.9), region(same, "person", 0.9)]);
        assert_eq!((again[0].track, again[1].track), (out[1].track, out[0].track));
    }

    #[test]
    fn a_weak_detection_starts_no_track_but_continues_one() {
        let mut tracker = ByteTracker::default();
        let b = BBox::new(10.0, 10.0, 90.0, 90.0);
        assert_eq!(tracker.update(&frame(0), vec![region(b, "part", 0.3)])[0].track, None);
        let first = tracker.update(&frame(1), vec![region(b, "part", 0.9)])[0].track;
        assert!(first.is_some());
        // A low score inside the track's box recovers it (ByteTrack's second pass).
        assert_eq!(tracker.update(&frame(2), vec![region(b, "part", 0.3)])[0].track, first);
    }

    #[test]
    fn regions_without_area_carry_no_track() {
        let mut tracker = ByteTracker::default();
        let out = tracker.update(&frame(0), vec![region(BBox::new(5.0, 5.0, 5.0, 9.0), "x", 0.9)]);
        assert_eq!(out[0].track, None);
    }

    #[test]
    fn forgotten_tracks_release_their_lanes() {
        let mut tracker = ByteTracker::new(ByteTrackConfig::default().with_max_missed(2));
        tracker.update(&frame(0), vec![region(BBox::new(0.0, 0.0, 50.0, 50.0), "a", 0.9)]);
        for i in 1..6 {
            tracker.update(&frame(i), Vec::new());
        }
        assert!(tracker.lanes.is_empty());
    }
}
