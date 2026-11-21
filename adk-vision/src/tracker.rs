//! [`GreedyIouTracker`]: a dependency-free reference [`Tracker`].

use crate::geometry::{BBox, Region, TrackId};
use crate::media::FrameMeta;
use crate::traits::Tracker;

/// Associates each detection with the live track of the same label it overlaps most, at
/// `min_iou` or better, highest score first; unmatched detections start new tracks, and a
/// track unseen for more than `max_missed` frames ends. Identifiers are anonymous counters
/// that restart on [`reset`](Tracker::reset).
///
/// A hermetic reference for tests and the conformance suite; production tracking uses the
/// `track` feature's `ByteTracker`.
///
/// # Example
///
/// ```
/// use adk_vision::{GreedyIouTracker, Tracker};
///
/// let mut tracker = GreedyIouTracker::new(0.3, 2);
/// tracker.reset();
/// ```
#[derive(Clone, Debug)]
pub struct GreedyIouTracker {
    min_iou: f32,
    max_missed: u32,
    next: u64,
    live: Vec<LiveTrack>,
}

#[derive(Clone, Debug)]
struct LiveTrack {
    id: TrackId,
    bbox: BBox,
    label: Option<String>,
    missed: u32,
}

impl GreedyIouTracker {
    /// A tracker matching at `min_iou` and dropping tracks unseen for `max_missed` frames.
    pub fn new(min_iou: f32, max_missed: u32) -> Self {
        Self { min_iou, max_missed, next: 0, live: Vec::new() }
    }
}

impl Tracker for GreedyIouTracker {
    fn update(&mut self, _frame: &FrameMeta, mut detections: Vec<Region>) -> Vec<Region> {
        let mut order: Vec<usize> = (0..detections.len()).collect();
        order.sort_by(|&a, &b| {
            let score = |i: usize| detections[i].score.unwrap_or(0.0);
            score(b).total_cmp(&score(a))
        });
        let mut matched = vec![false; self.live.len()];
        for i in order {
            let Some(bbox) = detections[i].shape.bbox() else { continue };
            let best = self
                .live
                .iter()
                .enumerate()
                .filter(|(t, track)| !matched[*t] && track.label == detections[i].label)
                .map(|(t, track)| (t, track.bbox.iou(&bbox)))
                .filter(|(_, iou)| *iou >= self.min_iou)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let id = match best {
                Some((t, _)) => {
                    matched[t] = true;
                    self.live[t].bbox = bbox;
                    self.live[t].missed = 0;
                    self.live[t].id
                }
                None => {
                    let id = TrackId(self.next);
                    self.next += 1;
                    self.live.push(LiveTrack {
                        id,
                        bbox,
                        label: detections[i].label.clone(),
                        missed: 0,
                    });
                    matched.push(true);
                    id
                }
            };
            detections[i].track = Some(id);
        }
        for (track, seen) in self.live.iter_mut().zip(&matched) {
            if !seen {
                track.missed += 1;
            }
        }
        let max_missed = self.max_missed;
        self.live.retain(|t| t.missed <= max_missed);
        detections
    }

    fn reset(&mut self) {
        self.live.clear();
        self.next = 0;
    }
}
