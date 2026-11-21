//! Stream fixtures for tests and the phase B scenarios (spec 3.7 §5, §11): the conveyor
//! clip and a local RTSP server, both in process.
//!
//! | Fixture | What it is |
//! |---------|------------|
//! | [`ConveyorClip`] | 200 printed parts on a belt, 12 with cracks of seeded length, a 24 mm fiducial in the belt's plane and a wall clock; drawn per frame from a seed, with its ground truth |
//! | [`RtspTestServer`] | RTSP on `127.0.0.1` with RTP interleaved on the connection: MJPEG (RFC 2435) encoded in process, or H.264 (RFC 6184) access units |
//! | [`H264Stream`], [`write_mp4`] | H.264 and MP4 encodings of any frames, made by a user-installed `ffmpeg` with libx264 |
//!
//! Nothing here is downloaded or committed as media: every clip is drawn at test time.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use adk_vision::stream::fixtures::{ConveyorClip, ConveyorSpec, RtspTestServer, TestVideo};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let clip = Arc::new(ConveyorClip::new(ConveyorSpec::default()));
//! let server = RtspTestServer::start(TestVideo::Mjpeg {
//!     frames: clip.frame_count(),
//!     render: Arc::new({ let clip = Arc::clone(&clip); move |i| clip.render(i) }),
//!     fps: clip.truth().spec.fps,
//!     quality: 85,
//! })
//! .await
//! .unwrap();
//! // An RtspSource reads server.url(); scoring uses clip.truth() and clip.visible(index).
//! server.shutdown().await;
//! # });
//! ```

mod conveyor;
mod h264;
mod jpeg;
mod rtsp_server;

pub use conveyor::{
    ConveyorClip, ConveyorSpec, ConveyorTruth, DefectTruth, FiducialTruth, PartTruth, VisiblePart,
};
pub use h264::{H264Stream, write_mp4};
pub use rtsp_server::{FrameRenderer, RtspTestOptions, RtspTestServer, TestVideo};

/// Encodes `image` as a baseline 4:2:0 JPEG at `quality` (1–100): the form an RTP/JPEG
/// camera sends.
///
/// # Example
///
/// ```
/// use adk_vision::stream::fixtures::encode_jpeg_420;
/// use adk_vision::{DecodeLimits, DecodedImage};
///
/// let jpeg = encode_jpeg_420(&DecodedImage::filled(32, 24, [10, 200, 30, 255]), 90);
/// let (decoded, mime) = DecodedImage::decode(&jpeg, &DecodeLimits::default()).unwrap();
/// assert_eq!((decoded.size(), mime), ((32, 24), "image/jpeg"));
/// ```
pub fn encode_jpeg_420(image: &crate::media::DecodedImage, quality: u8) -> Vec<u8> {
    jpeg::encode(image, quality).to_jpeg()
}
