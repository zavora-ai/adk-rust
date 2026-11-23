//! Video clips and the video analyzers (spec 3.7 §7.13).
//!
//! A [`SourceClip`] is an MP4 or MOV file read once: its digest, and the codec, size,
//! duration and frame count its container states. The runtime hands an on-device
//! [`VideoAnalyzer`](crate::VideoAnalyzer) a [`SentClip`] and a remote one only a
//! [`ReleasedClip`](crate::ReleasedClip), which the egress gate makes after it checked the
//! destination and the rule and recorded the release.
//!
//! | Analyzer | Requests | Input | Feature |
//! |----------|----------|-------|---------|
//! | `FramesAnalyzer` | `Ask`, `Caption` | sampled frames, on any vision-language model (Qwen3-VL on this machine, or a remote one through the gate), optionally after a `VisibilityPass` | `cloud` |
//! | `GeminiVideoAnalyzer` | `Ask` | a clip, as Gemini native video: inline up to 20 MB, else uploaded once through the File API and sent as a file part | `gemini-video` |
//! | `sam::SamVideoTracker` | `Track` | a clip, through `sam-3.1` video: masks or boxes with stable object identifiers | `sam` |

#[cfg(feature = "cloud")]
pub(crate) mod answer;
#[cfg(feature = "cloud")]
mod frames;
#[cfg(feature = "gemini-video")]
mod gemini;
mod mp4;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(feature = "cloud")]
pub use frames::{DEFAULT_CAPTION_INSTRUCTIONS, FramesAnalyzer, VisibilityPass};
#[cfg(feature = "gemini-video")]
pub use gemini::{GEMINI_INLINE_LIMIT, GeminiVideoAnalyzer, MediaResolution};
pub use mp4::VideoCodec;

use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::media::{ArtifactLocator, VideoRef};

/// The largest clip accepted: Gemini's File API limit, 2 GB.
pub const MAX_CLIP_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// What a clip's container states about its video track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ClipInfo {
    /// `video/mp4` or `video/quicktime`.
    pub mime_type: String,
    /// The codec.
    pub codec: VideoCodec,
    /// Coded width in pixels.
    pub width: u32,
    /// Coded height in pixels.
    pub height: u32,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Frames in the video track.
    pub frames: u64,
}

impl ClipInfo {
    /// Frames per second, from the frame count and duration.
    pub fn frame_rate(&self) -> f64 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        self.frames as f64 * 1_000.0 / self.duration_ms as f64
    }

    /// The frames a backend that samples at `fps` analyses: one per `1 / fps` seconds, at
    /// least one, at most every frame.
    pub fn sampled_frames(&self, fps: f32) -> u64 {
        if !(fps.is_finite() && fps > 0.0) {
            return self.frames;
        }
        let sampled = (self.duration_ms as f64 * f64::from(fps) / 1_000.0).ceil() as u64;
        sampled.clamp(1, self.frames.max(1))
    }
}

/// An MP4 or MOV clip, read once: its bytes, their digest, and what its container states.
///
/// # Example
///
/// ```no_run
/// use adk_vision::SourceClip;
///
/// let clip = SourceClip::decode(std::fs::read("aisle.mp4").unwrap())?;
/// println!("{} frames over {} ms", clip.info().frames, clip.info().duration_ms);
/// # Ok::<(), adk_vision::VisionError>(())
/// ```
#[derive(Clone, Debug)]
pub struct SourceClip {
    bytes: Arc<[u8]>,
    digest: Digest,
    info: ClipInfo,
    artifact: Option<ArtifactLocator>,
}

impl SourceClip {
    /// Reads a clip, checking its container from the header before anything else: MP4 or
    /// MOV with an H.264, H.265 or MPEG-4 video track, at most [`MAX_CLIP_BYTES`].
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] above the limit, for another container or codec, or
    /// for a container that states no duration or frame count.
    pub fn decode(bytes: impl Into<Arc<[u8]>>) -> Result<Self> {
        let bytes: Arc<[u8]> = bytes.into();
        if bytes.len() > MAX_CLIP_BYTES {
            return Err(VisionError::InvalidInput(format!(
                "the clip is {} bytes, over the {MAX_CLIP_BYTES}-byte cap; split or re-encode it",
                bytes.len()
            )));
        }
        let probe = mp4::probe(&bytes)
            .map_err(|reason| VisionError::InvalidInput(format!("unsupported video: {reason}")))?;
        let info = ClipInfo {
            mime_type: probe.mime_type.to_string(),
            codec: probe.codec,
            width: probe.width,
            height: probe.height,
            duration_ms: probe.duration_ms,
            frames: probe.frames,
        };
        Ok(Self { digest: Digest::of(&bytes), bytes, info, artifact: None })
    }

    /// Reads a clip stored as `video`, checking that the bytes are the ones it names.
    ///
    /// # Errors
    ///
    /// As [`decode`](Self::decode), and [`VisionError::InvalidInput`] when the bytes'
    /// digest is not the reference's.
    pub fn from_ref(video: &VideoRef, bytes: impl Into<Arc<[u8]>>) -> Result<Self> {
        let mut clip = Self::decode(bytes)?;
        if clip.digest != video.digest {
            return Err(VisionError::InvalidInput(format!(
                "the clip's bytes hash to {}, not to {} as its reference says",
                clip.digest, video.digest
            )));
        }
        clip.artifact = Some(video.artifact.clone());
        Ok(clip)
    }

    /// The encoded bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// `sha256:` of the bytes.
    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// What the container states.
    pub fn info(&self) -> &ClipInfo {
        &self.info
    }

    /// `(width, height)` of the coded frames.
    pub fn size(&self) -> (u32, u32) {
        (self.info.width, self.info.height)
    }

    /// The artifact the clip is stored in, when it was read from one.
    pub fn artifact(&self) -> Option<&ArtifactLocator> {
        self.artifact.as_ref()
    }

    /// A reference to the clip stored as `artifact`.
    pub fn to_video_ref(&self, artifact: ArtifactLocator) -> VideoRef {
        VideoRef {
            artifact,
            digest: self.digest,
            mime_type: self.info.mime_type.clone(),
            duration_ms: self.info.duration_ms,
            width: self.info.width,
            height: self.info.height,
        }
    }
}

/// The clip an on-device video backend sees. Only the runtime constructs one.
#[derive(Clone, Debug)]
pub struct SentClip {
    clip: SourceClip,
}

impl SentClip {
    pub(crate) fn new(clip: SourceClip) -> Self {
        Self { clip }
    }

    /// The clip.
    pub fn clip(&self) -> &SourceClip {
        &self.clip
    }

    /// The encoded bytes.
    pub fn bytes(&self) -> &[u8] {
        self.clip.bytes()
    }

    /// What the container states.
    pub fn info(&self) -> &ClipInfo {
        self.clip.info()
    }
}

/// At most `n` of `items`, evenly spaced, the first and last always among them.
#[cfg(feature = "cloud")]
pub(crate) fn evenly_spaced<T>(items: &[T], n: usize) -> Vec<&T> {
    if items.len() <= n {
        return items.iter().collect();
    }
    if n <= 1 {
        return items.get(items.len() / 2).into_iter().collect();
    }
    let last = items.len() - 1;
    (0..n).map(|i| &items[(i * last + (n - 1) / 2) / (n - 1)]).collect()
}

/// `m:ss.s` for a time in milliseconds, the form timestamps take in prompts and answers.
#[cfg(feature = "cloud")]
pub(crate) fn timestamp(ms: u64) -> String {
    let tenths = ms / 100;
    format!("{}:{:02}.{}", tenths / 600, (tenths / 10) % 60, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_are_read_once_and_digested() {
        let bytes = mp4::tests::mp4(b"avc1", 1280, 720, 1_200_000, 36_000, 64);
        let clip = SourceClip::decode(bytes.clone()).unwrap();
        assert_eq!(clip.digest(), Digest::of(&bytes));
        assert_eq!(clip.size(), (1280, 720));
        assert!((clip.info().frame_rate() - 30.0).abs() < 1e-9);
        // Twenty minutes at Gemini's default 1 fps is 1,200 frames.
        assert_eq!(clip.info().sampled_frames(1.0), 1_200);
        assert_eq!(clip.info().sampled_frames(60.0), 36_000);
        assert_eq!(clip.info().sampled_frames(0.0), 36_000);
        let video = clip.to_video_ref(ArtifactLocator::new("a", "u", "s", "aisle.mp4", 1));
        assert!(SourceClip::from_ref(&video, bytes).is_ok());
        let other = mp4::tests::mp4(b"avc1", 1280, 720, 1_000, 30, 1);
        assert!(SourceClip::from_ref(&video, other).is_err());
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn evenly_spaced_keeps_the_ends() {
        assert_eq!(evenly_spaced(&[1, 2, 3, 4, 5, 6, 7, 8], 4), vec![&1, &3, &6, &8]);
        assert_eq!(evenly_spaced(&[1, 2, 3], 1), vec![&2]);
        assert_eq!(evenly_spaced(&[1, 2], 4), vec![&1, &2]);
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn timestamps_read_as_minutes_and_seconds() {
        assert_eq!(timestamp(0), "0:00.0");
        assert_eq!(timestamp(83_450), "1:23.4");
        assert_eq!(timestamp(1_200_000), "20:00.0");
    }
}
