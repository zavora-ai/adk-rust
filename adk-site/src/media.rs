//! The media pipeline (spec 3.5 §7.8.2 "Media", A-I1).
//!
//! Every upload is type-sniffed; executables and polyglot files are refused; raster images
//! are decoded and re-encoded, which drops every metadata block (location included), and
//! resized to the policy's limits; SVG is refused when it carries script and loses its
//! `<metadata>`; other media is refused when it carries location metadata. Alt text is
//! drafted by the caller and must be confirmed before the launch gate's accessibility check
//! passes; generated images are labelled as illustrations.

use std::io::Cursor;

use bytes::Bytes;
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use awp_types::{Digest, MediaRef};

use crate::error::{Result, SiteError};
use crate::scan::{Sniffed, scan_file, sniff};

/// The format raster images are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImageOutput {
    /// The format the upload arrived in.
    #[default]
    Keep,
    /// JPEG at the policy's quality.
    Jpeg,
    /// PNG.
    Png,
    /// Lossless WebP.
    Webp,
}

/// What the media pipeline accepts and how it converts.
///
/// # Example
///
/// ```rust
/// use adk_site::media::MediaPolicy;
///
/// let policy = MediaPolicy { max_dimension: 1600, widths: vec![480, 960], ..MediaPolicy::default() };
/// assert_eq!(policy.jpeg_quality, 85);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaPolicy {
    /// Largest accepted upload in bytes.
    pub max_bytes: u64,
    /// Longest side of a stored image in pixels; larger images are scaled down.
    pub max_dimension: u32,
    /// Extra widths to produce for responsive images, smaller than the original only.
    pub widths: Vec<u32>,
    /// The output format of raster images.
    pub output: ImageOutput,
    /// JPEG quality, 1 to 100.
    pub jpeg_quality: u8,
    /// Accepted formats.
    pub accepted: Vec<Sniffed>,
}

impl Default for MediaPolicy {
    fn default() -> Self {
        Self {
            max_bytes: 25 * 1024 * 1024,
            max_dimension: 2400,
            widths: Vec::new(),
            output: ImageOutput::Keep,
            jpeg_quality: 85,
            accepted: vec![
                Sniffed::Png,
                Sniffed::Jpeg,
                Sniffed::Gif,
                Sniffed::Webp,
                Sniffed::Svg,
                Sniffed::Pdf,
                Sniffed::Mp4,
                Sniffed::Webm,
                Sniffed::Mp3,
                Sniffed::Ogg,
            ],
        }
    }
}

/// The state of an image's text alternative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "text")]
pub enum AltText {
    /// No alt text yet: the accessibility check fails until one is confirmed.
    Missing,
    /// Drafted (by an agent or an import) and waiting for a person to confirm it.
    Drafted(String),
    /// Confirmed by a person.
    Confirmed(String),
    /// Confirmed as decorative: rendered with `alt=""`.
    Decorative,
}

impl AltText {
    /// Whether the launch gate accepts it.
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed(_) | Self::Decorative)
    }
}

/// One upload into the media pipeline.
#[derive(Debug, Clone)]
pub struct MediaUpload<'a> {
    /// The uploaded file name.
    pub name: &'a str,
    /// The bytes.
    pub bytes: &'a [u8],
    /// Drafted alt text, if any.
    pub alt: Option<String>,
    /// Whether a model generated the image; it is then labelled as an illustration.
    pub generated: bool,
}

/// A smaller copy of an image for responsive `srcset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaVariant {
    /// Width in pixels.
    pub width: u32,
    /// File name.
    pub name: String,
    /// The encoded image.
    pub bytes: Bytes,
}

/// A media file ready to sit beside its content item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedMedia {
    /// A safe file name with the sniffed format's extension.
    pub name: String,
    /// The sniffed media type.
    pub content_type: &'static str,
    /// The stored bytes.
    pub bytes: Bytes,
    /// The digest of `bytes`.
    pub sha256: Digest,
    /// Width in pixels, for images.
    pub width: Option<u32>,
    /// Height in pixels, for images.
    pub height: Option<u32>,
    /// Responsive copies.
    pub variants: Vec<MediaVariant>,
    /// Whether metadata blocks were removed (re-encoded images, SVG `<metadata>`).
    pub metadata_stripped: bool,
    /// The alt text state; images start `Missing` or `Drafted`.
    pub alt: AltText,
    /// Whether the file is a generated illustration.
    pub illustration: bool,
}

fn refuse(name: &str, reason: impl Into<String>) -> SiteError {
    SiteError::MediaRefused { name: name.to_string(), reason: reason.into() }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len())
        .find(|&i| haystack[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// Markers of a second format hidden inside a file: archives, documents and markup that a
/// browser or another tool would act on.
const POLYGLOT_MARKERS: &[(&[u8], &str)] = &[
    (b"PK\x03\x04", "a ZIP archive"),
    (b"Rar!\x1a\x07", "a RAR archive"),
    (b"7z\xbc\xaf\x27\x1c", "a 7-Zip archive"),
    (b"%PDF-", "a PDF document"),
    (b"<script", "an HTML script"),
    (b"<html", "an HTML document"),
    (b"<?php", "a PHP script"),
];

/// Location metadata markers in formats the pipeline cannot rewrite.
const LOCATION_MARKERS: &[&[u8]] = &[b"GPSLatitude", b"exif:GPS", b"\xa9xyz", b"geo:lat"];

fn safe_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = base.split('.').next().unwrap_or(base);
    let mut out = String::new();
    for c in stem.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "media".to_string() } else { out }
}

fn extension(format: Sniffed) -> &'static str {
    match format {
        Sniffed::Png => "png",
        Sniffed::Jpeg => "jpg",
        Sniffed::Gif => "gif",
        Sniffed::Webp => "webp",
        Sniffed::Avif => "avif",
        Sniffed::Ico => "ico",
        Sniffed::Pdf => "pdf",
        Sniffed::Zip => "zip",
        Sniffed::Gzip => "gz",
        Sniffed::Mp4 => "mp4",
        Sniffed::Webm => "webm",
        Sniffed::Mp3 => "mp3",
        Sniffed::Ogg => "ogg",
        Sniffed::Woff => "woff2",
        Sniffed::Wasm => "wasm",
        Sniffed::Svg => "svg",
        Sniffed::Html => "html",
        Sniffed::Elf | Sniffed::Pe | Sniffed::MachO | Sniffed::JavaClass | Sniffed::Script => "bin",
    }
}

impl ProcessedMedia {
    /// The content item's reference to this file once it is stored in `dir` (a repository
    /// directory such as `content/products/panel-400`). Drafted alt text is carried but not
    /// confirmed, so the launch gate holds the item until a person confirms it.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use adk_site::media::{AltText, MediaPipeline, MediaUpload};
    /// # let png = {
    /// #     let image = image::DynamicImage::new_rgb8(2, 2);
    /// #     let mut out = std::io::Cursor::new(Vec::new());
    /// #     image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    /// #     out.into_inner()
    /// # };
    /// let processed = MediaPipeline::default()
    ///     .process(&MediaUpload { name: "Roof.png", bytes: &png, alt: Some("A roof".into()), generated: false })
    ///     .unwrap();
    /// let media = processed.media_ref("content/posts/hello");
    /// assert_eq!(media.path, "content/posts/hello/roof.png");
    /// assert!(!media.alt_confirmed);
    /// ```
    pub fn media_ref(&self, dir: &str) -> MediaRef {
        let (alt, alt_confirmed) = match &self.alt {
            AltText::Missing => (None, false),
            AltText::Drafted(text) => (Some(text.clone()), false),
            AltText::Confirmed(text) => (Some(text.clone()), true),
            AltText::Decorative => (Some(String::new()), true),
        };
        MediaRef {
            path: format!("{}/{}", dir.trim_end_matches('/'), self.name),
            content_type: self.content_type.to_string(),
            sha256: Some(self.sha256.clone()),
            alt,
            alt_confirmed,
            generated: self.illustration,
        }
    }
}

/// Runs uploads through the media rules.
///
/// # Example
///
/// ```rust
/// use adk_site::media::{AltText, MediaPipeline, MediaUpload};
///
/// let pipeline = MediaPipeline::default();
/// let refused = pipeline.process(&MediaUpload {
///     name: "photo.jpg",
///     bytes: b"\x7fELF\x02\x01\x01",
///     alt: None,
///     generated: false,
/// });
/// assert!(refused.is_err());
/// ```
#[derive(Debug, Clone, Default)]
pub struct MediaPipeline {
    policy: MediaPolicy,
}

impl MediaPipeline {
    /// A pipeline with `policy`.
    pub fn new(policy: MediaPolicy) -> Self {
        Self { policy }
    }

    /// The policy.
    pub fn policy(&self) -> &MediaPolicy {
        &self.policy
    }

    /// Checks and converts one upload.
    ///
    /// # Errors
    ///
    /// [`SiteError::MediaRefused`] when the upload is too large, of an unaccepted or
    /// unrecognised type, executable, a polyglot, an SVG with script, undecodable, or
    /// carries location metadata the pipeline cannot remove.
    pub fn process(&self, upload: &MediaUpload<'_>) -> Result<ProcessedMedia> {
        let name = upload.name;
        let bytes = upload.bytes;
        if bytes.len() as u64 > self.policy.max_bytes {
            return Err(refuse(
                name,
                format!("it is {} bytes; the limit is {}", bytes.len(), self.policy.max_bytes),
            ));
        }
        if let Some(finding) = scan_file(name, bytes).into_iter().next() {
            return Err(refuse(name, format!("server code (A-I1): {:?}", finding.kind)));
        }
        let format = sniff(bytes).ok_or_else(|| refuse(name, "its type is not recognised"))?;
        if format.is_executable() {
            return Err(refuse(name, "it is an executable"));
        }
        if !self.policy.accepted.contains(&format) {
            return Err(refuse(name, format!("{} files are not accepted", format.content_type())));
        }
        let stem = safe_stem(name);
        let mut processed = ProcessedMedia {
            name: format!("{stem}.{}", extension(format)),
            content_type: format.content_type(),
            bytes: Bytes::copy_from_slice(bytes),
            sha256: Digest::from_sha256_bytes([0; 32]),
            width: None,
            height: None,
            variants: Vec::new(),
            metadata_stripped: false,
            alt: AltText::Missing,
            illustration: upload.generated,
        };
        match format {
            Sniffed::Svg => {
                let text =
                    std::str::from_utf8(bytes).map_err(|_| refuse(name, "the SVG is not UTF-8"))?;
                let lower = text.to_ascii_lowercase();
                let handler = lower.match_indices(" on").any(|(at, _)| {
                    lower[at + 3..].chars().take_while(|c| c.is_ascii_alphabetic()).count() > 0
                        && lower[at + 3..]
                            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
                            .trim_start()
                            .starts_with('=')
                });
                if lower.contains("<script")
                    || lower.contains("javascript:")
                    || lower.contains("<foreignobject")
                    || handler
                {
                    return Err(refuse(name, "the SVG carries script"));
                }
                let stripped = strip_svg_metadata(text);
                processed.metadata_stripped = stripped.len() != text.len();
                processed.bytes = Bytes::from(stripped);
                processed.alt = upload.alt.clone().map_or(AltText::Missing, AltText::Drafted);
            }
            raster if raster.is_raster_image() => {
                self.check_polyglot(name, bytes, raster)?;
                let decoded = image::load_from_memory_with_format(bytes, image_format(raster))
                    .map_err(|e| refuse(name, format!("the image does not decode: {e}")))?;
                let scaled = fit(decoded, self.policy.max_dimension);
                let target = match self.policy.output {
                    ImageOutput::Keep => raster,
                    ImageOutput::Jpeg => Sniffed::Jpeg,
                    ImageOutput::Png => Sniffed::Png,
                    ImageOutput::Webp => Sniffed::Webp,
                };
                processed.bytes = Bytes::from(self.encode(name, &scaled, target)?);
                processed.name = format!("{stem}.{}", extension(target));
                processed.content_type = target.content_type();
                processed.width = Some(scaled.width());
                processed.height = Some(scaled.height());
                processed.metadata_stripped = true;
                processed.alt = upload.alt.clone().map_or(AltText::Missing, AltText::Drafted);
                let mut widths = self.policy.widths.clone();
                widths.sort_unstable();
                widths.dedup();
                for width in widths.into_iter().filter(|w| *w < scaled.width()) {
                    let variant = scaled.resize(width, u32::MAX, FilterType::Lanczos3);
                    processed.variants.push(MediaVariant {
                        width,
                        name: format!("{stem}-{width}w.{}", extension(target)),
                        bytes: Bytes::from(self.encode(name, &variant, target)?),
                    });
                }
            }
            other => {
                self.check_polyglot(name, bytes, other)?;
                if LOCATION_MARKERS.iter().any(|marker| find(bytes, marker, 0).is_some()) {
                    return Err(refuse(
                        name,
                        "it carries location metadata; remove it and upload again",
                    ));
                }
            }
        }
        processed.sha256 = Digest::from_sha256_bytes(Sha256::digest(&processed.bytes).into());
        Ok(processed)
    }

    fn check_polyglot(&self, name: &str, bytes: &[u8], format: Sniffed) -> Result<()> {
        for (marker, what) in POLYGLOT_MARKERS {
            // A file may start with its own signature; anything after it is a second format.
            let own = matches!((format, *marker), (Sniffed::Pdf, b"%PDF-"));
            let from = usize::from(own);
            if let Some(at) = find(bytes, marker, from)
                && !(own && at == 0)
            {
                return Err(refuse(
                    name,
                    format!("it also contains {what} at byte {at} (a polyglot file)"),
                ));
            }
        }
        Ok(())
    }

    fn encode(&self, name: &str, image: &DynamicImage, format: Sniffed) -> Result<Vec<u8>> {
        let mut out = Cursor::new(Vec::new());
        let result = match format {
            Sniffed::Jpeg => {
                let rgb = DynamicImage::ImageRgb8(image.to_rgb8());
                let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                    &mut out,
                    self.policy.jpeg_quality.clamp(1, 100),
                );
                rgb.write_with_encoder(encoder)
            }
            Sniffed::Webp => {
                let rgba = DynamicImage::ImageRgba8(image.to_rgba8());
                rgba.write_with_encoder(image::codecs::webp::WebPEncoder::new_lossless(&mut out))
            }
            Sniffed::Gif => image.write_to(&mut out, ImageFormat::Gif),
            _ => image.write_to(&mut out, ImageFormat::Png),
        };
        result.map_err(|e| refuse(name, format!("re-encoding failed: {e}")))?;
        Ok(out.into_inner())
    }
}

fn image_format(format: Sniffed) -> ImageFormat {
    match format {
        Sniffed::Jpeg => ImageFormat::Jpeg,
        Sniffed::Gif => ImageFormat::Gif,
        Sniffed::Webp => ImageFormat::WebP,
        _ => ImageFormat::Png,
    }
}

fn fit(image: DynamicImage, max: u32) -> DynamicImage {
    if image.width() <= max && image.height() <= max {
        image
    } else {
        image.resize(max, max, FilterType::Lanczos3)
    }
}

fn strip_svg_metadata(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(start) = lower.find("<metadata") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        match lower[start..].find("</metadata>") {
            Some(end) => rest = &rest[start + end + "</metadata>".len()..],
            None => match lower[start..].find("/>") {
                Some(end) => rest = &rest[start + end + 2..],
                None => break,
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            width,
            height,
            image::Rgb([200, 30, 30]),
        ));
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// A JPEG with an APP1 Exif block holding a GPS marker.
    fn jpeg_with_gps() -> Vec<u8> {
        let image =
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(16, 16, image::Rgb([1, 2, 3])));
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Jpeg).unwrap();
        let plain = out.into_inner();
        let payload = b"Exif\0\0GPSLatitude 1.2921 S GPSLongitude 36.8219 E";
        let mut app1 = vec![0xff, 0xe1];
        app1.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        app1.extend_from_slice(payload);
        let mut jpeg = plain[..2].to_vec();
        jpeg.extend_from_slice(&app1);
        jpeg.extend_from_slice(&plain[2..]);
        jpeg
    }

    fn upload<'a>(name: &'a str, bytes: &'a [u8]) -> MediaUpload<'a> {
        MediaUpload { name, bytes, alt: None, generated: false }
    }

    #[test]
    fn images_are_re_encoded_without_location_and_resized() {
        let pipeline = MediaPipeline::new(MediaPolicy {
            max_dimension: 8,
            widths: vec![4, 100],
            ..MediaPolicy::default()
        });
        let jpeg = jpeg_with_gps();
        assert!(find(&jpeg, b"GPSLatitude", 0).is_some());
        let out = pipeline.process(&upload("Holiday Photo.JPG", &jpeg)).unwrap();
        assert!(find(&out.bytes, b"GPSLatitude", 0).is_none());
        assert!(out.metadata_stripped);
        assert_eq!((out.width, out.height), (Some(8), Some(8)));
        assert_eq!(out.name, "holiday-photo.jpg");
        assert_eq!(out.variants.len(), 1, "only widths below the original");
        assert_eq!(out.alt, AltText::Missing);
    }

    #[test]
    fn the_sniffed_type_wins_over_the_name() {
        let out = MediaPipeline::default().process(&upload("logo.jpg", &png(3, 3))).unwrap();
        assert_eq!((out.name.as_str(), out.content_type), ("logo.png", "image/png"));
    }

    #[test]
    fn executables_polyglots_and_unknown_types_are_refused() {
        let pipeline = MediaPipeline::default();
        assert!(pipeline.process(&upload("a.png", b"MZ\x90\x00\x03")).is_err());
        assert!(pipeline.process(&upload("a.png", b"#!/bin/sh\n")).is_err());
        assert!(pipeline.process(&upload("notes.bin", b"\x00\x01\x02")).is_err());
        let mut gifar = png(2, 2);
        gifar.extend_from_slice(b"PK\x03\x04\x14\x00rest-of-a-jar");
        let error = pipeline.process(&upload("a.png", &gifar)).unwrap_err().to_string();
        assert!(error.contains("polyglot"), "{error}");
        let mut pdf = b"%PDF-1.7\n".to_vec();
        pdf.extend_from_slice(b"<< /Author (x) /exif:GPSLatitude (1) >>");
        assert!(
            pipeline
                .process(&upload("doc.pdf", &pdf))
                .unwrap_err()
                .to_string()
                .contains("location")
        );
        assert!(pipeline.process(&upload("doc.pdf", b"%PDF-1.7\n%%EOF")).is_ok());
    }

    #[test]
    fn svg_with_script_is_refused_and_metadata_is_dropped() {
        let pipeline = MediaPipeline::default();
        let bad = br#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"></svg>"#;
        assert!(pipeline.process(&upload("x.svg", bad)).is_err());
        let ok = br#"<svg xmlns="http://www.w3.org/2000/svg"><metadata><geo>1,2</geo></metadata><rect/></svg>"#;
        let out = pipeline
            .process(&MediaUpload {
                name: "x.svg",
                bytes: ok,
                alt: Some("A square".into()),
                generated: true,
            })
            .unwrap();
        assert!(!String::from_utf8_lossy(&out.bytes).contains("geo"));
        assert_eq!(out.alt, AltText::Drafted("A square".into()));
        assert!(out.illustration);
    }

    #[test]
    fn processing_is_deterministic() {
        let pipeline =
            MediaPipeline::new(MediaPolicy { output: ImageOutput::Webp, ..MediaPolicy::default() });
        let a = pipeline.process(&upload("a.png", &png(20, 10))).unwrap();
        let b = pipeline.process(&upload("a.png", &png(20, 10))).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.content_type, "image/webp");
    }
}
