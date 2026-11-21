//! The frame source conformance check (spec 3.7 §7.18 applied to §7.13's sources),
//! features `stream` and `conformance`.

use std::collections::BTreeSet;
use std::time::Duration;

use futures::StreamExt;

use super::FrameSource;
use crate::error::VisionError;
use crate::media::FramePixels;
use crate::traits::Sampling;

/// What a conforming source delivered to [`check_frame_source`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameSourceReport {
    /// Frames checked.
    pub frames: usize,
    /// The shortest time between two of them.
    pub min_gap: Option<Duration>,
    /// The frame sizes seen.
    pub sizes: BTreeSet<(u32, u32)>,
}

/// Checks that `source` keeps the [`FrameSource`] contract over `count` frames sampled at
/// `sampling`, panicking with the first breach, as the other conformance checks do:
///
/// - **Sampling.** A rate that is not a positive number is refused with `InvalidInput`.
/// - **Identity.** Every frame names the source; indices and presentation times rise.
/// - **Rate.** Frames are at least seven eighths of `1 / fps` apart.
/// - **Size.** No frame's long edge exceeds `max_long_edge`.
/// - **Privacy.** Every frame is ephemeral: a source stores nothing.
///
/// Frames must arrive within a minute.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use std::time::Duration;
/// use adk_vision::stream::{FrameSource, FrameStream, Sampler, check_frame_source};
/// use adk_vision::{DecodedImage, Frame, FramePixels, FrameSourceId, Sampling};
/// use futures::StreamExt;
///
/// /// A 30 fps source of grey frames, sampled and scaled as asked.
/// struct Grey(FrameSourceId);
///
/// #[async_trait::async_trait]
/// impl FrameSource for Grey {
///     fn id(&self) -> &FrameSourceId {
///         &self.0
///     }
///     async fn frames(&self, sampling: Sampling) -> adk_vision::Result<FrameStream> {
///         let mut sampler = Sampler::new(sampling)?;
///         let id = self.0.clone();
///         let frames: Vec<_> = (0..90u64)
///             .filter_map(|index| {
///                 let pts = Duration::from_micros(index * 33_333);
///                 sampler.admit(pts).then(|| {
///                     let pixels = sampler.fit(DecodedImage::filled(64, 48, [90; 4]));
///                     Ok(Frame {
///                         source: id.clone(),
///                         index,
///                         pts,
///                         captured_at: chrono::Utc::now(),
///                         pixels: FramePixels::Ephemeral(Arc::new(pixels)),
///                     })
///                 })
///             })
///             .collect();
///         Ok(futures::stream::iter(frames).boxed())
///     }
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let source = Grey(FrameSourceId("grey".into()));
/// let report = check_frame_source(&source, Sampling { fps: 10.0, max_long_edge: Some(32) }, 3).await;
/// assert_eq!(report.sizes.into_iter().collect::<Vec<_>>(), vec![(32, 24)]);
/// # });
/// ```
pub async fn check_frame_source(
    source: &dyn FrameSource,
    sampling: Sampling,
    count: usize,
) -> FrameSourceReport {
    let id = source.id().clone();
    match source.frames(Sampling { fps: 0.0, max_long_edge: sampling.max_long_edge }).await {
        Err(VisionError::InvalidInput(_)) => {}
        Err(other) => {
            panic!("`{}`: a zero sampling rate must be refused as invalid input, got {other}", id.0)
        }
        Ok(_) => panic!("`{}`: a zero sampling rate must be refused", id.0),
    }
    let interval = Duration::from_secs_f64(1.0 / f64::from(sampling.fps));
    let stream = source
        .frames(sampling)
        .await
        .unwrap_or_else(|err| panic!("`{}` did not open: {err}", id.0));
    let frames: Vec<_> =
        tokio::time::timeout(Duration::from_secs(60), stream.take(count).collect())
            .await
            .unwrap_or_else(|_| {
                panic!("`{}` did not deliver {count} frames within a minute", id.0)
            });
    assert_eq!(frames.len(), count, "`{}` ended after {} of {count} frames", id.0, frames.len());
    let mut report = FrameSourceReport { frames: count, ..FrameSourceReport::default() };
    let mut previous: Option<(u64, Duration)> = None;
    for frame in frames {
        let frame = frame.unwrap_or_else(|err| panic!("`{}` yielded an error: {err}", id.0));
        assert_eq!(frame.source, id, "a frame of `{}` names `{}`", id.0, frame.source.0);
        let size = match &frame.pixels {
            FramePixels::Ephemeral(pixels) => pixels.size(),
            FramePixels::Stored(image) => {
                panic!("`{}` stored frame {} as {}", id.0, frame.index, image.digest)
            }
        };
        if let Some(max) = sampling.max_long_edge {
            assert!(
                size.0.max(size.1) <= max,
                "`{}` frame {} is {size:?}, over {max} px",
                id.0,
                frame.index
            );
        }
        report.sizes.insert(size);
        if let Some((index, pts)) = previous {
            assert!(
                frame.index > index,
                "`{}` frame indices went from {index} to {}",
                id.0,
                frame.index
            );
            assert!(
                frame.pts > pts,
                "`{}` presentation times went from {pts:?} to {:?}",
                id.0,
                frame.pts
            );
            let gap = frame.pts - pts;
            assert!(
                gap + interval / 8 >= interval,
                "`{}` frames {:?} apart at {} fps",
                id.0,
                gap,
                sampling.fps
            );
            report.min_gap = Some(report.min_gap.map_or(gap, |min| min.min(gap)));
        }
        previous = Some((frame.index, frame.pts));
    }
    report
}
