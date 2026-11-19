//! Decoding under limits, and decoded pixels.

use std::io::Cursor;
use std::sync::Arc;

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};

use crate::digest::Digest;
use crate::error::{Result, VisionError};

/// Caps checked before an image is decoded.
///
/// The byte cap is checked against the encoded size and the pixel cap against the size the
/// header declares, both before any pixel buffer is allocated; the decoder then runs with
/// the declared dimensions as hard limits.
///
/// # Example
///
/// ```
/// use adk_vision::DecodeLimits;
///
/// let limits = DecodeLimits::default().with_max_pixels(4_000_000);
/// assert_eq!(limits.max_pixels, 4_000_000);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeLimits {
    /// Largest encoded image in bytes. Default 50 MiB.
    pub max_bytes: u64,
    /// Largest image in pixels. Default 50 million.
    pub max_pixels: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self { max_bytes: 50 * 1024 * 1024, max_pixels: 50_000_000 }
    }
}

impl DecodeLimits {
    /// Sets the byte cap.
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Sets the pixel cap.
    pub fn with_max_pixels(mut self, max_pixels: u64) -> Self {
        self.max_pixels = max_pixels;
        self
    }
}

/// Decoded RGBA8 pixels, row-major, EXIF orientation already applied.
#[derive(Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pixels: RgbaImage,
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width())
            .field("height", &self.height())
            .finish_non_exhaustive()
    }
}

impl DecodedImage {
    /// Pixels from a row-major RGBA8 buffer.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::InvalidInput`] when the buffer is not `width × height × 4`
    /// bytes, or either side is zero.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::DecodedImage;
    ///
    /// let white = DecodedImage::from_rgba(2, 1, vec![255; 8]).unwrap();
    /// assert_eq!(white.pixel(1, 0), [255, 255, 255, 255]);
    /// assert!(DecodedImage::from_rgba(2, 2, vec![0; 3]).is_err());
    /// ```
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(VisionError::InvalidInput(format!(
                "a {width}x{height} image has no pixels"
            )));
        }
        let expected = width as usize * height as usize * 4;
        let actual = rgba.len();
        let pixels = RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
            VisionError::InvalidInput(format!(
                "a {width}x{height} RGBA image needs {expected} bytes, got {actual}"
            ))
        })?;
        Ok(Self { pixels })
    }

    /// A `width × height` image filled with one colour.
    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        Self { pixels: RgbaImage::from_pixel(width.max(1), height.max(1), image::Rgba(rgba)) }
    }

    pub(crate) fn from_inner(pixels: RgbaImage) -> Self {
        Self { pixels }
    }

    pub(crate) fn inner(&self) -> &RgbaImage {
        &self.pixels
    }

    pub(crate) fn inner_mut(&mut self) -> &mut RgbaImage {
        &mut self.pixels
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.pixels.width()
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.pixels.height()
    }

    /// `(width, height)`.
    pub fn size(&self) -> (u32, u32) {
        self.pixels.dimensions()
    }

    /// The row-major RGBA8 bytes.
    pub fn rgba(&self) -> &[u8] {
        self.pixels.as_raw()
    }

    /// One pixel's RGBA value.
    ///
    /// # Panics
    ///
    /// Panics when `(x, y)` is outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        self.pixels.get_pixel(x, y).0
    }

    /// Encodes the pixels as PNG. The same pixels always encode to the same bytes.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Decode`] when the encoder fails.
    pub fn encode_png(&self) -> Result<Vec<u8>> {
        let mut out = Cursor::new(Vec::new());
        self.pixels
            .write_to(&mut out, ImageFormat::Png)
            .map_err(|e| VisionError::Decode(format!("PNG encoding failed: {e}")))?;
        Ok(out.into_inner())
    }

    /// Decodes an encoded image under `limits`, applying its EXIF orientation once. Returns
    /// the pixels and the MIME type of the format.
    ///
    /// # Errors
    ///
    /// [`VisionError::TooManyBytes`] or [`VisionError::TooManyPixels`] before any pixel
    /// buffer is allocated; [`VisionError::UnsupportedFormat`] for anything other than PNG,
    /// JPEG, WebP or GIF; [`VisionError::Decode`] for a corrupt image.
    pub fn decode(bytes: &[u8], limits: &DecodeLimits) -> Result<(Self, &'static str)> {
        let size = bytes.len() as u64;
        if size > limits.max_bytes {
            return Err(VisionError::TooManyBytes { limit: limits.max_bytes, actual: size });
        }
        let reader = || {
            ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|e| VisionError::Decode(e.to_string()))
        };
        let format = reader()?.format();
        let mime = match format {
            Some(ImageFormat::Png) => "image/png",
            Some(ImageFormat::Jpeg) => "image/jpeg",
            Some(ImageFormat::WebP) => "image/webp",
            Some(ImageFormat::Gif) => "image/gif",
            other => {
                let detected = other.map_or_else(|| "unknown".to_string(), |f| format!("{f:?}"));
                return Err(VisionError::UnsupportedFormat { detected });
            }
        };
        let (width, height) =
            reader()?.into_dimensions().map_err(|e| VisionError::Decode(e.to_string()))?;
        let pixels = u64::from(width) * u64::from(height);
        if pixels > limits.max_pixels {
            return Err(VisionError::TooManyPixels { limit: limits.max_pixels, actual: pixels });
        }
        if pixels == 0 {
            return Err(VisionError::Decode("the image declares no pixels".into()));
        }
        let mut reader = reader()?;
        let mut caps = image::Limits::default();
        caps.max_image_width = Some(width);
        caps.max_image_height = Some(height);
        caps.max_alloc = Some(pixels.saturating_mul(8).saturating_add(1 << 20));
        reader.limits(caps);
        let mut decoder = reader.into_decoder().map_err(|e| VisionError::Decode(e.to_string()))?;
        let orientation =
            decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut decoded =
            DynamicImage::from_decoder(decoder).map_err(|e| VisionError::Decode(e.to_string()))?;
        decoded.apply_orientation(orientation);
        Ok((Self { pixels: decoded.into_rgba8() }, mime))
    }
}

/// A source image on the device: its pixels and the digest of the bytes they came from.
///
/// Source images never leave the machine as such; a backend sees one only through the
/// runtime, as a `SentImage` or, for a remote backend, a `ReleasedImage`.
///
/// # Example
///
/// ```
/// use adk_vision::{DecodeLimits, DecodedImage, SourceImage};
///
/// let png = DecodedImage::filled(4, 3, [0, 0, 0, 255]).encode_png().unwrap();
/// let source = SourceImage::decode(&png, &DecodeLimits::default()).unwrap();
/// assert_eq!(source.size(), (4, 3));
/// assert_eq!(source.mime_type(), "image/png");
/// ```
#[derive(Clone, Debug)]
pub struct SourceImage {
    digest: Digest,
    mime_type: String,
    pixels: Arc<DecodedImage>,
}

impl SourceImage {
    /// Decodes encoded bytes; the digest is the bytes' SHA-256.
    ///
    /// # Errors
    ///
    /// As [`DecodedImage::decode`].
    pub fn decode(bytes: &[u8], limits: &DecodeLimits) -> Result<Self> {
        let (pixels, mime) = DecodedImage::decode(bytes, limits)?;
        Ok(Self {
            digest: Digest::of(bytes),
            mime_type: mime.to_string(),
            pixels: Arc::new(pixels),
        })
    }

    /// A source image from pixels; the digest is that of their PNG encoding.
    ///
    /// # Errors
    ///
    /// As [`DecodedImage::encode_png`].
    pub fn from_pixels(pixels: DecodedImage) -> Result<Self> {
        let png = pixels.encode_png()?;
        Ok(Self {
            digest: Digest::of(&png),
            mime_type: "image/png".into(),
            pixels: Arc::new(pixels),
        })
    }

    pub(crate) fn from_parts(digest: Digest, mime_type: String, pixels: Arc<DecodedImage>) -> Self {
        Self { digest, mime_type, pixels }
    }

    /// The digest of the image's encoded bytes.
    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// The MIME type of the encoded bytes.
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// The pixels.
    pub fn pixels(&self) -> &DecodedImage {
        &self.pixels
    }

    pub(crate) fn shared_pixels(&self) -> Arc<DecodedImage> {
        Arc::clone(&self.pixels)
    }

    /// `(width, height)`.
    pub fn size(&self) -> (u32, u32) {
        self.pixels.size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_refuse_before_decoding() {
        let png = DecodedImage::filled(100, 100, [1, 2, 3, 255]).encode_png().unwrap();
        let err = DecodedImage::decode(&png, &DecodeLimits::default().with_max_pixels(9_999))
            .unwrap_err();
        assert!(matches!(err, VisionError::TooManyPixels { limit: 9_999, actual: 10_000 }));
        let err =
            DecodedImage::decode(&png, &DecodeLimits::default().with_max_bytes(10)).unwrap_err();
        assert!(matches!(err, VisionError::TooManyBytes { limit: 10, .. }));
    }

    #[test]
    fn unknown_formats_are_refused() {
        let err = DecodedImage::decode(b"GIF89 is not this", &DecodeLimits::default()).unwrap_err();
        assert!(matches!(err, VisionError::UnsupportedFormat { .. } | VisionError::Decode(_)));
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&[0; 60]);
        let err = DecodedImage::decode(&bmp, &DecodeLimits::default()).unwrap_err();
        assert!(matches!(err, VisionError::UnsupportedFormat { .. }), "{err}");
    }

    #[test]
    fn png_round_trips_and_encoding_is_stable() {
        let image = DecodedImage::filled(3, 2, [10, 20, 30, 255]);
        let a = image.encode_png().unwrap();
        let (decoded, mime) = DecodedImage::decode(&a, &DecodeLimits::default()).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(decoded, image);
        assert_eq!(decoded.encode_png().unwrap(), a);
    }
}
