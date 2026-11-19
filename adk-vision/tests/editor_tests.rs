//! The deterministic editor: crop, resize, redact, blur, pixelate and overlays, with
//! coordinates in source pixels mapped through the image's chain (spec 3.7 §7.5).

use adk_vision::{
    BBox, DecodedImage, Digest, EditOp, Mask, Overlay, Polygon, Rgba, Transform, TransformChain,
    VisionError, apply_edit,
};

fn checker(width: u32, height: u32) -> DecodedImage {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let v = if (x / 4 + y / 4) % 2 == 0 { 230 } else { 20 };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    DecodedImage::from_rgba(width, height, rgba).unwrap()
}

fn identity(image: &DecodedImage) -> TransformChain {
    TransformChain::new(image.width(), image.height())
}

#[test]
fn crop_keeps_the_box_and_resize_scales() {
    let image = checker(64, 48);
    let cropped = apply_edit(
        &image,
        &identity(&image),
        &EditOp::Crop { bbox: BBox::new(8.0, 4.0, 40.0, 36.0) },
    )
    .unwrap();
    assert_eq!(cropped.size(), (32, 32));
    assert_eq!(cropped.pixel(0, 0), image.pixel(8, 4));
    let resized =
        apply_edit(&image, &identity(&image), &EditOp::Resize { width: 16, height: 12 }).unwrap();
    assert_eq!(resized.size(), (16, 12));
    let err = apply_edit(
        &image,
        &identity(&image),
        &EditOp::Crop { bbox: BBox::new(100.0, 100.0, 120.0, 120.0) },
    )
    .unwrap_err();
    assert!(matches!(err, VisionError::InvalidInput(_)), "{err}");
}

#[test]
fn redaction_fills_solid_and_leaves_the_rest() {
    let image = checker(64, 48);
    let op =
        EditOp::Redact { regions: vec![BBox::new(10.5, 10.5, 20.2, 20.2)], color: Rgba::BLACK };
    let out = apply_edit(&image, &identity(&image), &op).unwrap();
    // Every pixel the box touches is filled, so a fractional edge never leaves a sliver.
    for y in 10..21 {
        for x in 10..21 {
            assert_eq!(out.pixel(x, y), [0, 0, 0, 255], "({x}, {y})");
        }
    }
    assert_eq!(out.pixel(30, 30), image.pixel(30, 30));
    assert_eq!(out.pixel(9, 9), image.pixel(9, 9));
}

#[test]
fn blur_and_pixelate_change_only_their_regions() {
    let image = checker(64, 48);
    let region = BBox::new(0.0, 0.0, 32.0, 24.0);
    for op in [
        EditOp::Blur { regions: vec![region], sigma: 3.0 },
        EditOp::Pixelate { regions: vec![region], block: 8 },
    ] {
        let out = apply_edit(&image, &identity(&image), &op).unwrap();
        assert_ne!(out.pixel(5, 5), image.pixel(5, 5), "{op:?} left the region unchanged");
        assert_eq!(out.pixel(50, 40), image.pixel(50, 40), "{op:?} touched outside the region");
    }
    // Pixelation averages each block: the 8x8 block of a 4x4 checker is mid-grey.
    let out = apply_edit(
        &image,
        &identity(&image),
        &EditOp::Pixelate { regions: vec![region], block: 8 },
    )
    .unwrap();
    assert_eq!(out.pixel(0, 0), [125, 125, 125, 255]);
    assert_eq!(out.pixel(0, 0), out.pixel(7, 7));
}

#[test]
fn edits_are_deterministic() {
    let image = checker(64, 48);
    let op = EditOp::Annotate {
        overlays: vec![
            Overlay::Box { bbox: BBox::new(4.0, 4.0, 30.0, 20.0), color: Rgba::RED, thickness: 2 },
            Overlay::Mark { at: [40.0, 30.0], number: 12, color: Rgba::BLUE },
        ],
    };
    let a = apply_edit(&image, &identity(&image), &op).unwrap();
    let b = apply_edit(&image, &identity(&image), &op).unwrap();
    assert_eq!(Digest::of(&a.encode_png().unwrap()), Digest::of(&b.encode_png().unwrap()));
    assert_eq!(op.digest(), op.clone().digest());
}

#[test]
fn overlays_draw_where_their_source_coordinates_land() {
    let image = DecodedImage::filled(200, 100, [255, 255, 255, 255]);
    // The editor sees a half-size copy; overlay coordinates are source pixels.
    let chain =
        TransformChain::new(400, 200).then(Transform::Resize { width: 200, height: 100 }).unwrap();
    let op = EditOp::Annotate {
        overlays: vec![
            Overlay::Box {
                bbox: BBox::new(20.0, 20.0, 120.0, 80.0),
                color: Rgba::RED,
                thickness: 1,
            },
            Overlay::Point { at: [300.0, 100.0], color: Rgba::GREEN, radius: 3 },
            Overlay::Polygon {
                polygon: Polygon { points: vec![[200.0, 160.0], [380.0, 160.0], [380.0, 190.0]] },
                color: Rgba::BLUE,
                thickness: 1,
            },
            Overlay::Mask {
                mask: Mask::from_box(400, 200, &BBox::new(0.0, 160.0, 40.0, 200.0)),
                color: Rgba([255, 0, 0, 128]),
            },
        ],
    };
    let out = apply_edit(&image, &chain, &op).unwrap();
    assert_eq!(out.pixel(10, 10), Rgba::RED.0, "box corner at source (20, 20) is (10, 10)");
    assert_eq!(out.pixel(30, 25), [255, 255, 255, 255], "a box overlay is an outline");
    assert_eq!(out.pixel(150, 50), Rgba::GREEN.0, "point at source (300, 100) is (150, 50)");
    assert_eq!(out.pixel(150, 80), Rgba::BLUE.0, "polygon edge at y = 160 is y = 80");
    let tinted = out.pixel(5, 90);
    assert_eq!(tinted, [255, 127, 127, 255], "a half-transparent red tint over white");
}

#[test]
fn a_mark_draws_its_number() {
    let image = DecodedImage::filled(80, 80, [255, 255, 255, 255]);
    let mark = |number| {
        let op = EditOp::Annotate {
            overlays: vec![Overlay::Mark { at: [40.0, 40.0], number, color: Rgba::BLUE }],
        };
        apply_edit(&image, &TransformChain::new(80, 80), &op).unwrap()
    };
    let one = mark(1);
    let seven = mark(7);
    // Both badges are blue discs with white digits, and the digits differ.
    assert_eq!(one.pixel(40, 28), Rgba::BLUE.0);
    assert_ne!(Digest::of(&one.encode_png().unwrap()), Digest::of(&seven.encode_png().unwrap()));
    let white = |img: &DecodedImage| {
        let mut n = 0;
        for y in 20..60 {
            for x in 20..60 {
                if img.pixel(x, y) == [255, 255, 255, 255]
                    && ((x as i32 - 40).pow(2) + (y as i32 - 40).pow(2)) < 100
                {
                    n += 1;
                }
            }
        }
        n
    };
    assert!(white(&one) > 0, "the digit is drawn inside the badge");
}

#[test]
fn generative_edits_need_a_model() {
    let image = checker(8, 8);
    let err =
        apply_edit(&image, &identity(&image), &EditOp::Generative { prompt: "add a hat".into() })
            .unwrap_err();
    assert!(matches!(err, VisionError::Unsupported(_)));
}
