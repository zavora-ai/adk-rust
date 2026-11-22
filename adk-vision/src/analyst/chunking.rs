//! How the video analyst cuts a stream of frames into chunks.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{Result, VisionError};
use crate::media::{DecodedImage, Frame, FrameMeta, FramePixels};

/// The side of the grey thumbnail a scene cut compares, in pixels: 32 by 18.
const THUMB: (u32, u32) = (32, 18);

/// How the [`VideoAnalyst`](super::VideoAnalyst) cuts a video into the chunks it captions.
///
/// # Example
///
/// ```
/// use adk_vision::analyst::Chunking;
///
/// // Thirty-second windows from the start of the video, NVIDIA's chunk, caption and index
/// // pattern with a fixed chunk.
/// let fixed = Chunking::fixed(30_000);
/// // A cut wherever the picture changes, every chunk 5 to 60 seconds long.
/// let scenes = Chunking::scene_change().with_threshold(0.15).with_bounds(5_000, 60_000);
/// assert_ne!(fixed, scenes);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
#[non_exhaustive]
pub enum Chunking {
    /// Windows of `window_ms` counted from presentation time zero: chunk `k` covers
    /// `[k × window_ms, (k + 1) × window_ms)`.
    Fixed {
        /// The window length in milliseconds.
        window_ms: u64,
    },
    /// A cut before each frame that differs from the one before it by more than `threshold`
    /// — the mean absolute difference of their 32×18 grey thumbnails, from 0 to 1 — once
    /// the chunk is `min_ms` long, and a cut whenever it reaches `max_ms`.
    SceneChange {
        /// The difference that starts a new chunk, from 0 to 1.
        threshold: f32,
        /// The shortest chunk in milliseconds.
        min_ms: u64,
        /// The longest chunk in milliseconds.
        max_ms: u64,
    },
}

impl Default for Chunking {
    /// Thirty-second windows.
    fn default() -> Self {
        Self::fixed(30_000)
    }
}

impl Chunking {
    /// Windows of `window_ms` from presentation time zero.
    pub fn fixed(window_ms: u64) -> Self {
        Self::Fixed { window_ms }
    }

    /// Scene cuts at a difference of 0.12, every chunk 5 to 60 seconds long.
    pub fn scene_change() -> Self {
        Self::SceneChange { threshold: 0.12, min_ms: 5_000, max_ms: 60_000 }
    }

    /// Sets the difference that starts a new scene; a fixed chunking is unchanged.
    pub fn with_threshold(self, value: f32) -> Self {
        match self {
            Self::SceneChange { min_ms, max_ms, .. } => {
                Self::SceneChange { threshold: value, min_ms, max_ms }
            }
            fixed @ Self::Fixed { .. } => fixed,
        }
    }

    /// Sets the shortest and longest scene; a fixed chunking is unchanged.
    pub fn with_bounds(self, min_ms: u64, max_ms: u64) -> Self {
        match self {
            Self::SceneChange { threshold, .. } => Self::SceneChange { threshold, min_ms, max_ms },
            fixed @ Self::Fixed { .. } => fixed,
        }
    }

    /// Refuses a chunking that cannot cut: an empty window, a threshold outside 0–1, or
    /// bounds out of order.
    pub(crate) fn validate(&self) -> Result<()> {
        let valid = match *self {
            Self::Fixed { window_ms } => window_ms > 0,
            Self::SceneChange { threshold, min_ms, max_ms } => {
                (0.0..=1.0).contains(&threshold) && max_ms > 0 && min_ms <= max_ms
            }
        };
        if valid {
            Ok(())
        } else {
            Err(VisionError::InvalidInput(format!(
                "{self:?} cannot cut a video: a window must be at least 1 ms, a threshold \
                 between 0 and 1, and the shortest scene no longer than the longest"
            )))
        }
    }
}

/// A chunk the cutter closed: its time range and every frame in it.
#[derive(Debug)]
pub(crate) struct PendingChunk {
    pub index: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub frames: Vec<(FrameMeta, Arc<DecodedImage>)>,
}

/// Cuts frames, in presentation order, into chunks.
pub(crate) struct ChunkCutter {
    chunking: Chunking,
    step_ms: u64,
    next_index: u64,
    open: Option<Open>,
}

struct Open {
    start_ms: u64,
    /// The fixed window the chunk is, for `Fixed`.
    window: u64,
    thumb: Vec<f32>,
    frames: Vec<(FrameMeta, Arc<DecodedImage>)>,
}

impl ChunkCutter {
    /// A cutter for frames `step_ms` apart; the last chunk ends `step_ms` after its last
    /// frame.
    pub fn new(chunking: Chunking, step_ms: u64) -> Self {
        Self { chunking, step_ms, next_index: 0, open: None }
    }

    /// Adds `frame`; returns the chunk it closed, if it starts a new one.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a stored frame (the analyst reads frames as a
    /// source decodes them) or one earlier than the frame before it.
    pub fn push(&mut self, frame: Frame) -> Result<Option<PendingChunk>> {
        let meta = frame.meta();
        let FramePixels::Ephemeral(pixels) = frame.pixels else {
            return Err(VisionError::InvalidInput(format!(
                "frame {} of `{}` is already stored; the analyst reads frames as their source \
                 decodes them",
                meta.index, meta.source.0
            )));
        };
        let thumb = match self.chunking {
            Chunking::SceneChange { .. } => thumbnail(&pixels),
            Chunking::Fixed { .. } => Vec::new(),
        };
        let pts = meta.pts_ms;
        let Some(open) = &mut self.open else {
            self.open = Some(self.start(pts, thumb, (meta, pixels)));
            return Ok(None);
        };
        if let Some((last, _)) = open.frames.last()
            && pts < last.pts_ms
        {
            return Err(VisionError::InvalidInput(format!(
                "frame {} of `{}` at {pts} ms comes after a frame at {} ms; frames must be in \
                 presentation order",
                meta.index, meta.source.0, last.pts_ms
            )));
        }
        let cut = match self.chunking {
            Chunking::Fixed { window_ms } => pts / window_ms != open.window,
            Chunking::SceneChange { threshold, min_ms, max_ms } => {
                let length = pts.saturating_sub(open.start_ms);
                length >= max_ms || (length >= min_ms && difference(&open.thumb, &thumb) > threshold)
            }
        };
        if !cut {
            open.thumb = thumb;
            open.frames.push((meta, pixels));
            return Ok(None);
        }
        let closed = self.open.take().map(|open| self.close(open, Some(pts)));
        self.open = Some(self.start(pts, thumb, (meta, pixels)));
        Ok(closed)
    }

    /// Closes the open chunk, at the end of the stream.
    pub fn finish(&mut self) -> Option<PendingChunk> {
        self.open.take().map(|open| self.close(open, None))
    }

    fn start(&self, pts: u64, thumb: Vec<f32>, frame: (FrameMeta, Arc<DecodedImage>)) -> Open {
        let (start_ms, window) = match self.chunking {
            Chunking::Fixed { window_ms } => ((pts / window_ms) * window_ms, pts / window_ms),
            Chunking::SceneChange { .. } => (pts, 0),
        };
        Open { start_ms, window, thumb, frames: vec![frame] }
    }

    /// The chunk `open`, ending where the next chunk starts, at its window's end, or
    /// `step_ms` after its last frame.
    fn close(&mut self, open: Open, next: Option<u64>) -> PendingChunk {
        let last = open.frames.last().map_or(open.start_ms, |(m, _)| m.pts_ms);
        let end_ms = match (self.chunking, next) {
            (Chunking::Fixed { window_ms }, _) => {
                let window_end = open.start_ms + window_ms;
                window_end.min(next.unwrap_or(window_end)).min(last + self.step_ms.max(1))
                    .max(last + 1)
            }
            (Chunking::SceneChange { .. }, Some(next)) => next,
            (Chunking::SceneChange { .. }, None) => last + self.step_ms.max(1),
        };
        let index = self.next_index;
        self.next_index += 1;
        PendingChunk { index, start_ms: open.start_ms, end_ms, frames: open.frames }
    }
}

/// `pixels` as a 32×18 grey thumbnail, each value 0–1, by averaging the pixels of each cell.
fn thumbnail(pixels: &DecodedImage) -> Vec<f32> {
    let (w, h) = pixels.size();
    let (tw, th) = THUMB;
    let mut sums = vec![0.0f32; (tw * th) as usize];
    let mut counts = vec![0u32; (tw * th) as usize];
    // A stride keeps the cost of a large frame near that of a small one.
    let stride = ((w / (tw * 8)).max(1), (h / (th * 8)).max(1));
    for y in (0..h).step_by(stride.1 as usize) {
        for x in (0..w).step_by(stride.0 as usize) {
            let [r, g, b, _] = pixels.pixel(x, y);
            let cell = ((y * th / h.max(1)) * tw + x * tw / w.max(1)) as usize;
            sums[cell] += (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0;
            counts[cell] += 1;
        }
    }
    sums.iter().zip(&counts).map(|(s, &n)| if n == 0 { 0.0 } else { s / n as f32 }).collect()
}

fn difference(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 1.0;
    }
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::FrameSourceId;
    use std::time::Duration;

    fn frame(index: u64, pts_ms: u64, shade: u8) -> Frame {
        Frame {
            source: FrameSourceId("cam".into()),
            index,
            pts: Duration::from_millis(pts_ms),
            captured_at: chrono::DateTime::from_timestamp(1_763_380_800, 0).unwrap(),
            pixels: FramePixels::Ephemeral(Arc::new(DecodedImage::filled(
                64,
                36,
                [shade, shade, shade, 255],
            ))),
        }
    }

    fn cut(chunking: Chunking, frames: &[(u64, u8)], step_ms: u64) -> Vec<(u64, u64, usize)> {
        let mut cutter = ChunkCutter::new(chunking, step_ms);
        let mut out = Vec::new();
        for (i, &(pts, shade)) in frames.iter().enumerate() {
            if let Some(c) = cutter.push(frame(i as u64, pts, shade)).unwrap() {
                out.push((c.start_ms, c.end_ms, c.frames.len()));
            }
        }
        out.extend(cutter.finish().map(|c| (c.start_ms, c.end_ms, c.frames.len())));
        out
    }

    #[test]
    fn fixed_windows_count_from_time_zero_and_skip_empty_windows() {
        // Frames every 10 s from 5 s, none between 45 s and 95 s.
        let frames = [(5_000, 0), (15_000, 0), (25_000, 0), (35_000, 0), (95_000, 0), (105_000, 0)];
        assert_eq!(
            cut(Chunking::fixed(30_000), &frames, 10_000),
            vec![(0, 30_000, 3), (30_000, 45_000, 1), (90_000, 115_000, 2)]
        );
    }

    #[test]
    fn scene_cuts_follow_the_picture_within_their_bounds() {
        // Dark for 20 s; bright, with a dark flicker 8 s in, inside the 10 s minimum; then
        // 70 s of grey, cut at the 40 s maximum.
        let mut frames: Vec<(u64, u8)> = (0..4).map(|i| (i * 5_000, 10)).collect();
        frames.extend([(20_000, 240), (25_000, 240), (28_000, 10), (30_000, 10)]);
        frames.extend((7..21).map(|i| (i * 5_000, 128)));
        let chunking = Chunking::scene_change().with_bounds(10_000, 40_000);
        assert_eq!(
            cut(chunking, &frames, 5_000),
            vec![(0, 20_000, 4), (20_000, 35_000, 4), (35_000, 75_000, 8), (75_000, 105_000, 6)]
        );
    }

    #[test]
    fn frames_out_of_order_and_bad_chunkings_are_refused() {
        let mut cutter = ChunkCutter::new(Chunking::fixed(1_000), 100);
        cutter.push(frame(0, 500, 0)).unwrap();
        assert!(cutter.push(frame(1, 400, 0)).is_err());
        assert!(Chunking::fixed(0).validate().is_err());
        assert!(Chunking::scene_change().with_threshold(1.5).validate().is_err());
        assert!(Chunking::scene_change().with_bounds(9, 8).validate().is_err());
        assert!(Chunking::default().validate().is_ok());
    }
}
