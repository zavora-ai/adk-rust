//! [`Sampler`]: which decoded frames a source keeps, and at what size.

use std::time::Duration;

use crate::edit::ops;
use crate::error::{Result, VisionError};
use crate::media::DecodedImage;
use crate::traits::Sampling;

/// Keeps frames at most [`Sampling::fps`] apart by presentation time and downscales them to
/// [`Sampling::max_long_edge`].
///
/// Admission follows a grid of one slot per `1 / fps`: the first frame is kept, then the
/// first frame at or after each next slot, with a slack of an eighth of the interval for
/// timestamp jitter. A frame kept late moves the grid to itself, so a source that stalls
/// never bursts to catch up; a timestamp that goes backwards (a restarted stream) restarts
/// the grid. Kept frames are therefore at least seven eighths of an interval apart, and a
/// source faster than `fps` yields `fps` frames a second.
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use adk_vision::Sampling;
/// use adk_vision::stream::Sampler;
///
/// // A 30 fps source sampled at 5 fps keeps every sixth frame.
/// let mut sampler = Sampler::new(Sampling { fps: 5.0, max_long_edge: Some(640) }).unwrap();
/// let kept: Vec<u64> = (0..30u64)
///     .filter(|i| sampler.admit(Duration::from_micros(i * 33_333)))
///     .collect();
/// assert_eq!(kept, vec![0, 6, 12, 18, 24]);
/// assert_eq!(sampler.fitted_size(1920, 1080), (640, 360));
/// ```
#[derive(Clone, Debug)]
pub struct Sampler {
    interval: Duration,
    slack: Duration,
    max_long_edge: Option<u32>,
    next_due: Option<Duration>,
    last: Option<Duration>,
}

impl Sampler {
    /// A sampler for `sampling`.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when `fps` is not a finite positive number, or
    /// `max_long_edge` is zero.
    pub fn new(sampling: Sampling) -> Result<Self> {
        if !sampling.fps.is_finite() || sampling.fps <= 0.0 {
            return Err(VisionError::InvalidInput(format!(
                "a sampling rate must be a positive number of frames a second; got {}",
                sampling.fps
            )));
        }
        if sampling.max_long_edge == Some(0) {
            return Err(VisionError::InvalidInput(
                "max_long_edge must be at least 1 pixel; leave it unset to keep the source size"
                    .into(),
            ));
        }
        let interval = Duration::from_secs_f64(1.0 / f64::from(sampling.fps));
        Ok(Self {
            interval,
            slack: interval / 8,
            max_long_edge: sampling.max_long_edge,
            next_due: None,
            last: None,
        })
    }

    /// The time between two kept frames.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Whether to keep the frame presented at `pts`, counted from the start of the stream.
    pub fn admit(&mut self, pts: Duration) -> bool {
        let backwards = self.last.is_some_and(|last| pts < last);
        let keep = match self.next_due {
            None => true,
            Some(_) if backwards => true,
            Some(due) => pts + self.slack >= due,
        };
        if keep {
            let due = self.next_due.filter(|_| !backwards).unwrap_or(pts);
            // A frame on time keeps the grid; a late one moves it, so the next frame is at
            // least an interval less the slack after this one and a stalled source never
            // bursts to catch up.
            self.next_due = Some((due + self.interval).max(pts + self.interval));
            self.last = Some(pts);
        }
        keep
    }

    /// The size a `width` × `height` frame is scaled to: unchanged when its long edge is
    /// within `max_long_edge`, otherwise scaled down to it with the aspect ratio kept.
    pub fn fitted_size(&self, width: u32, height: u32) -> (u32, u32) {
        let Some(max) = self.max_long_edge else { return (width, height) };
        let long = width.max(height);
        if long <= max {
            return (width, height);
        }
        let scale = f64::from(max) / f64::from(long);
        let side = |v: u32| ((f64::from(v) * scale).round() as u32).clamp(1, max);
        (side(width), side(height))
    }

    /// `pixels` scaled to [`fitted_size`](Self::fitted_size).
    pub fn fit(&self, pixels: DecodedImage) -> DecodedImage {
        let (width, height) = pixels.size();
        let (w, h) = self.fitted_size(width, height);
        if (w, h) == (width, height) { pixels } else { ops::resize(&pixels, w, h) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sampler(fps: f32) -> Sampler {
        Sampler::new(Sampling { fps, max_long_edge: None }).unwrap()
    }

    #[test]
    fn invalid_rates_are_refused() {
        for fps in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(Sampler::new(Sampling { fps, max_long_edge: None }).is_err(), "{fps}");
        }
        assert!(Sampler::new(Sampling { fps: 1.0, max_long_edge: Some(0) }).is_err());
    }

    #[test]
    fn a_slow_source_is_kept_whole() {
        let mut s = sampler(10.0);
        assert!((0..20u64).all(|i| s.admit(Duration::from_millis(i * 500))));
    }

    #[test]
    fn a_stall_re_anchors_instead_of_bursting() {
        let mut s = sampler(10.0);
        assert!(s.admit(Duration::ZERO));
        // Five seconds of nothing, then a 30 fps burst: one frame per 100 ms again.
        let kept: Vec<u64> =
            (0..9u64).filter(|i| s.admit(Duration::from_millis(5_000 + i * 33))).collect();
        assert_eq!(kept, vec![0, 3, 6]);
    }

    #[test]
    fn backwards_timestamps_restart_the_grid() {
        let mut s = sampler(1.0);
        assert!(s.admit(Duration::from_secs(10)));
        assert!(!s.admit(Duration::from_millis(10_500)));
        assert!(s.admit(Duration::from_secs(2)));
        assert!(!s.admit(Duration::from_millis(2_500)));
        assert!(s.admit(Duration::from_secs(3)));
    }

    #[test]
    fn frames_are_only_ever_scaled_down() {
        let s = Sampler::new(Sampling { fps: 1.0, max_long_edge: Some(320) }).unwrap();
        assert_eq!(s.fitted_size(640, 360), (320, 180));
        assert_eq!(s.fitted_size(360, 640), (180, 320));
        assert_eq!(s.fitted_size(200, 100), (200, 100));
        let fitted = s.fit(DecodedImage::filled(640, 480, [9; 4]));
        assert_eq!(fitted.size(), (320, 240));
    }
}
