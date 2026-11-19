//! Media: decoding under limits, EXIF orientation applied once, the media store by digest,
//! and image parts that carry their digest (spec 3.7 §7.2).

use std::io::Cursor;
use std::sync::Arc;

use adk_artifact::{ArtifactService, InMemoryArtifactService, ListRequest};
use adk_vision::{
    DecodeLimits, DecodedImage, Digest, MediaScope, MediaStore, Provenance, SourceImage,
    VisionError,
};
use image::{ImageFormat, RgbaImage};

fn encode(image: &RgbaImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    match format {
        ImageFormat::Jpeg => image::DynamicImage::ImageRgba8(image.clone())
            .into_rgb8()
            .write_to(&mut out, format)
            .unwrap(),
        _ => image.write_to(&mut out, format).unwrap(),
    }
    out.into_inner()
}

/// A 40x20 image: red on the left quarter, white elsewhere.
fn landscape() -> RgbaImage {
    RgbaImage::from_fn(40, 20, |x, _| {
        if x < 10 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([255; 4]) }
    })
}

/// Inserts an EXIF APP1 segment with one orientation tag after the JPEG's SOI marker.
fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
    let mut tiff = b"MM\x00\x2a\x00\x00\x00\x08".to_vec();
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&0x0112u16.to_be_bytes());
    tiff.extend_from_slice(&3u16.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes());
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_be_bytes());
    let mut payload = b"Exif\x00\x00".to_vec();
    payload.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&jpeg[2..]);
    out
}

#[test]
fn exif_orientation_is_applied_once() {
    let jpeg = with_exif_orientation(&encode(&landscape(), ImageFormat::Jpeg), 6);
    let (pixels, mime) = DecodedImage::decode(&jpeg, &DecodeLimits::default()).unwrap();
    assert_eq!(mime, "image/jpeg");
    // Orientation 6 is a quarter turn clockwise: 40x20 becomes 20x40, the red left edge the top.
    assert_eq!(pixels.size(), (20, 40));
    let top = pixels.pixel(10, 2);
    let bottom = pixels.pixel(10, 37);
    assert!(top[0] > 200 && top[1] < 60, "top {top:?}");
    assert!(bottom[1] > 200, "bottom {bottom:?}");
}

#[test]
fn png_jpeg_webp_and_gif_decode_and_other_formats_do_not() {
    let image = landscape();
    for (format, mime) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::WebP, "image/webp"),
        (ImageFormat::Gif, "image/gif"),
    ] {
        let (decoded, got) =
            DecodedImage::decode(&encode(&image, format), &DecodeLimits::default()).unwrap();
        assert_eq!((decoded.size(), got), ((40, 20), mime));
    }
    let tiff = b"II*\x00\x08\x00\x00\x00".to_vec();
    let err = DecodedImage::decode(&tiff, &DecodeLimits::default()).unwrap_err();
    assert!(matches!(err, VisionError::UnsupportedFormat { .. }), "{err}");
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[test]
fn the_pixel_cap_is_read_from_the_header() {
    // A PNG header that declares 100,000 x 100,000 pixels and holds no pixel data.
    let mut png = encode(&RgbaImage::new(1, 1), ImageFormat::Png);
    png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    png[20..24].copy_from_slice(&100_000u32.to_be_bytes());
    let crc = crc32(&png[12..29]);
    png[29..33].copy_from_slice(&crc.to_be_bytes());
    let err = DecodedImage::decode(&png, &DecodeLimits::default()).unwrap_err();
    assert!(matches!(err, VisionError::TooManyPixels { actual: 10_000_000_000, .. }), "{err}");
}

#[tokio::test]
async fn the_store_keeps_one_artifact_per_digest_and_checks_it_on_load() {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts.clone());
    let scope = MediaScope::new("desk", "u1", "s1");
    let png = encode(&landscape(), ImageFormat::Png);
    let (first, source) = store.put_image(&scope, png.clone(), Provenance::Upload).await.unwrap();
    let (second, _) = store
        .put_image(
            &scope,
            png.clone(),
            Provenance::Screen { display: "built-in".into(), device_pixel_ratio_milli: 2000 },
        )
        .await
        .unwrap();
    assert_eq!(first.artifact, second.artifact);
    assert_eq!(first.digest, Digest::of(&png));
    assert_eq!((first.width, first.height), (40, 20));
    assert_eq!(source.digest(), first.digest);
    assert_eq!(store.load_bytes(&first).await.unwrap(), png);

    let mut forged = first.clone();
    forged.digest = Digest::of(b"something else");
    let err = store.load(&forged).await.unwrap_err();
    assert!(matches!(err, VisionError::DigestMismatch { .. }), "{err}");

    let listed = artifacts
        .list(ListRequest {
            app_name: "desk".into(),
            user_id: "u1".into(),
            session_id: "s1".into(),
        })
        .await
        .unwrap();
    assert_eq!(listed.file_names, vec![format!("vision-{}.png", first.digest.hex())]);
}

#[tokio::test]
async fn refused_images_are_never_stored() {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts.clone())
        .with_limits(DecodeLimits::default().with_max_pixels(100));
    let scope = MediaScope::new("desk", "u1", "s1");
    let err = store
        .put_image(&scope, encode(&landscape(), ImageFormat::Png), Provenance::Upload)
        .await
        .unwrap_err();
    assert!(matches!(err, VisionError::TooManyPixels { limit: 100, actual: 800 }), "{err}");
    let listed = artifacts
        .list(ListRequest {
            app_name: "desk".into(),
            user_id: "u1".into(),
            session_id: "s1".into(),
        })
        .await
        .unwrap();
    assert!(listed.file_names.is_empty());
}

#[tokio::test]
async fn an_image_part_carries_its_digest_and_artifact() {
    let store = MediaStore::new(Arc::new(InMemoryArtifactService::new()));
    let scope = MediaScope::new("desk", "u1", "s1");
    let (image, _) = store
        .put_pixels(&scope, DecodedImage::filled(4, 4, [9, 9, 9, 255]), Provenance::Upload)
        .await
        .unwrap();
    let bytes = store.load_bytes(&image).await.unwrap();
    let adk_core::Part::InlineData { mime_type, uri, annotations, .. } = image.to_part(bytes)
    else {
        panic!()
    };
    assert_eq!(mime_type, "image/png");
    assert_eq!(uri.unwrap(), adk_vision::artifact_uri(&image.artifact));
    let annotations = annotations.unwrap();
    assert_eq!(annotations["adkVision"]["digest"], image.digest.to_string());
    assert!(annotations["adkVision"]["release"].is_null(), "an unreleased image says so");
}

#[test]
fn source_images_from_pixels_are_named_by_their_png() {
    let pixels = DecodedImage::filled(3, 3, [1, 2, 3, 255]);
    let png = pixels.encode_png().unwrap();
    let source = SourceImage::from_pixels(pixels).unwrap();
    assert_eq!(source.digest(), Digest::of(&png));
}
