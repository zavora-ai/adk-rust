//! [`SentImage`] and the [`VisionInput`] seal: what a backend is handed.

use std::sync::Arc;

use crate::digest::Digest;
use crate::egress::ReleasedImage;
use crate::error::Result;
use crate::geometry::TransformChain;
use crate::media::DecodedImage;

/// The image an on-device backend sees: pixels, the source image's digest, and the
/// transform chain from the source to these pixels.
///
/// Every trait method takes a `SentImage` (on-device backends) or a [`ReleasedImage`]
/// (remote backends), never an `ImageRef` or raw pixels. Only `VisionRuntime` constructs a
/// `SentImage`, and only the egress gate a `ReleasedImage`. A remote backend implements its
/// trait for `ReleasedImage`, so handing it a `SentImage` does not compile (V-I1):
///
/// ```compile_fail,E0308
/// use adk_vision::{DetectQuery, Detector, ReleasedImage, SentImage, VisionCx};
///
/// async fn leak(remote: &dyn Detector<ReleasedImage>, cx: &VisionCx, image: &SentImage) {
///     // A remote backend takes only an image the egress gate released.
///     let _ = remote.detect(cx, image, &DetectQuery::Concepts(vec!["face".into()])).await;
/// }
/// ```
///
/// Neither can a `SentImage` be made outside the runtime:
///
/// ```compile_fail,E0451
/// use std::sync::Arc;
/// use adk_vision::{DecodedImage, Digest, SentImage, TransformChain};
///
/// let pixels = Arc::new(DecodedImage::filled(1, 1, [0, 0, 0, 255]));
/// let forged = SentImage { source: Digest::of(b"x"), pixels, chain: TransformChain::new(1, 1) };
/// ```
#[derive(Clone, Debug)]
pub struct SentImage {
    source: Digest,
    pixels: Arc<DecodedImage>,
    chain: TransformChain,
}

impl SentImage {
    pub(crate) fn new(source: Digest, pixels: Arc<DecodedImage>, chain: TransformChain) -> Self {
        debug_assert_eq!(chain.output_size(), pixels.size());
        Self { source, pixels, chain }
    }

    /// The digest of the source image these pixels came from.
    pub fn source(&self) -> Digest {
        self.source
    }

    /// The pixels.
    pub fn pixels(&self) -> &DecodedImage {
        &self.pixels
    }

    /// `(width, height)` of the pixels.
    pub fn size(&self) -> (u32, u32) {
        self.pixels.size()
    }

    /// The transforms from the source image to these pixels.
    pub fn chain(&self) -> &TransformChain {
        &self.chain
    }

    /// The pixels encoded as PNG.
    ///
    /// # Errors
    ///
    /// As [`DecodedImage::encode_png`].
    pub fn encode_png(&self) -> Result<Vec<u8>> {
        self.pixels.encode_png()
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::SentImage {}
    impl Sealed for crate::egress::ReleasedImage {}
}

/// The two kinds of image a backend can be handed: [`SentImage`] for on-device backends
/// and [`ReleasedImage`] for remote ones. Sealed: no other type implements it.
pub trait VisionInput: sealed::Sealed + Send + Sync + 'static {
    /// `true` for [`ReleasedImage`].
    const REMOTE: bool;

    /// The pixels and transform chain.
    fn sent(&self) -> &SentImage;
}

impl VisionInput for SentImage {
    const REMOTE: bool = false;

    fn sent(&self) -> &SentImage {
        self
    }
}

impl VisionInput for ReleasedImage {
    const REMOTE: bool = true;

    fn sent(&self) -> &SentImage {
        self.image()
    }
}
