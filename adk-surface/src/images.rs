//! Images a person sends with a message (spec 3.3 §5.1): a screenshot of a failing build, a
//! photo of a receipt. They reach the agent as the message's own parts, so a model that sees
//! images reads them with the text.
//!
//! The engine trusts nothing about an image but its bytes: the type a surface declares must
//! match the file's own signature, and only raster formats a browser shows without running
//! anything are taken — PNG, JPEG, WebP and GIF; never SVG.

use base64::Engine as _;

use crate::error::{Result, SurfaceError};
use crate::model::ImageData;

/// The most images one message carries.
pub const MAX_IMAGES: usize = 4;

/// The most bytes one image has, decoded. The web home shrinks larger pictures before it
/// sends them.
pub const MAX_IMAGE_BYTES: usize = 3 * 1024 * 1024;

/// The types the engine takes.
pub const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// An image of a message, decoded and checked: what a [`Source`](crate::Source) sends to the
/// agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageImage {
    /// One of [`IMAGE_TYPES`].
    pub mime_type: String,
    /// The image's bytes.
    pub data: Vec<u8>,
}

/// The type `bytes` begin like, among [`IMAGE_TYPES`].
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

/// Decodes and checks a message's images: at most [`MAX_IMAGES`], each base64 of at most
/// [`MAX_IMAGE_BYTES`] whose bytes are the type it declares.
///
/// # Errors
///
/// `surface.invalid` naming the first image that fails, and why.
///
/// # Example
///
/// ```rust
/// use adk_surface::images::decode;
/// use adk_surface::model::ImageData;
///
/// let png = ImageData { mime_type: "image/png".into(), data: "iVBORw0KGgoAAAANSUhEUg==".into() };
/// assert_eq!(decode(&[png]).unwrap()[0].mime_type, "image/png");
///
/// let svg = ImageData { mime_type: "image/svg+xml".into(), data: "PHN2Zz4=".into() };
/// assert!(decode(&[svg]).is_err());
/// ```
pub fn decode(images: &[ImageData]) -> Result<Vec<MessageImage>> {
    if images.len() > MAX_IMAGES {
        return Err(SurfaceError::Invalid(format!(
            "a message carries at most {MAX_IMAGES} images; this one has {}",
            images.len()
        )));
    }
    images
        .iter()
        .enumerate()
        .map(|(index, image)| {
            let n = index + 1;
            if !IMAGE_TYPES.contains(&image.mime_type.as_str()) {
                return Err(SurfaceError::Invalid(format!(
                    "image {n} is {}: send PNG, JPEG, WebP or GIF",
                    image.mime_type
                )));
            }
            // Four base64 characters make three bytes: refuse an oversized one before decoding.
            if image.data.len() > MAX_IMAGE_BYTES / 3 * 4 + 4 {
                return Err(too_large(n));
            }
            let data = base64::engine::general_purpose::STANDARD
                .decode(image.data.as_bytes())
                .map_err(|_| SurfaceError::Invalid(format!("image {n} is not base64")))?;
            if data.len() > MAX_IMAGE_BYTES {
                return Err(too_large(n));
            }
            match sniff(&data) {
                Some(found) if found == image.mime_type => {
                    Ok(MessageImage { mime_type: image.mime_type.clone(), data })
                }
                Some(found) => Err(SurfaceError::Invalid(format!(
                    "image {n} says {} but is {found}",
                    image.mime_type
                ))),
                None => Err(SurfaceError::Invalid(format!(
                    "image {n} is not a PNG, JPEG, WebP or GIF file"
                ))),
            }
        })
        .collect()
}

fn too_large(n: usize) -> SurfaceError {
    SurfaceError::Invalid(format!(
        "image {n} is larger than {} MB; send a smaller one",
        MAX_IMAGE_BYTES / (1024 * 1024)
    ))
}

/// The images of a message as the thread shows them: base64 again, for the page.
pub(crate) fn shown(mime_type: &str, data: &[u8]) -> Option<ImageData> {
    (IMAGE_TYPES.contains(&mime_type)
        && data.len() <= MAX_IMAGE_BYTES
        && sniff(data) == Some(mime_type))
    .then(|| ImageData {
        mime_type: mime_type.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(data),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(mime_type: &str, bytes: &[u8]) -> ImageData {
        ImageData {
            mime_type: mime_type.into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    #[test]
    fn takes_each_raster_type_by_its_signature() {
        let webp = [b"RIFF".as_slice(), &[0, 0, 0, 0], b"WEBPVP8 "].concat();
        for (mime, bytes) in [
            ("image/png", b"\x89PNG\r\n\x1a\nrest".to_vec()),
            ("image/jpeg", vec![0xFF, 0xD8, 0xFF, 0xE0]),
            ("image/webp", webp),
            ("image/gif", b"GIF89a....".to_vec()),
        ] {
            let decoded = decode(&[image(mime, &bytes)]).unwrap();
            assert_eq!(decoded, vec![MessageImage { mime_type: mime.into(), data: bytes }]);
        }
    }

    #[test]
    fn refuses_what_is_not_what_it_says() {
        let png = b"\x89PNG\r\n\x1a\n";
        let refused = |images: &[ImageData]| decode(images).unwrap_err().to_string();
        assert!(refused(&[image("image/jpeg", png)]).contains("says image/jpeg but is image/png"));
        assert!(refused(&[image("image/svg+xml", b"<svg onload=alert(1)>")]).contains("PNG, JPEG"));
        assert!(refused(&[image("image/png", b"<html>")]).contains("not a PNG"));
        let bad = ImageData { mime_type: "image/png".into(), data: "%%%".into() };
        assert!(refused(&[bad]).contains("not base64"));
        assert!(refused(&vec![image("image/png", png); MAX_IMAGES + 1]).contains("at most 4"));
        let big = [png.as_slice(), &vec![0; MAX_IMAGE_BYTES]].concat();
        assert!(refused(&[image("image/png", &big)]).contains("larger than 3 MB"));
    }

    #[test]
    fn shows_only_what_it_would_take() {
        assert!(shown("image/png", b"\x89PNG\r\n\x1a\n").is_some());
        assert!(shown("image/svg+xml", b"<svg/>").is_none());
        assert!(shown("image/png", b"GIF89a").is_none());
    }
}
